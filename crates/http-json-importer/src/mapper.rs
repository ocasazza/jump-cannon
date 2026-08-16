//! Pure projection from decoded JSON documents into the canonical graph.
//!
//! The mapper holds no authority to perform I/O. It reads the package's
//! collection rules and turns the documents the connector fetched into nodes,
//! edges, and discovery documents. Two passes are deliberate: every node
//! collection is materialized before any edge rule runs, so record order
//! never changes the result and an edge can reference a collection declared
//! after it.

use std::collections::{BTreeMap, HashMap, HashSet};

use data_loader::{
    identity::Namespace, DecodedRecord, GraphMapper, ImportError, LoadResult, SearchDocument,
};
use serde_json::Value;
use vault_data::{NodeMeta, VaultEdge, VaultGraph, VaultNode};

use crate::manifest::{
    Collection, Dedupe, EdgeListRules, EdgeRule, FieldRule, MatchOn, NodeRules, Predicate,
    Produces, TitleRule, Transform, ValidatedPackage,
};
use crate::RECORD_COLLECTION_KEY;

/// Projects decoded documents according to one validated package.
pub struct ManifestMapper {
    package: ValidatedPackage,
    namespace: Namespace,
}

/// A node the first pass materialized, indexed so edge rules can resolve
/// against either its local identifier or its title.
struct MappedNode {
    node_id: String,
}

impl ManifestMapper {
    pub fn new(package: ValidatedPackage, namespace: Namespace) -> Self {
        Self { package, namespace }
    }

    /// Documents belonging to `collection`, in fetch order.
    ///
    /// The connector tags every record with its collection; that tag rides
    /// through decoding untouched (see [`data_loader::DecodedRecord`]).
    fn documents<'a>(
        &self,
        records: &'a [DecodedRecord],
        collection: &Collection,
    ) -> Result<Vec<&'a Value>, ImportError> {
        let mut documents = Vec::new();
        for record in records.iter().filter(|record| {
            record
                .metadata
                .get(RECORD_COLLECTION_KEY)
                .and_then(Value::as_str)
                == Some(collection.name.as_str())
        }) {
            let items = record
                .value
                .pointer(&collection.items_pointer)
                .ok_or_else(|| ImportError::Map {
                    message: format!(
                        "collection {:?}: response from {} has nothing at {:?}",
                        collection.name, record.origin, collection.items_pointer
                    ),
                })?;
            let items = items.as_array().ok_or_else(|| ImportError::Map {
                message: format!(
                    "collection {:?}: {:?} in {} is not an array",
                    collection.name, collection.items_pointer, record.origin
                ),
            })?;
            documents.extend(items.iter());
        }
        Ok(documents)
    }
}

impl GraphMapper for ManifestMapper {
    fn map(&self, records: Vec<DecodedRecord>) -> Result<LoadResult, ImportError> {
        let mut graph = VaultGraph::new();
        let mut search_documents = Vec::new();
        let mut unresolved: Vec<String> = Vec::new();
        // collection -> local id -> node, plus a title index for `match_on = title`.
        let mut by_local: HashMap<&str, HashMap<String, MappedNode>> = HashMap::new();
        let mut by_title: HashMap<&str, HashMap<String, String>> = HashMap::new();
        let mut by_title_ci: HashMap<&str, HashMap<String, String>> = HashMap::new();

        // --- pass 1: nodes ----------------------------------------------------
        for collection in self.package.collections() {
            let Produces::Nodes(rules) = &collection.produces else {
                continue;
            };
            let folder = rules.folder.clone().unwrap_or_else(|| collection.name.clone());
            let locals = by_local.entry(collection.name.as_str()).or_default();
            let titles = by_title.entry(collection.name.as_str()).or_default();
            let titles_ci = by_title_ci.entry(collection.name.as_str()).or_default();

            for document in self.documents(&records, collection)? {
                if !predicate_holds(rules.skip_unless.as_ref(), document) {
                    continue;
                }
                let raw_id = pointer_str(document, &rules.id_pointer).ok_or_else(|| {
                    ImportError::Map {
                        message: format!(
                            "collection {:?}: document has no identifier at {:?}",
                            collection.name, rules.id_pointer
                        ),
                    }
                })?;
                let local = format!("{}{raw_id}", rules.local_prefix);
                let node_id = self.namespace.node_id(&local)?;
                let title = title_for(&rules.title, document, &raw_id);
                let tags = tags_for(rules, document);
                let path = format!("{}/{raw_id}", collection.name);

                let mut frontmatter = BTreeMap::new();
                for rule in &rules.fields {
                    if let Some(value) = field_value(rule, document) {
                        frontmatter.insert(rule.key.clone(), value);
                    }
                }

                graph
                    .try_add_node(VaultNode {
                        id: node_id.clone(),
                        meta: NodeMeta {
                            source_id: self.namespace.source_id().to_string(),
                            title: title.clone(),
                            tags: tags.clone(),
                            frontmatter: frontmatter.clone().into_iter().collect(),
                            mtime: 0,
                            path: path.clone(),
                            doctype: None,
                            folder: folder.clone(),
                            content_type: None,
                            content_readable: false,
                            content_writable: false,
                        },
                        ..VaultNode::default()
                    })
                    .map_err(|error| ImportError::Map {
                        message: format!("collection {:?}: {error}", collection.name),
                    })?;

                let mut search = SearchDocument::new(&node_id)
                    .with("id", node_id.clone())
                    .with("title", title.clone())
                    .with("tags", serde_json::json!(tags))
                    .with("path", path)
                    .with("type", rules.node_type.clone())
                    .with("folder", folder.clone());
                for (key, value) in frontmatter {
                    search.insert(key, value);
                }
                search_documents.push(search);

                locals.insert(
                    local,
                    MappedNode {
                        node_id: node_id.clone(),
                    },
                );
                titles.entry(title.clone()).or_insert_with(|| node_id.clone());
                titles_ci
                    .entry(title.to_lowercase())
                    .or_insert_with(|| node_id.clone());
            }
        }

        // --- pass 2: edges ----------------------------------------------------
        // One dedupe set across every edge source, so a package rule cannot
        // duplicate an API-provided link (or vice versa).
        let mut seen: HashSet<(String, String)> = HashSet::new();
        let mut push_edge =
            |graph: &mut VaultGraph, source: String, target: String, dedupe: Dedupe| {
                let key = match dedupe {
                    Dedupe::None => None,
                    Dedupe::Ordered => Some((source.clone(), target.clone())),
                    Dedupe::Unordered => Some(if source <= target {
                        (source.clone(), target.clone())
                    } else {
                        (target.clone(), source.clone())
                    }),
                };
                if let Some(key) = key {
                    if !seen.insert(key) {
                        return;
                    }
                }
                graph.add_edge(VaultEdge { source, target });
            };

        for collection in self.package.collections() {
            match &collection.produces {
                Produces::Nodes(rules) => {
                    if rules.edges.is_empty() {
                        continue;
                    }
                    for document in self.documents(&records, collection)? {
                        if !predicate_holds(rules.skip_unless.as_ref(), document) {
                            continue;
                        }
                        let Some(raw_id) = pointer_str(document, &rules.id_pointer) else {
                            continue;
                        };
                        let local = format!("{}{raw_id}", rules.local_prefix);
                        let Some(source_id) = by_local
                            .get(collection.name.as_str())
                            .and_then(|nodes| nodes.get(&local))
                            .map(|node| node.node_id.clone())
                        else {
                            continue;
                        };
                        for rule in &rules.edges {
                            self.apply_edge_rule(
                                rule,
                                document,
                                &source_id,
                                &raw_id,
                                &by_local,
                                &by_title,
                                &by_title_ci,
                                &mut graph,
                                &mut push_edge,
                                &mut unresolved,
                            );
                        }
                    }
                }
                Produces::Edges(rules) => {
                    for document in self.documents(&records, collection)? {
                        self.apply_edge_list(
                            rules,
                            document,
                            &by_local,
                            &mut graph,
                            &mut push_edge,
                        );
                    }
                }
            }
        }

        Ok(LoadResult {
            graph,
            search_documents,
            unresolved,
        })
    }
}

impl ManifestMapper {
    /// The `local_prefix` a node collection stamps onto its local ids.
    /// Validation guarantees an edge rule's target is a node collection.
    fn local_prefix_of(&self, collection: &str) -> String {
        self.package
            .collections()
            .iter()
            .find(|candidate| candidate.name == collection)
            .and_then(|candidate| match &candidate.produces {
                Produces::Nodes(rules) => Some(rules.local_prefix.clone()),
                Produces::Edges(_) => None,
            })
            .unwrap_or_default()
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_edge_rule(
        &self,
        rule: &EdgeRule,
        document: &Value,
        source_id: &str,
        owner: &str,
        by_local: &HashMap<&str, HashMap<String, MappedNode>>,
        by_title: &HashMap<&str, HashMap<String, String>>,
        by_title_ci: &HashMap<&str, HashMap<String, String>>,
        graph: &mut VaultGraph,
        push_edge: &mut impl FnMut(&mut VaultGraph, String, String, Dedupe),
        unresolved: &mut Vec<String>,
    ) {
        let target_collection = rule.target_collection.as_str();
        for value in rule_values(rule.transform, document, &rule.value_pointer) {
            let resolved = match rule.match_on {
                MatchOn::Id => {
                    // Stored keys are prefixed local ids; the referencing
                    // document carries the bare id, so rebuild the exact key
                    // from the target collection's own prefix. A suffix scan
                    // would resolve `doc-1` against `doc-11`.
                    let key = format!("{}{value}", self.local_prefix_of(target_collection));
                    by_local
                        .get(target_collection)
                        .and_then(|nodes| nodes.get(&key))
                        .map(|node| node.node_id.clone())
                }
                MatchOn::Title => by_title
                    .get(target_collection)
                    .and_then(|titles| titles.get(&value).cloned())
                    .or_else(|| {
                        by_title_ci
                            .get(target_collection)
                            .and_then(|titles| titles.get(&value.to_lowercase()).cloned())
                    }),
            };
            match resolved {
                Some(target) => {
                    push_edge(graph, source_id.to_string(), target, Dedupe::Unordered);
                }
                None => unresolved.push(format!(
                    "{} {value:?} referenced by {owner}",
                    rule.target_collection
                )),
            }
        }
    }

    fn apply_edge_list(
        &self,
        rules: &EdgeListRules,
        document: &Value,
        by_local: &HashMap<&str, HashMap<String, MappedNode>>,
        graph: &mut VaultGraph,
        push_edge: &mut impl FnMut(&mut VaultGraph, String, String, Dedupe),
    ) {
        if let Some(pointer) = &rules.kind_pointer {
            if !rules.include_kinds.is_empty() {
                let kind = pointer_str(document, pointer).unwrap_or_default();
                if !rules.include_kinds.iter().any(|allowed| allowed == &kind) {
                    return;
                }
            }
        }
        let (Some(source), Some(target)) = (
            pointer_str(document, &rules.source_pointer),
            pointer_str(document, &rules.target_pointer),
        ) else {
            return;
        };
        if rules.drop_self_loops && source == target {
            return;
        }
        let endpoints = by_local.get(rules.endpoints_collection.as_str());
        let (Some(source), Some(target)) = (
            endpoints.and_then(|nodes| nodes.get(&source)).map(|node| node.node_id.clone()),
            endpoints.and_then(|nodes| nodes.get(&target)).map(|node| node.node_id.clone()),
        ) else {
            // An endpoint outside the imported set (a filtered-out document):
            // the link has nothing to attach to.
            return;
        };
        push_edge(graph, source, target, rules.dedupe);
    }
}

/// Read a pointer as a trimmed, non-empty string. Numbers and booleans render
/// through their JSON form so an id may be numeric.
fn pointer_str(document: &Value, pointer: &str) -> Option<String> {
    let value = document.pointer(pointer)?;
    let text = match value {
        Value::String(text) => text.trim().to_string(),
        Value::Number(number) => number.to_string(),
        Value::Bool(flag) => flag.to_string(),
        _ => return None,
    };
    (!text.is_empty()).then_some(text)
}

fn predicate_holds(predicate: Option<&Predicate>, document: &Value) -> bool {
    let Some(predicate) = predicate else {
        return true;
    };
    match pointer_str(document, &predicate.pointer) {
        Some(value) => value.eq_ignore_ascii_case(&predicate.equals),
        None => predicate.missing_matches,
    }
}

/// First line of the pointed-to text, truncated at a word boundary. Falls back
/// to `{prefix} {short id}` so a title is never empty — `validate_output`
/// rejects blank titles.
fn title_for(rule: &TitleRule, document: &Value, raw_id: &str) -> String {
    let raw = pointer_str(document, &rule.pointer).unwrap_or_default();
    let first_line = raw.lines().next().unwrap_or_default().trim();
    if first_line.is_empty() {
        let short: String = raw_id.chars().take(8).collect();
        return format!("{} {short}", rule.fallback_prefix);
    }
    if first_line.chars().count() <= rule.max_chars {
        return first_line.to_string();
    }
    let truncated: String = first_line.chars().take(rule.max_chars).collect();
    match truncated.rfind(' ') {
        Some(space) if space > rule.max_chars / 2 => format!("{}…", &truncated[..space]),
        _ => format!("{truncated}…"),
    }
}

/// Canonical tags: trimmed, blanks dropped, deduplicated, order preserved.
/// `meta.tags` and the indexed `tags` must agree exactly.
fn tags_for(rules: &NodeRules, document: &Value) -> Vec<String> {
    let Some(pointer) = &rules.tags_pointer else {
        return Vec::new();
    };
    let Some(Value::Array(items)) = document.pointer(pointer) else {
        return Vec::new();
    };
    let mut seen = HashSet::with_capacity(items.len());
    items
        .iter()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|tag| !tag.is_empty())
        .filter(|tag| seen.insert(tag.to_string()))
        .map(str::to_string)
        .collect()
}

/// One discovery field value, or `None` when the document omits it. Absent is
/// omitted rather than emitted blank: `validate_output` rejects empty keyword
/// and date values.
fn field_value(rule: &FieldRule, document: &Value) -> Option<Value> {
    match rule.transform {
        Transform::SplitCsv => {
            let items = split_csv(document.pointer(&rule.pointer));
            (!items.is_empty()).then(|| serde_json::json!(items))
        }
        Transform::None => match document.pointer(&rule.pointer)? {
            Value::String(text) => {
                let text = text.trim();
                (!text.is_empty()).then(|| Value::String(text.to_string()))
            }
            value @ (Value::Number(_) | Value::Bool(_)) => Some(value.clone()),
            Value::Array(items) => {
                let items: Vec<String> = items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::trim)
                    .filter(|item| !item.is_empty())
                    .map(str::to_string)
                    .collect();
                (!items.is_empty()).then(|| serde_json::json!(items))
            }
            _ => None,
        },
    }
}

/// Values an edge rule references, after its transform.
fn rule_values(transform: Transform, document: &Value, pointer: &str) -> Vec<String> {
    match transform {
        Transform::SplitCsv => split_csv(document.pointer(pointer)),
        Transform::None => pointer_str(document, pointer).into_iter().collect(),
    }
}

/// `"tofu, Hydra"` becomes `["tofu", "Hydra"]`, deduplicated.
fn split_csv(value: Option<&Value>) -> Vec<String> {
    let Some(Value::String(raw)) = value else {
        return Vec::new();
    };
    let mut seen = HashSet::new();
    raw.split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .filter(|item| seen.insert(item.to_string()))
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests;
