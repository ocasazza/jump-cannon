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

use serde::{Deserialize, Serialize};
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
//
// The caps are env-tunable so the NATIVE server (graph-api) can raise them for
// the large self-assembly demos — the geometric GPU engine + 2-D dispatch tiling
// scales to millions of particles, and `soupGen` now builds in O(n). The default
// (50k) still protects a browser tab on the wasm path, where `std::env::var`
// always returns `Err` (no env) and the default holds.
const DEFAULT_MAX_NODES: usize = 50_000;
const DEFAULT_MAX_EDGES: usize = 200_000;

fn env_cap(var: &str, default: usize) -> usize {
    std::env::var(var)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

/// Output cap on node count. Override with `JC_MAX_NODES` (native server only).
fn max_nodes() -> usize {
    env_cap("JC_MAX_NODES", DEFAULT_MAX_NODES)
}

/// Output cap on edge count. Override with `JC_MAX_EDGES` (native server only).
fn max_edges() -> usize {
    env_cap("JC_MAX_EDGES", DEFAULT_MAX_EDGES)
}

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
    // The initial-position SEED interface + built-in implementations.
    // Self-contained (no birds.nix import) so it dodges the tvix
    // closure-across-imports bug.
    "/jc/src/seed.nix"              => "nix/seed.nix",
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
//
// These also serve as the canonical `{ nodes, links }` JSON WIRE for the
// server-side generate backend: graph-api evaluates natively, re-emits a
// `GeneratedGraph` through [`to_graph_json`] (which round-trips through these
// shapes), and the WASM client parses it back via [`parse_graph_json`]. They
// are `Serialize` for exactly that purpose.
#[derive(Serialize, Deserialize)]
struct RawNode {
    id: String,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    kind: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct RawEdge {
    source: String,
    target: String,
}

#[derive(Serialize, Deserialize)]
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
    parse_graph_json(&json)
}

/// Parse a `toGraphJSON`-shaped JSON string (`{ nodes = [...]; links = [...]; }`)
/// into a typed [`GeneratedGraph`], applying the same node/edge output caps as
/// [`eval_graph`].
///
/// This is the shared back half of [`eval_graph`] AND the parse side of the
/// SERVER generate backend: graph-api evaluates natively and returns this exact
/// `{ nodes, links }` JSON (via [`to_graph_json`]); the WASM client parses it
/// back here, so a server-side eval flows into the same promotion path as a
/// local eval with no shape drift.
pub fn parse_graph_json(json: &str) -> Result<GeneratedGraph, String> {
    let raw: RawGraph = serde_json::from_str(json).map_err(|e| {
        format!("result is not a {{ nodes, links }} graph (toGraphJSON shape): {e}")
    })?;

    let max_nodes = max_nodes();
    if raw.nodes.len() > max_nodes {
        return Err(format!(
            "graph too large: {} nodes exceeds the {max_nodes} node cap",
            raw.nodes.len()
        ));
    }
    let max_edges = max_edges();
    if raw.links.len() > max_edges {
        return Err(format!(
            "graph too large: {} edges exceeds the {max_edges} edge cap",
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

/// Serialise a [`GeneratedGraph`] to the canonical `{ nodes, links }` JSON wire
/// (the same shape `toGraphJSON` / [`parse_graph_json`] accept). Used by the
/// server-side generate backend to return an evaluated graph to the client.
pub fn to_graph_json(graph: &GeneratedGraph) -> String {
    let raw = RawGraph {
        nodes: graph
            .nodes
            .iter()
            .map(|n| RawNode {
                id: n.id.clone(),
                kind: n.kind.clone(),
            })
            .collect(),
        links: graph
            .edges
            .iter()
            .map(|e| RawEdge {
                source: e.source.clone(),
                target: e.target.clone(),
            })
            .collect(),
    };
    // The shapes are plain structs of owned strings — serialisation is
    // infallible in practice; fall back to an empty graph rather than panic.
    serde_json::to_string(&raw).unwrap_or_else(|_| r#"{"nodes":[],"links":[]}"#.to_string())
}

// ── Seed evaluation ─────────────────────────────────────────────────────────
//
// The SEED use case (alongside `eval_graph`): a user Nix expression implements
// the abstract seed interface documented in `seed.nix` and returns initial
// per-node positions. The interface is:
//
//   seed : { n, ... } -> [ { x; y; z; } ]
//
// The host appends `{ n = <count>; }` application context by passing `n` to the
// VFS expression via a `let`-binding, evaluates with the same deepSeq+toJSON +
// output-cap machinery as `eval_graph`, then validates that the returned list
// has exactly `n` entries (the empty list is the special "no seed" sentinel).

/// Cap on the number of seed positions accepted (mirrors `max_nodes()`).
fn max_seed_points() -> usize {
    max_nodes()
}

/// A single `{ x; y; z; }` position. Numbers may be ints or floats in Nix;
/// serde + `f32` coerce either.
#[derive(Deserialize)]
struct RawPos {
    x: f32,
    y: f32,
    z: f32,
}

/// Evaluate a Nix expression implementing the seed interface into per-node
/// positions.
///
/// `n` is the live graph's node count. It is injected into the evaluation as a
/// top-level `let n = <n>;` binding, so a custom expression can reference `n`
/// directly, and the built-in seeds are typically applied as
/// `(import /jc/src/seed.nix {}).sphere { inherit n; }`.
///
/// The expression must evaluate to a list of `{ x; y; z; }` attrsets. Returns:
///   * `Ok(vec)` with `vec.len() == n` for a normal seed,
///   * `Ok(vec![])` (empty) for the "no seed" sentinel (the `none` impl), which
///     the caller interprets as "leave positions as-is",
///   * `Err(_)` for an eval error, a non-list / wrong-shape result, or a list
///     whose length is neither `0` nor `n`.
pub fn eval_seed(expr: &str, n: usize) -> Result<Vec<[f32; 3]>, String> {
    // Inject `n` so the expression (and the built-in seeds) can reference it.
    let wrapped = format!("let n = {n}; in (\n{expr}\n)");
    let json = eval_to_json(&wrapped)?;

    let raw: Vec<RawPos> = serde_json::from_str(&json).map_err(|e| {
        format!(
            "seed result is not a list of {{ x; y; z; }} positions: {e}\n\
             (the seed interface is: seed : {{ n, ... }} -> [ {{ x; y; z; }} ])"
        )
    })?;

    // Empty list is the "no seed" sentinel — accept it regardless of n.
    if raw.is_empty() {
        return Ok(Vec::new());
    }

    let max_seed_points = max_seed_points();
    if raw.len() > max_seed_points {
        return Err(format!(
            "seed too large: {} positions exceeds the {max_seed_points} cap",
            raw.len()
        ));
    }

    if raw.len() != n {
        return Err(format!(
            "seed returned {} positions but the graph has {n} nodes \
             (a seed must return exactly n positions, or [] for no seed)",
            raw.len()
        ));
    }

    Ok(raw.into_iter().map(|p| [p.x, p.y, p.z]).collect())
}

/// Built-in seed example expressions for the Layout panel's seed picker. Each
/// evaluates against the embedded `seed.nix` library. `name` doubles as the
/// strategy label. Every entry is verified by the `all_seed_demos_evaluate`
/// test.
pub fn seed_demos() -> &'static [Demo] {
    SEED_DEMOS
}

const SEED_DEMOS: &[Demo] = &[
    Demo {
        name: "Sphere (Fibonacci shell)",
        expr: r#"# Fibonacci sphere shell — matches the renderer's default seed.
let s = import /jc/src/seed.nix {};
in s.sphere { inherit n; radius = 800.0; }
"#,
    },
    Demo {
        name: "Random (ball)",
        expr: r#"# Deterministic pseudo-random points in a cube.
let s = import /jc/src/seed.nix {};
in s.random { inherit n; radius = 800.0; seed = 1; }
"#,
    },
    Demo {
        name: "Grid (cubic lattice)",
        expr: r#"# Axis-aligned cubic lattice, side = ceil(cbrt n).
let s = import /jc/src/seed.nix {};
in s.grid { inherit n; spacing = 60.0; }
"#,
    },
    Demo {
        name: "No seed",
        expr: r#"# No seed: leave the current positions untouched.
let s = import /jc/src/seed.nix {};
in s.none { inherit n; }
"#,
    },
    Demo {
        name: "Custom (flat line)",
        expr: r#"# Author your own: a flat line along x. The interface is
#   seed : { n, ... } -> [ { x; y; z; } ]
# `n` is bound for you. Return exactly n positions (or [] for no seed).
builtins.genList (i: { x = i * 20.0; y = 0.0; z = 0.0; }) n
"#,
    },
];

// ── Demo catalog ──────────────────────────────────────────────────────────────

/// A named example expression for the Generate panel's demo picker.
#[derive(Debug, Clone, Copy)]
pub struct Demo {
    pub name: &'static str,
    pub expr: &'static str,
}

/// Built-in example expressions, each evaluating to a `toGraphJSON`-shaped graph
/// against the embedded library. Selecting one loads its source into the editor.
/// Every entry is verified to evaluate by the `all_demos_evaluate` test, so the
/// picker can never offer a broken example.
pub fn demos() -> &'static [Demo] {
    DEMOS
}

const DEMOS: &[Demo] = &[
    Demo {
        name: "Star (hub)",
        expr: r#"# Star: one hub connected to N spokes.
let
  g  = import /jc/src/graph.nix {};
  gc = import /jc/src/graph-combinators.nix { graph = g; };
in
  g.toGraphJSON (gc.starGen { nodes = 12; prefix = "n"; })
"#,
    },
    Demo {
        name: "Chain (path)",
        expr: r#"# Chain: a linear path — the primary self-assembly seed.
let
  g  = import /jc/src/graph.nix {};
  gc = import /jc/src/graph-combinators.nix { graph = g; };
in
  g.toGraphJSON (gc.pathGen { nodes = 16; prefix = "p"; })
"#,
    },
    Demo {
        name: "Ring (cycle)",
        expr: r#"# Ring: a closed cycle.
let
  g  = import /jc/src/graph.nix {};
  gc = import /jc/src/graph-combinators.nix { graph = g; };
in
  g.toGraphJSON (gc.cycleGen { nodes = 16; prefix = "c"; })
"#,
    },
    Demo {
        name: "Grid (sheet)",
        expr: r#"# Grid: a 2-D lattice — a flat "sheet" patch.
let
  g  = import /jc/src/graph.nix {};
  gc = import /jc/src/graph-combinators.nix { graph = g; };
in
  g.toGraphJSON (gc.gridGen { rows = 6; cols = 6; prefix = "g"; })
"#,
    },
    Demo {
        name: "Complete (K6)",
        expr: r#"# Complete: every node connected to every other.
let
  g  = import /jc/src/graph.nix {};
  gc = import /jc/src/graph-combinators.nix { graph = g; };
in
  g.toGraphJSON (gc.completeGen { nodes = 6; prefix = "k"; })
"#,
    },
    Demo {
        name: "Bridged stars",
        expr: r#"# Composition: two stars merged and joined by one bridge edge.
let
  g  = import /jc/src/graph.nix {};
  gc = import /jc/src/graph-combinators.nix { graph = g; };
  a = gc.starGen { nodes = 6; prefix = "a"; };
  b = gc.starGen { nodes = 6; prefix = "b"; };
in
  g.toGraphJSON (g.addEdge "bridge" "a0" "b0" true (g.merge a b))
"#,
    },
    Demo {
        name: "Soup (self-assembly seed)",
        expr: r#"# Unbonded particle soup: N isolated nodes, zero edges. The
# initial condition for the dynamic-bonding self-assembly engine — bonds
# (chains → sheets → tubes → vesicles) grow at runtime from this soup.
let
  g  = import /jc/src/graph.nix {};
  gc = import /jc/src/graph-combinators.nix { graph = g; };
in
  g.toGraphJSON (gc.soupGen { nodes = 200; prefix = "s"; })
"#,
    },
    Demo {
        name: "Custom (edge list)",
        expr: r#"# Author your own: build a graph from an explicit edge list.
let
  g = import /jc/src/graph.nix {};
in
  g.toGraphJSON (g.fromEdgeList [
    { source = "x"; target = "y"; }
    { source = "y"; target = "z"; }
    { source = "z"; target = "x"; }
  ])
"#,
    },
];

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

    /// The server generate backend evaluates natively, re-emits the graph as
    /// `{ nodes, links }` JSON (`to_graph_json`), and the client parses it back
    /// (`parse_graph_json`). That round-trip must reproduce the eval result
    /// exactly, including the optional `type`/`kind` field.
    #[test]
    fn graph_json_round_trip() {
        let graph = eval_graph(STAR_EXPR).expect("star eval should succeed");
        let json = to_graph_json(&graph);
        let parsed = parse_graph_json(&json).expect("re-parse should succeed");
        assert_eq!(parsed, graph, "round-trip must be lossless");

        // A present kind survives the round-trip.
        let center = parsed.nodes.iter().find(|n| n.id == "n0").unwrap();
        assert_eq!(center.kind.as_deref(), Some("center"));
    }

    /// A `kind = None` node must round-trip as an absent `type` field (and a
    /// present kind must survive) — the wire shape skips `None` on serialize.
    #[test]
    fn graph_json_round_trip_optional_kind() {
        let expr = r#"{
            nodes = [ { id = "a"; type = "x"; } { id = "b"; } ];
            links = [ { source = "a"; target = "b"; } ];
        }"#;
        let graph = eval_graph(expr).expect("inline graph should parse");
        let json = to_graph_json(&graph);
        // `b` has no kind → no `type` key in the JSON.
        assert!(
            json.contains(r#"{"id":"b"}"#),
            "None kind must serialize without a type field: {json}"
        );
        let parsed = parse_graph_json(&json).expect("re-parse");
        assert_eq!(parsed, graph, "round-trip must be lossless");
        assert_eq!(parsed.nodes[0].kind.as_deref(), Some("x"));
        assert_eq!(parsed.nodes[1].kind, None);
    }

    #[test]
    fn parse_graph_json_rejects_non_graph() {
        let err = parse_graph_json(r#"{"foo":1}"#).unwrap_err();
        assert!(err.contains("nodes, links"), "got: {err}");
    }

    #[test]
    fn non_graph_result_is_err() {
        // Valid Nix, valid JSON, but not a { nodes, links } graph.
        assert!(eval_graph("1 + 1").is_err());
    }

    #[test]
    fn node_cap_triggers() {
        // genList of (default cap + 1) trivial nodes, no links. Uses the default
        // (env unset) so the test stays cheap and deterministic.
        let expr = format!(
            "{{ nodes = builtins.genList (i: {{ id = builtins.toString i; }}) {}; links = []; }}",
            DEFAULT_MAX_NODES + 1
        );
        let err = eval_graph(&expr).unwrap_err();
        assert!(err.contains("node cap"), "unexpected error: {err}");
    }

    #[test]
    fn all_demos_evaluate() {
        // Every catalog demo must evaluate to a non-empty graph — the picker
        // can then never offer a broken example.
        for d in demos() {
            let g = eval_graph(d.expr)
                .unwrap_or_else(|e| panic!("demo {:?} failed to evaluate: {e}", d.name));
            assert!(!g.nodes.is_empty(), "demo {:?} produced no nodes", d.name);
            // Every edge endpoint must reference a declared node id.
            let ids: std::collections::HashSet<&str> =
                g.nodes.iter().map(|n| n.id.as_str()).collect();
            for e in &g.edges {
                assert!(
                    ids.contains(e.source.as_str()) && ids.contains(e.target.as_str()),
                    "demo {:?} has a dangling edge {} -> {}",
                    d.name,
                    e.source,
                    e.target
                );
            }
        }
    }

    // ── Seed interface ────────────────────────────────────────────────────

    #[test]
    fn seed_sphere_returns_n_positions() {
        let expr = "let s = import /jc/src/seed.nix {}; in s.sphere { inherit n; }";
        let pts = eval_seed(expr, 32).expect("sphere seed should evaluate");
        assert_eq!(pts.len(), 32, "sphere returns n positions");
        // Every point should sit ~on the radius-800 shell (our sqrt/trig are
        // approximate, so allow a loose tolerance).
        for p in &pts {
            let r = (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt();
            assert!(
                (r - 800.0).abs() < 80.0,
                "point off the shell: r={r} ({p:?})"
            );
        }
    }

    #[test]
    fn seed_random_returns_n_positions() {
        let expr = "let s = import /jc/src/seed.nix {}; in s.random { inherit n; }";
        let pts = eval_seed(expr, 50).expect("random seed should evaluate");
        assert_eq!(pts.len(), 50);
        // Determinism: the same expr + n yields identical output.
        let pts2 = eval_seed(expr, 50).unwrap();
        assert_eq!(pts, pts2, "random seed is deterministic");
    }

    #[test]
    fn seed_grid_returns_n_positions() {
        let expr = "let s = import /jc/src/seed.nix {}; in s.grid { inherit n; }";
        let pts = eval_seed(expr, 27).expect("grid seed should evaluate");
        assert_eq!(pts.len(), 27);
    }

    #[test]
    fn seed_none_returns_empty() {
        let expr = "let s = import /jc/src/seed.nix {}; in s.none { inherit n; }";
        // The "no seed" sentinel: empty list, accepted regardless of n.
        let pts = eval_seed(expr, 100).expect("none seed should evaluate");
        assert!(pts.is_empty(), "none returns the empty sentinel");
    }

    #[test]
    fn seed_custom_inline_works() {
        // A user-authored seed referencing the injected `n`.
        let expr = "builtins.genList (i: { x = i * 1.0; y = 0.0; z = 0.0; }) n";
        let pts = eval_seed(expr, 8).expect("custom seed should evaluate");
        assert_eq!(pts.len(), 8);
        assert_eq!(pts[0], [0.0, 0.0, 0.0]);
        assert_eq!(pts[7], [7.0, 0.0, 0.0]);
    }

    #[test]
    fn seed_wrong_length_is_err() {
        // Returns 3 positions but graph has 5 → error.
        let expr = "[ { x = 0; y = 0; z = 0; } { x = 1; y = 0; z = 0; } { x = 2; y = 0; z = 0; } ]";
        let err = eval_seed(expr, 5).unwrap_err();
        assert!(err.contains("exactly n"), "unexpected error: {err}");
    }

    #[test]
    fn seed_bad_expr_is_err() {
        assert!(eval_seed("let x = in", 4).is_err());
        // Valid Nix, wrong shape (not a list of {x;y;z}).
        assert!(eval_seed("42", 4).is_err());
    }

    #[test]
    fn all_seed_demos_evaluate() {
        // Every seed demo must evaluate for a representative n. "No seed"
        // yields the empty sentinel; the rest yield exactly n positions.
        for d in seed_demos() {
            let pts = eval_seed(d.expr, 24)
                .unwrap_or_else(|e| panic!("seed demo {:?} failed: {e}", d.name));
            if d.name == "No seed" {
                assert!(pts.is_empty(), "No seed must be empty");
            } else {
                assert_eq!(pts.len(), 24, "seed demo {:?} must return n positions", d.name);
            }
        }
    }

    #[test]
    fn soup_gen_is_unbonded() {
        // The self-assembly soup must be N nodes with ZERO edges — bonds are
        // grown at runtime by the geometric engine, not pre-wired.
        let expr = r#"
            let
              g  = import /jc/src/graph.nix {};
              gc = import /jc/src/graph-combinators.nix { graph = g; };
            in g.toGraphJSON (gc.soupGen { nodes = 64; prefix = "s"; })
        "#;
        let graph = eval_graph(expr).expect("soup eval should succeed");
        assert_eq!(graph.nodes.len(), 64, "soup must have n nodes");
        assert!(graph.edges.is_empty(), "soup must have no edges");
        assert_eq!(graph.nodes[0].kind.as_deref(), Some("particle"));
    }

    #[test]
    fn edge_cap_triggers() {
        let expr = format!(
            "{{ nodes = []; links = builtins.genList (i: {{ source = \"a\"; target = \"b\"; }}) {}; }}",
            DEFAULT_MAX_EDGES + 1
        );
        let err = eval_graph(&expr).unwrap_err();
        assert!(err.contains("edge cap"), "unexpected error: {err}");
    }
}
