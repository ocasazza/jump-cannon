//! tvix-wasm — real Nix evaluation, native and in the browser, via tvix-eval.
//!
//! User code is evaluated with tvix-eval against an in-memory virtual
//! filesystem that serves an embedded graph-generation Nix library
//! (`graph.nix` + `graph-combinators.nix`, dogfooding `birds.nix`). The result
//! is serialised with `builtins.toJSON` (a pure builtin at the pinned rev) and
//! deserialised into a typed [`GeneratedGraph`].
//!
//! `tvix-eval` is built with `default-features = false`, which drops the
//! impure / arbitrary / nix_tests features and makes evaluation pure by
//! construction — this is what lets the crate compile for
//! `wasm32-unknown-unknown`.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use serde::Deserialize;
use tvix_eval::{EvalIO, Evaluation, FileType};

#[cfg(feature = "wasm")]
use wasm_bindgen::prelude::*;

// ── Output caps ─────────────────────────────────────────────────────────────
//
// tvix has no fuel / step limit, so a malicious or runaway expression can
// produce an arbitrarily large graph. Eval runs inline on the caller's thread
// for now (a Web Worker + timeout is a later follow-up), but we cap the *output*
// size with a clear error. A node id is unbounded in length so this is not a
// hard memory bound, but it stops the common runaway cases cheaply.
const MAX_NODES: usize = 50_000;
const MAX_EDGES: usize = 200_000;

// ── Virtual filesystem ──────────────────────────────────────────────────────
//
// Embeds the graph-generation Nix library at compile time. When tvix-eval
// encounters `import /jc/src/graph.nix {}`, the VFS serves the file content
// from memory. Paths live under the virtual root `/jc/src/`.

/// Embed Nix source files at compile time, keyed by their virtual path.
macro_rules! embed_nix_files {
    ($($vpath:expr => $rpath:expr),+ $(,)?) => {
        fn build_vfs() -> HashMap<PathBuf, &'static str> {
            let mut m = HashMap::new();
            $(
                m.insert(PathBuf::from($vpath), include_str!($rpath));
            )+
            m
        }
    };
}

embed_nix_files! {
    // graph.nix dogfoods birds.nix via `import ./birds.nix {}`, so it must be
    // present in the VFS too. graph-combinators.nix takes { graph } as an
    // explicit parameter (the tvix closure-across-3+-imports workaround) and so
    // does NOT import birds.nix itself.
    "/jc/src/birds.nix"             => "nix/birds.nix",
    "/jc/src/graph.nix"             => "nix/graph.nix",
    "/jc/src/graph-combinators.nix" => "nix/graph-combinators.nix",
}

/// The virtual directory the embedded library lives in. User code is evaluated
/// with this as its "current file" location so relative imports resolve.
const VFS_ROOT: &str = "/jc/src";

/// In-memory filesystem for tvix-eval. Serves the embedded Nix library.
struct GraphNixIO {
    files: HashMap<PathBuf, String>,
}

impl GraphNixIO {
    fn new() -> Self {
        let mut files: HashMap<PathBuf, String> = HashMap::new();
        for (path, content) in build_vfs() {
            files.insert(path, content.to_string());
        }
        GraphNixIO { files }
    }
}

impl EvalIO for GraphNixIO {
    fn path_exists(&self, path: &Path) -> io::Result<bool> {
        if self.files.contains_key(path) {
            return Ok(true);
        }
        // Directory: some embedded file lives below this path.
        let prefix = path.to_string_lossy();
        let is_dir = self.files.keys().any(|k| {
            let k = k.to_string_lossy();
            k.starts_with(prefix.as_ref()) && k.len() > prefix.len()
        });
        Ok(is_dir)
    }

    fn open(&self, path: &Path) -> io::Result<Box<dyn io::Read>> {
        match self.files.get(path) {
            Some(content) => Ok(Box::new(io::Cursor::new(content.clone().into_bytes()))),
            None => Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("file not found in VFS: {}", path.display()),
            )),
        }
    }

    fn file_type(&self, path: &Path) -> io::Result<FileType> {
        if self.files.contains_key(path) {
            return Ok(FileType::Regular);
        }
        let prefix = path.to_string_lossy();
        let is_dir = self.files.keys().any(|k| {
            let k = k.to_string_lossy();
            k.starts_with(prefix.as_ref()) && k.len() > prefix.len()
        });
        if is_dir {
            Ok(FileType::Directory)
        } else {
            Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("not found in VFS: {}", path.display()),
            ))
        }
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<(bytes::Bytes, FileType)>> {
        let prefix = format!("{}/", path.display());
        let mut entries: HashMap<String, FileType> = HashMap::new();

        for key in self.files.keys() {
            let key = key.to_string_lossy();
            if let Some(rest) = key.strip_prefix(&prefix) {
                let (name, ft) = match rest.find('/') {
                    Some(idx) => (rest[..idx].to_string(), FileType::Directory),
                    None => (rest.to_string(), FileType::Regular),
                };
                entries.insert(name, ft);
            }
        }

        Ok(entries
            .into_iter()
            .map(|(name, ft)| (bytes::Bytes::from(name), ft))
            .collect())
    }

    fn import_path(&self, path: &Path) -> io::Result<PathBuf> {
        // In-memory: paths are already canonical.
        Ok(path.to_path_buf())
    }
}

// ── Typed result ────────────────────────────────────────────────────────────

/// A graph produced by evaluating a Nix expression.
///
/// This is the typed projection of `toGraphJSON`'s
/// `{ nodes = [{ id, type?, ... }]; links = [{ source, target, ... }]; }`.
#[derive(Debug, Clone, PartialEq)]
pub struct GeneratedGraph {
    pub nodes: Vec<GenNode>,
    pub edges: Vec<GenEdge>,
}

/// A node. `kind` is the Nix `type` field (renamed to dodge the Rust keyword).
#[derive(Debug, Clone, PartialEq)]
pub struct GenNode {
    pub id: String,
    pub kind: Option<String>,
}

/// A directed/undirected edge between two node ids.
#[derive(Debug, Clone, PartialEq)]
pub struct GenEdge {
    pub source: String,
    pub target: String,
}

// Wire shapes matching toGraphJSON output. `#[serde(flatten)]` is avoided and
// extra fields are simply ignored, so node/edge metadata does not break parsing.
#[derive(Deserialize)]
struct RawNode {
    id: String,
    #[serde(rename = "type")]
    kind: Option<String>,
}

#[derive(Deserialize)]
struct RawEdge {
    source: String,
    target: String,
}

#[derive(Deserialize)]
struct RawGraph {
    nodes: Vec<RawNode>,
    /// toGraphJSON emits `links`; accept that name.
    links: Vec<RawEdge>,
}

// ── Evaluation ──────────────────────────────────────────────────────────────

/// Evaluate `expr` to a JSON string via tvix-eval against the embedded library.
///
/// The user expression is wrapped so lazy thunks are forced (`deepSeq`, which
/// surfaces errors) and the result is rendered as JSON (`toJSON`, a pure builtin
/// at the pinned rev).
fn eval_to_json(expr: &str) -> Result<String, String> {
    let wrapped = format!(
        "let __r = ( {expr}\n ); in builtins.deepSeq __r (builtins.toJSON __r)"
    );

    let io = Rc::new(GraphNixIO::new());
    let eval = Evaluation::builder(io as Rc<dyn EvalIO>)
        .enable_import()
        .build();

    let result = eval.evaluate(&wrapped, Some(PathBuf::from(VFS_ROOT)));

    if !result.errors.is_empty() {
        let msgs: Vec<String> = result.errors.iter().map(|e| format!("{:#}", e)).collect();
        return Err(msgs.join("\n"));
    }

    match result.value {
        // toJSON yields a Nix string value holding the JSON text.
        Some(value) => match value {
            tvix_eval::Value::String(s) => {
                Ok(String::from_utf8_lossy(s.as_ref()).into_owned())
            }
            other => Err(format!(
                "expected a JSON string from toJSON, got: {}",
                other.type_of()
            )),
        },
        None => Err("evaluation produced no value".to_string()),
    }
}

/// Evaluate a Nix expression into a typed [`GeneratedGraph`].
///
/// `expr` is evaluated with the embedded graph library importable via
/// `import /jc/src/graph.nix {}` and
/// `import /jc/src/graph-combinators.nix { graph = ...; }`. The expression must
/// evaluate to a `toGraphJSON`-shaped attrset
/// (`{ nodes = [...]; links = [...]; }`).
pub fn eval_graph(expr: &str) -> Result<GeneratedGraph, String> {
    let json = eval_to_json(expr)?;

    let raw: RawGraph = serde_json::from_str(&json).map_err(|e| {
        format!("result is not a {{ nodes, links }} graph (toGraphJSON shape): {e}")
    })?;

    if raw.nodes.len() > MAX_NODES {
        return Err(format!(
            "graph too large: {} nodes exceeds the {MAX_NODES} node cap",
            raw.nodes.len()
        ));
    }
    if raw.links.len() > MAX_EDGES {
        return Err(format!(
            "graph too large: {} edges exceeds the {MAX_EDGES} edge cap",
            raw.links.len()
        ));
    }

    let nodes = raw
        .nodes
        .into_iter()
        .map(|n| GenNode {
            id: n.id,
            kind: n.kind,
        })
        .collect();
    let edges = raw
        .links
        .into_iter()
        .map(|e| GenEdge {
            source: e.source,
            target: e.target,
        })
        .collect();

    Ok(GeneratedGraph { nodes, edges })
}

// ── WASM API ────────────────────────────────────────────────────────────────

/// Evaluate a Nix expression and return the generated graph as JSON:
/// `{ "ok": true, "nodes": [...], "edges": [...] }`
/// or `{ "ok": false, "error": "..." }`.
#[cfg(feature = "wasm")]
#[wasm_bindgen]
pub fn eval_graph_json(expr: &str) -> String {
    match eval_graph(expr) {
        Ok(g) => serde_json::json!({
            "ok": true,
            "nodes": g.nodes.iter().map(|n| serde_json::json!({
                "id": n.id,
                "kind": n.kind,
            })).collect::<Vec<_>>(),
            "edges": g.edges.iter().map(|e| serde_json::json!({
                "source": e.source,
                "target": e.target,
            })).collect::<Vec<_>>(),
        })
        .to_string(),
        Err(err) => serde_json::json!({ "ok": false, "error": err }).to_string(),
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// A star graph built through the embedded combinator library + toGraphJSON.
    /// `graph-combinators.nix` takes `{ graph }` explicitly (the tvix closure
    /// workaround), so we thread it through verbatim.
    const STAR_EXPR: &str = r#"
        let
          g  = import /jc/src/graph.nix {};
          gc = import /jc/src/graph-combinators.nix { graph = g; };
        in g.toGraphJSON (gc.starGen { nodes = 5; prefix = "n"; })
    "#;

    #[test]
    fn star_graph_counts() {
        let graph = eval_graph(STAR_EXPR).expect("star eval should succeed");
        // 5 nodes (center + 4 spokes), 4 hub->spoke edges.
        assert_eq!(graph.nodes.len(), 5, "nodes: {:?}", graph.nodes);
        assert_eq!(graph.edges.len(), 4, "edges: {:?}", graph.edges);

        // center is "n0", every edge originates from it.
        assert!(graph.nodes.iter().any(|n| n.id == "n0"));
        assert!(graph.edges.iter().all(|e| e.source == "n0"));
        // node `type` is carried into `kind`.
        let center = graph.nodes.iter().find(|n| n.id == "n0").unwrap();
        assert_eq!(center.kind.as_deref(), Some("center"));
    }

    #[test]
    fn inline_graph_json_shape() {
        // A hand-written toGraphJSON-shaped attrset (no library import).
        let expr = r#"{
            nodes = [ { id = "a"; type = "x"; } { id = "b"; } ];
            links = [ { source = "a"; target = "b"; } ];
        }"#;
        let g = eval_graph(expr).expect("inline graph should parse");
        assert_eq!(g.nodes.len(), 2);
        assert_eq!(g.edges.len(), 1);
        assert_eq!(g.nodes[0].kind.as_deref(), Some("x"));
        assert_eq!(g.nodes[1].kind, None);
        assert_eq!(g.edges[0].source, "a");
        assert_eq!(g.edges[0].target, "b");
    }

    #[test]
    fn syntax_error_is_err() {
        let err = eval_graph("let x = in").unwrap_err();
        assert!(!err.is_empty(), "expected a non-empty error message");
    }

    #[test]
    fn non_graph_result_is_err() {
        // Valid Nix, valid JSON, but not a { nodes, links } graph.
        assert!(eval_graph("1 + 1").is_err());
    }

    #[test]
    fn node_cap_triggers() {
        // genList of MAX_NODES + 1 trivial nodes, no links.
        let expr = format!(
            "{{ nodes = builtins.genList (i: {{ id = builtins.toString i; }}) {}; links = []; }}",
            MAX_NODES + 1
        );
        let err = eval_graph(&expr).unwrap_err();
        assert!(err.contains("node cap"), "unexpected error: {err}");
    }

    #[test]
    fn edge_cap_triggers() {
        let expr = format!(
            "{{ nodes = []; links = builtins.genList (i: {{ source = \"a\"; target = \"b\"; }}) {}; }}",
            MAX_EDGES + 1
        );
        let err = eval_graph(&expr).unwrap_err();
        assert!(err.contains("edge cap"), "unexpected error: {err}");
    }
}
