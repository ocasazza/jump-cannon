//! Source-neutral full-text index built from importer discovery documents.
//!
//! The importer owns the schema and emitted values; graph-api owns indexing.
//! This keeps source authority separate from pure host-side discovery and lets
//! graph, search, and facets publish atomically under one graph revision.

use std::collections::HashMap;

use anyhow::{Context, Result};
use data_loader::{DiscoveryField, DiscoveryFieldType, ImporterSchema, SearchDocument};
use tantivy::{
    collector::{Count, TopDocs},
    doc,
    query::QueryParser,
    schema::{
        Field, IndexRecordOption, Schema, TextFieldIndexing, TextOptions, Value as TantivyValue,
        STORED, STRING,
    },
    Index, IndexReader, ReloadPolicy, SnippetGenerator, TantivyDocument,
};

const WRITER_MEMORY_BYTES: usize = 50_000_000;

#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    pub id: String,
    pub score: f32,
    pub snippet: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SearchResults {
    pub total: usize,
    pub hits: Vec<SearchHit>,
}

#[derive(Clone)]
struct IndexedField {
    field: Field,
    descriptor: DiscoveryField,
}

/// Immutable Tantivy index associated with one `GraphSnapshot`.
pub struct SearchIndex {
    // Keep the in-memory directory and tokenizer registry alive with the reader.
    _index: Index,
    reader: IndexReader,
    node_id: Field,
    fields: Vec<IndexedField>,
}

impl SearchIndex {
    pub fn build(schema: &ImporterSchema, documents: &[SearchDocument]) -> Result<Self> {
        schema.validate().map_err(anyhow::Error::msg)?;

        let mut builder = Schema::builder();
        let node_id = builder.add_text_field("_node_id", STRING | STORED);
        let text_indexing = TextFieldIndexing::default()
            .set_tokenizer("en_stem")
            .set_index_option(IndexRecordOption::WithFreqsAndPositions);
        let text_options = TextOptions::default()
            .set_indexing_options(text_indexing)
            .set_stored();

        let mut fields = Vec::new();
        for descriptor in schema.searchable_fields() {
            let options = match descriptor.field_type {
                DiscoveryFieldType::Text => text_options.clone(),
                DiscoveryFieldType::Keyword
                | DiscoveryFieldType::KeywordList
                | DiscoveryFieldType::Number
                | DiscoveryFieldType::Boolean
                | DiscoveryFieldType::Date
                | DiscoveryFieldType::Url => STRING | STORED,
            };
            let field = builder.add_text_field(&descriptor.key, options);
            fields.push(IndexedField {
                field,
                descriptor: descriptor.clone(),
            });
        }

        let index = Index::create_in_ram(builder.build());
        let mut writer = index
            .writer(WRITER_MEMORY_BYTES)
            .context("create importer search index writer")?;
        let descriptors: HashMap<&str, &DiscoveryField> = schema
            .fields
            .iter()
            .map(|descriptor| (descriptor.key.as_str(), descriptor))
            .collect();

        for source in documents {
            let mut document = doc!(node_id => source.node_id.clone());
            for indexed in &fields {
                let descriptor = descriptors
                    .get(indexed.descriptor.key.as_str())
                    .expect("indexed fields come from the validated schema");
                let value = source
                    .fields
                    .get(&descriptor.key)
                    .or(descriptor.default_value.as_ref());
                if let Some(value) = value {
                    add_value(&mut document, indexed.field, descriptor.field_type, value);
                }
            }
            writer
                .add_document(document)
                .context("add importer search document")?;
        }
        writer.commit().context("commit importer search index")?;

        let reader: IndexReader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .context("open importer search reader")?;
        reader.reload().context("load importer search reader")?;

        Ok(Self {
            _index: index,
            reader,
            node_id,
            fields,
        })
    }

    pub fn search(&self, query: &str, limit: usize, snippets: bool) -> Result<SearchResults> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(SearchResults {
                total: 0,
                hits: Vec::new(),
            });
        }

        let default_fields = self.fields.iter().map(|field| field.field).collect();
        let searcher = self.reader.searcher();
        let index = searcher.index();
        let mut parser = QueryParser::for_index(index, default_fields);
        parser.set_conjunction_by_default();
        for indexed in &self.fields {
            parser.set_field_boost(indexed.field, f32::from(indexed.descriptor.boost));
        }
        let parsed = parser
            .parse_query(query)
            .with_context(|| format!("parse discovery query {query:?}"))?;

        let (mut top, mut total) = searcher
            .search(&parsed, &(TopDocs::with_limit(limit), Count))
            .context("search importer discovery index")?;

        // Exact parsing is conjunctive and term-exact, so a typo or a partial
        // word ("aspirn", "aspir") scores nothing at all. When it finds
        // nothing, retry the same terms as a fuzzy/prefix disjunction over
        // every searchable field. Precision first: this never dilutes a query
        // that already matched.
        let mut fuzzy_query = None;
        if top.is_empty() {
            if let Some(fallback) = self.fuzzy_query(query) {
                let (fuzzy_top, fuzzy_total) = searcher
                    .search(&fallback, &(TopDocs::with_limit(limit), Count))
                    .context("fuzzy search importer discovery index")?;
                top = fuzzy_top;
                total = fuzzy_total;
                fuzzy_query = Some(fallback);
            }
        }
        let scoring: &dyn tantivy::query::Query = match &fuzzy_query {
            Some(fallback) => fallback.as_ref(),
            None => &*parsed,
        };

        let snippet_generators = if snippets {
            self.fields
                .iter()
                .filter(|field| field.descriptor.snippet)
                .map(|field| {
                    SnippetGenerator::create(&searcher, scoring, field.field)
                        .map(|generator| (field.field, generator))
                })
                .collect::<tantivy::Result<Vec<_>>>()?
        } else {
            Vec::new()
        };

        let mut hits = Vec::with_capacity(top.len());
        for (score, address) in top {
            let document: TantivyDocument = searcher
                .doc(address)
                .context("load importer search result")?;
            let Some(id) = document
                .get_first(self.node_id)
                .and_then(|value| value.as_str())
            else {
                continue;
            };
            let snippet = snippet_generators
                .iter()
                .map(|(_, generator)| generator.snippet_from_doc(&document).to_html())
                .find(|html| html.contains("<b>"))
                .unwrap_or_default();
            hits.push(SearchHit {
                id: id.to_string(),
                score,
                snippet,
            });
        }

        Ok(SearchResults { total, hits })
    }

    /// Build the typo/prefix-tolerant fallback for a query that matched
    /// nothing exactly: every whitespace term becomes a Levenshtein match
    /// (distance scaled by term length) plus a prefix match, `Should`-joined
    /// across every searchable field so a hit on any field counts.
    ///
    /// Returns `None` when there is nothing to fuzz (no usable terms, or the
    /// query used field-qualified syntax like `title:foo`, which the exact
    /// parser already handles precisely and whose `field:value` shape would
    /// otherwise be fuzzed as a literal term).
    fn fuzzy_query(&self, query: &str) -> Option<Box<dyn tantivy::query::Query>> {
        use tantivy::query::{BooleanQuery, FuzzyTermQuery, Occur};
        use tantivy::schema::Term;

        if query.contains(':') || self.fields.is_empty() {
            return None;
        }
        let terms: Vec<String> = query
            .split_whitespace()
            .map(|term| term.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase())
            .filter(|term| term.len() >= 3)
            .collect();
        if terms.is_empty() {
            return None;
        }
        let mut clauses: Vec<(Occur, Box<dyn tantivy::query::Query>)> = Vec::new();
        for term_text in &terms {
            // Tantivy caps Levenshtein distance at 2; short words get 1 so
            // "gene" cannot fuzz into every four-letter token.
            let distance = if term_text.len() <= 5 { 1 } else { 2 };
            for indexed in &self.fields {
                if !indexed.descriptor.searchable {
                    continue;
                }
                let term = Term::from_field_text(indexed.field, term_text);
                clauses.push((
                    Occur::Should,
                    Box::new(FuzzyTermQuery::new(term.clone(), distance, true)),
                ));
                clauses.push((
                    Occur::Should,
                    Box::new(FuzzyTermQuery::new_prefix(term, 0, true)),
                ));
            }
        }
        if clauses.is_empty() {
            return None;
        }
        Some(Box::new(BooleanQuery::new(clauses)))
    }
}

fn add_value(
    document: &mut TantivyDocument,
    field: Field,
    field_type: DiscoveryFieldType,
    value: &serde_json::Value,
) {
    match field_type {
        DiscoveryFieldType::KeywordList => {
            if let Some(values) = value.as_array() {
                for value in values.iter().filter_map(serde_json::Value::as_str) {
                    document.add_text(field, value);
                }
            }
        }
        DiscoveryFieldType::Text
        | DiscoveryFieldType::Keyword
        | DiscoveryFieldType::Date
        | DiscoveryFieldType::Url => {
            if let Some(value) = value.as_str() {
                document.add_text(field, value);
            }
        }
        DiscoveryFieldType::Number | DiscoveryFieldType::Boolean => {
            document.add_text(field, value.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use data_loader::{
        DiscoveryField, EdgeTypeSchema, ImporterSchema, SearchDocument, TagHierarchySchema,
    };
    use serde_json::json;

    use super::*;

    fn schema() -> ImporterSchema {
        ImporterSchema::new(
            "okf",
            vec![
                DiscoveryField::new("id", DiscoveryFieldType::Keyword, true).searchable(2),
                DiscoveryField::new("title", DiscoveryFieldType::Text, true)
                    .searchable(4)
                    .snippet(),
                DiscoveryField::new("tags", DiscoveryFieldType::KeywordList, true)
                    .searchable(3)
                    .facetable(),
                DiscoveryField::new("kind", DiscoveryFieldType::Keyword, true)
                    .searchable(2)
                    .facetable(),
            ],
            vec![EdgeTypeSchema::directed("relationship", "test edge")],
            TagHierarchySchema::slash(),
        )
    }

    #[test]
    fn searches_declared_fields_and_preserves_namespaced_ids() {
        let documents = vec![
            SearchDocument::new("okf:catalog:alpha")
                .with("id", "okf:catalog:alpha")
                .with("title", "Customer orders")
                .with("tags", json!(["finance", "revenue"]))
                .with("kind", "BigQuery Table"),
            SearchDocument::new("okf:catalog:beta")
                .with("id", "okf:catalog:beta")
                .with("title", "Incident response")
                .with("tags", json!(["oncall"]))
                .with("kind", "Playbook"),
        ];
        let index = SearchIndex::build(&schema(), &documents).unwrap();

        let tag = index.search("tags:revenue", 10, true).unwrap();
        assert_eq!(tag.total, 1);
        assert_eq!(tag.hits[0].id, "okf:catalog:alpha");

        let kind = index.search("kind:Playbook", 10, false).unwrap();
        assert_eq!(kind.total, 1);
        assert_eq!(kind.hits[0].id, "okf:catalog:beta");

        // Keyword fields use Tantivy's raw tokenizer: partial values must not
        // silently acquire full-text/stemming semantics.
        assert_eq!(index.search("tags:rev", 10, false).unwrap().total, 0);
    }

    #[test]
    fn rejects_unknown_query_fields() {
        let documents = vec![SearchDocument::new("n1")
            .with("id", "n1")
            .with("title", "One")
            .with("tags", json!([]))
            .with("kind", "demo")];
        let index = SearchIndex::build(&schema(), &documents).unwrap();
        assert!(index.search("secret:value", 10, false).is_err());
    }

    /// Exploring an imported corpus means typing a half-remembered name:
    /// "aspirn" (typo) and "aspir" (prefix) must find aspirin, while an exact
    /// match keeps its precision — a query that already hits must not be
    /// widened into everything that is merely similar.
    #[test]
    fn fuzzy_fallback_tolerates_typos_and_prefixes_without_diluting_exact_hits() {
        let documents = vec![
            SearchDocument::new("chembl:molecule:CHEMBL25")
                .with("id", "chembl:molecule:CHEMBL25")
                .with("title", "ASPIRIN")
                .with("tags", json!(["analgesic"]))
                .with("kind", "molecule"),
            SearchDocument::new("chembl:molecule:CHEMBL1201823")
                .with("id", "chembl:molecule:CHEMBL1201823")
                .with("title", "ADALIMUMAB")
                .with("tags", json!(["antibody"]))
                .with("kind", "molecule"),
        ];
        let index = SearchIndex::build(&schema(), &documents).unwrap();

        let typo = index.search("aspirn", 10, false).unwrap();
        assert_eq!(typo.hits.first().map(|hit| hit.id.as_str()), Some("chembl:molecule:CHEMBL25"), "one-edit typo finds aspirin: {typo:?}");

        let prefix = index.search("aspir", 10, false).unwrap();
        assert_eq!(prefix.hits.first().map(|hit| hit.id.as_str()), Some("chembl:molecule:CHEMBL25"), "prefix finds aspirin: {prefix:?}");

        // An exact hit stays exact: the fallback only runs on an empty result,
        // so "adalimumab" must not also drag in the unrelated molecule.
        let exact = index.search("adalimumab", 10, false).unwrap();
        assert_eq!(exact.total, 1);
        assert_eq!(exact.hits[0].id, "chembl:molecule:CHEMBL1201823");

        // Nonsense stays empty rather than matching the whole corpus.
        assert_eq!(index.search("zzzzqqqqxxxx", 10, false).unwrap().total, 0);
    }
}
