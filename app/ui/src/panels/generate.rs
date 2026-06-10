//! Generate panel — Dioxus port of crates/graph-renderer/src/ui/sections/generate.rs
//! (the tvix-expr Generate panel; graph conversion ported from
//! crates/graph-renderer/src/generate.rs::bootstrap_from_generated).
//!
//! Two graph-api endpoints back this panel:
//!   - `POST /generate { expr }` → `{ ok, graph: { nodes, links }, error }` —
//!     the egui panel's Server execution backend (`GenerateBackendChoice::Server`).
//!     The server evaluates the Nix expression with the same embedded tvix
//!     library the egui app bundles, so the demo catalog below evaluates
//!     identically. `/generate` does NOT host the result; on success the graph
//!     is converted client-side and replaces the live canvas graph — the same
//!     contract as the egui `App::update` drain of `state.generate.pending`.
//!   - `POST /compute/soup { n, radius, seed, morphology }` — the self-assembly
//!     particle-soup demo. The server hosts the soup as its active graph AND
//!     selects the Geometric (GPU) engine with the validated membrane regime,
//!     so the Examples (self-assembly) picker runs in one click here, followed
//!     by `crate::reload_graph` to fetch the hosted soup. (The egui catalog
//!     instead swaps in a full `AppState` and the user presses Evaluate.)
//!
//! Panel-local state lives in `GlobalSignal`s inside this module (not on
//! `crate::Ctx`) so each panel file is self-contained. User-facing settings
//! persist to localStorage under "jc_generate_v1". Renderer access goes through
//! `crate::render` (`mount_canvas` swaps the live scene in place).

use std::collections::HashMap;

use dioxus::prelude::*;
use gloo_net::http::Request;
use gloo_storage::{LocalStorage, Storage};
use serde::{Deserialize, Serialize};

use crate::api::{err, url};
use crate::graph_canvas::GraphData;
use crate::render;
use crate::Ctx;

// --- constants (verbatim from the egui app) -----------------------------------

/// Radius of the seed sphere shell for a freshly generated graph — matches
/// `crates/graph-renderer/src/generate.rs::SPAWN_RADIUS` (and this app's
/// network-bootstrap path) so the result fits the camera the same way.
const SPAWN_RADIUS: f32 = 800.0;

/// Client-side mirror of the server's `MAX_SOUP_NODES` cap so an out-of-range
/// `n` is rejected before the round-trip.
const MAX_SOUP_NODES: u32 = 1_000_000;

/// The prefilled star-graph demo — verbatim `GENERATE_DEMO_EXPR` from
/// `crates/graph-renderer/src/ui/state.rs` (`GenerateState::with_demo`).
const GENERATE_DEMO_EXPR: &str = r#"# Edit this Nix expression, then press Evaluate.
# It must produce toGraphJSON's { nodes = [...]; links = [...]; } shape.
let
  g  = import /jc/src/graph.nix {};
  gc = import /jc/src/graph-combinators.nix { graph = g; };
in
  g.toGraphJSON (gc.starGen { nodes = 12; prefix = "n"; })
"#;

/// A named example expression for the editor's demo picker.
struct Demo {
    name: &'static str,
    expr: &'static str,
}

/// The built-in editor examples — verbatim `tvix_wasm::demos()` (this crate
/// does not depend on tvix-wasm; the server embeds the same library, so every
/// entry evaluates over `POST /generate` exactly as it does locally in the
/// egui app, and the catalog is covered by tvix-wasm's `all_demos_evaluate`).
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

/// One self-assembly example — mirrors `crates/graph-renderer/src/ui/examples.rs`
/// (`examples::catalog()`), keyed to the server's `/compute/soup` morphology
/// strings (its `membrane_lens` duplicates the renderer's `SelfAssemblyPreset`).
struct SoupExample {
    name: &'static str,
    description: &'static str,
    /// `/compute/soup` regime: "chains" | "sheet" | "tube" | "vesicle".
    morphology: &'static str,
    /// Default particle count, per the egui catalog's `soup_nodes`.
    soup_nodes: u32,
}

const SOUP_EXAMPLES: &[SoupExample] = &[
    SoupExample {
        name: "Lipid chains (self-assembly)",
        description: "Valence-2 @180° bonding on a Brownian soup → spontaneous chains. \
                      Geometric (GPU). Sphere seed.",
        morphology: "chains",
        soup_nodes: 5_000,
    },
    SoupExample {
        name: "Honeycomb sheet (self-assembly)",
        description: "Valence-3 @120° + membrane flattening → spontaneous honeycomb patches. \
                      Geometric (GPU). Sphere seed.",
        morphology: "sheet",
        soup_nodes: 50_000,
    },
    SoupExample {
        name: "Tube (curved sheet)",
        description: "Sheet regime + spontaneous curvature folds a patch into a tube. \
                      Geometric (GPU). Grid seed (start as a flat-ish disc).",
        morphology: "tube",
        soup_nodes: 20_000,
    },
    SoupExample {
        name: "Vesicle (rim seam + curvature)",
        description: "P3 rim line-tension (γ=4) + curvature (c₀=0.5) folds a seeded bonded \
                      disc toward a shell. Geometric (GPU). Grid seed.",
        morphology: "vesicle",
        soup_nodes: 20_000,
    },
];

/// The egui catalog stages a matching soup-generator expression in the editor
/// when an example loads — replicate that staging (`Example::generator_expr`).
fn generator_expr(ex: &SoupExample) -> String {
    format!(
        "# {} — initial particle soup for the dynamic-bonding engine.\n\
         # Evaluate to spawn {} unbonded particles; the Geometric (GPU)\n\
         # engine grows bonds at runtime into the target morphology.\n\
         let\n  \
         g  = import /jc/src/graph.nix {{}};\n  \
         gc = import /jc/src/graph-combinators.nix {{ graph = g; }};\n\
         in\n  \
         g.toGraphJSON (gc.soupGen {{ nodes = {}; prefix = \"s\"; }})\n",
        ex.name, ex.soup_nodes, ex.soup_nodes
    )
}

// --- execution backend ---------------------------------------------------------

/// Port of `GenerateBackendChoice` (ui/state.rs). All four egui choices stay
/// selectable so a persisted/staged pick round-trips, but only the Server path
/// is wired in this build.
// PARITY GAP: Inline and LocalWorker eval (tvix-wasm in the browser / the
// tvix-worker Web Worker) are out of scope here — picking them surfaces an
// inline error instead of evaluating. Auto's "else a local fallback" half is
// likewise unavailable: Auto behaves as Server.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
enum Backend {
    #[default]
    Auto,
    Inline,
    Server,
    LocalWorker,
}

impl Backend {
    fn label(self) -> &'static str {
        match self {
            Backend::Auto => "Auto (server if reachable)",
            Backend::Server => "Server (graph-api)",
            Backend::Inline => "Inline (local)",
            Backend::LocalWorker => "Local worker",
        }
    }
    fn key(self) -> &'static str {
        match self {
            Backend::Auto => "auto",
            Backend::Server => "server",
            Backend::Inline => "inline",
            Backend::LocalWorker => "worker",
        }
    }
    fn from_key(k: &str) -> Self {
        match k {
            "server" => Backend::Server,
            "inline" => Backend::Inline,
            "worker" => Backend::LocalWorker,
            _ => Backend::Auto,
        }
    }
}

const ALL_BACKENDS: [Backend; 4] = [
    Backend::Auto,
    Backend::Server,
    Backend::Inline,
    Backend::LocalWorker,
];

// --- persisted settings ----------------------------------------------------------

const STORE_KEY: &str = "jc_generate_v1";

/// User-facing parameters (the egui app round-trips `editor.source` + `backend`
/// through share links / YAML; localStorage is this app's persistence channel).
/// `error`/`status` stay transient, exactly like `NixEditorState`'s
/// `#[serde(skip)]` fields.
#[derive(Clone, Serialize, Deserialize)]
struct Persisted {
    source: String,
    backend: Backend,
    soup_example: usize,
    soup_n: u32,
    soup_radius: f32,
    soup_seed: u64,
}

impl Default for Persisted {
    fn default() -> Self {
        Persisted {
            source: GENERATE_DEMO_EXPR.to_string(),
            backend: Backend::default(),
            soup_example: 0,
            soup_n: SOUP_EXAMPLES[0].soup_nodes,
            // Server defaults for /compute/soup (`radius` 40, `seed` 1).
            soup_radius: 40.0,
            soup_seed: 1,
        }
    }
}

fn restore() -> Persisted {
    LocalStorage::get(STORE_KEY).unwrap_or_default()
}

fn persist() {
    let p = Persisted {
        source: SOURCE.read().clone(),
        backend: *BACKEND.read(),
        soup_example: *SOUP_EXAMPLE.read(),
        soup_n: *SOUP_N.read(),
        soup_radius: *SOUP_RADIUS.read(),
        soup_seed: *SOUP_SEED.read(),
    };
    let _ = LocalStorage::set(STORE_KEY, &p);
}

// --- panel-local state -------------------------------------------------------------

static SOURCE: GlobalSignal<String> = Signal::global(|| restore().source);
static BACKEND: GlobalSignal<Backend> = Signal::global(|| restore().backend);
static SOUP_EXAMPLE: GlobalSignal<usize> = Signal::global(|| restore().soup_example);
static SOUP_N: GlobalSignal<u32> = Signal::global(|| restore().soup_n);
static SOUP_RADIUS: GlobalSignal<f32> = Signal::global(|| restore().soup_radius);
static SOUP_SEED: GlobalSignal<u64> = Signal::global(|| restore().soup_seed);

/// Transient eval chrome (NixEditorState's `error`/`status`). `RUNNING` is the
/// egui `state.generate.request.is_some()` latch — at most one in-flight eval.
static STATUS: GlobalSignal<Option<String>> = Signal::global(|| None);
static ERROR: GlobalSignal<Option<String>> = Signal::global(|| None);
static RUNNING: GlobalSignal<bool> = Signal::global(|| false);

/// Transient chrome for the /compute/soup runner.
static SOUP_STATUS: GlobalSignal<Option<String>> = Signal::global(|| None);
static SOUP_ERROR: GlobalSignal<Option<String>> = Signal::global(|| None);
static SOUP_RUNNING: GlobalSignal<bool> = Signal::global(|| false);

// --- private endpoint helpers (api.rs stays untouched) ------------------------------

async fn post_json<B: Serialize, T: serde::de::DeserializeOwned>(
    path: &str,
    body: &B,
) -> Result<T, String> {
    Request::post(&url(path))
        .json(body)
        .map_err(err)?
        .send()
        .await
        .map_err(err)?
        .json()
        .await
        .map_err(err)
}

/// `toGraphJSON`'s `{ nodes, links }` shape, as embedded in the /generate
/// response (`GeneratePostResp.graph`). Extra per-node fields (`kind`, …) are
/// ignored — only identity and topology feed the renderer.
#[derive(Clone, Debug, Deserialize)]
struct GeneratedGraph {
    #[serde(default)]
    nodes: Vec<GenNode>,
    #[serde(default)]
    links: Vec<GenLink>,
}

#[derive(Clone, Debug, Deserialize)]
struct GenNode {
    id: String,
}

#[derive(Clone, Debug, Deserialize)]
struct GenLink {
    source: String,
    target: String,
}

/// `POST /generate` — soft-error contract: HTTP 200 with `ok:false` carries the
/// eval message, surfaced inline exactly like the egui local path.
#[derive(Deserialize)]
struct GenerateResp {
    ok: bool,
    #[serde(default)]
    graph: Option<GeneratedGraph>,
    #[serde(default)]
    error: Option<String>,
}

async fn generate(expr: &str) -> Result<GeneratedGraph, String> {
    let resp: GenerateResp =
        post_json("/generate", &serde_json::json!({ "expr": expr })).await?;
    if resp.ok {
        resp.graph
            .ok_or_else(|| "server returned ok without a graph".to_string())
    } else {
        Err(resp.error.unwrap_or_else(|| "evaluation failed".to_string()))
    }
}

/// `POST /compute/soup` — same soft-error contract.
#[derive(Deserialize)]
struct SoupResp {
    ok: bool,
    #[serde(default)]
    n_nodes: u32,
    #[serde(default)]
    error: Option<String>,
}

// --- generated graph -> live canvas (bootstrap_from_generated port) -----------------

/// Weakly-connected component count via union-find over the undirected edge
/// set — ported from `generate.rs::wcc_labels` (the labels themselves have no
/// home here, see the PARITY GAP in `graph_data_from_generated`).
fn wcc_count(n_nodes: usize, edges: &[u32]) -> u32 {
    let mut parent: Vec<u32> = (0..n_nodes as u32).collect();
    fn find(parent: &mut [u32], x: u32) -> u32 {
        let mut root = x;
        while parent[root as usize] != root {
            root = parent[root as usize];
        }
        let mut cur = x;
        while parent[cur as usize] != root {
            let next = parent[cur as usize];
            parent[cur as usize] = root;
            cur = next;
        }
        root
    }
    for pair in edges.chunks_exact(2) {
        let a = find(&mut parent, pair[0]);
        let b = find(&mut parent, pair[1]);
        if a != b {
            parent[a as usize] = b;
        }
    }
    let mut roots = std::collections::HashSet::new();
    for i in 0..n_nodes as u32 {
        roots.insert(find(&mut parent, i));
    }
    roots.len() as u32
}

/// Convert a freshly evaluated graph into this app's `GraphData`, mirroring
/// `bootstrap_from_generated`: dense first-seen `id -> idx` (duplicate ids
/// collapse), edges kept only when both endpoints resolve (missing-endpoint
/// edges silently dropped), positions seeded on the 800-radius sphere shell,
/// `num_communities` 0 (Louvain is server-only) with a real `num_wcc`.
// PARITY GAP: the egui path also derives client-side `degree` + `wcc` metric
// BUFFERS so Style colour-by/size-by works without a server round-trip;
// `GraphData` carries no metrics map, so colors/sizes fall back to defaults
// (which is also what the egui defaults — community/pagerank — resolve to on
// a generated graph).
// PARITY GAP: the egui app seeds positions from the Layout panel's
// Initial-seed strategy (`seed_positions_for`: jitter / built-in / custom Nix
// seed); seed eval is tvix-local, so the sphere shell stands in here.
fn graph_data_from_generated(g: &GeneratedGraph) -> GraphData {
    let mut id_to_idx: HashMap<String, u32> = HashMap::with_capacity(g.nodes.len());
    let mut ids: Vec<String> = Vec::with_capacity(g.nodes.len());
    for node in &g.nodes {
        if !id_to_idx.contains_key(&node.id) {
            id_to_idx.insert(node.id.clone(), ids.len() as u32);
            ids.push(node.id.clone());
        }
    }
    let n = ids.len();

    let mut edges: Vec<u32> = Vec::with_capacity(g.links.len() * 2);
    for e in &g.links {
        let (Some(&s), Some(&t)) = (id_to_idx.get(&e.source), id_to_idx.get(&e.target)) else {
            continue;
        };
        edges.push(s);
        edges.push(t);
    }
    let n_edges = (edges.len() / 2) as u32;

    let positions = render::data::spawn_on_unit_sphere(n, SPAWN_RADIUS);
    let metrics: HashMap<String, Vec<f32>> = HashMap::new();
    let colors = render::data::colors_from_metric("community", &metrics, n);
    let sizes = render::data::sizes_from_metric("pagerank", &metrics, n, 0.5);
    let num_wcc = wcc_count(n, &edges);

    GraphData {
        n_nodes: n as u32,
        n_edges,
        num_communities: 0,
        num_wcc,
        ids,
        id_to_idx,
        scene: render::Scene {
            positions,
            edges,
            colors,
            sizes,
        },
    }
}

// --- panel ---------------------------------------------------------------------------

pub fn panel(ctx: Ctx) -> Element {
    let running = *RUNNING.read();
    let soup_running = *SOUP_RUNNING.read();
    let has_src = !SOURCE.read().trim().is_empty();
    let backend = *BACKEND.read();
    let soup_idx = (*SOUP_EXAMPLE.read()).min(SOUP_EXAMPLES.len() - 1);

    // Evaluate — the egui flow queues a one-shot request and `App::update`
    // dispatches it to the picked `ExecutionBackend`; here the Server backend
    // is the only wired executor, dispatched as an async task off the click.
    let evaluate = move |_| {
        if *RUNNING.read() || SOURCE.read().trim().is_empty() {
            return;
        }
        match *BACKEND.read() {
            Backend::Inline | Backend::LocalWorker => {
                // PARITY GAP: local executors (tvix-wasm inline / tvix-worker)
                // are not bundled in the Dioxus build — Server is the target.
                *ERROR.write() = Some(
                    "local eval (Inline / Local worker) is not available in this \
                     build yet — pick Server (graph-api) or Auto"
                        .to_string(),
                );
                *STATUS.write() = None;
                return;
            }
            Backend::Auto | Backend::Server => {}
        }
        let src = SOURCE.read().clone();
        *RUNNING.write() = true;
        *ERROR.write() = None;
        *STATUS.write() = Some("queued…".to_string());
        let mut graph = ctx.graph;
        spawn(async move {
            *STATUS.write() = Some("evaluating on the server…".to_string());
            match generate(&src).await {
                Ok(g) => {
                    // Replace the live graph client-side (the egui pending →
                    // Bootstrap → GPU promotion path). `/generate` does not
                    // host the result, so a server reload would discard it —
                    // mount the converted scene directly instead.
                    let gd = graph_data_from_generated(&g);
                    *STATUS.write() = Some(format!(
                        "{} nodes, {} edges — replaced the live graph",
                        gd.n_nodes, gd.n_edges
                    ));
                    *ERROR.write() = None;
                    let scene = gd.scene.clone();
                    graph.set(Some(gd));
                    render::mount_canvas(scene);
                }
                Err(e) => {
                    *ERROR.write() = Some(e);
                    *STATUS.write() = None;
                }
            }
            *RUNNING.write() = false;
        });
    };

    // Run the self-assembly demo: host the soup + select Geometric (GPU) with
    // the membrane regime server-side, then reload the hosted graph.
    let assemble = move |_| {
        if *SOUP_RUNNING.read() {
            return;
        }
        let i = (*SOUP_EXAMPLE.read()).min(SOUP_EXAMPLES.len() - 1);
        let ex = &SOUP_EXAMPLES[i];
        let n = *SOUP_N.read();
        if n == 0 || n > MAX_SOUP_NODES {
            *SOUP_ERROR.write() = Some(format!("n must be in 1..={MAX_SOUP_NODES} (got {n})"));
            *SOUP_STATUS.write() = None;
            return;
        }
        let req = serde_json::json!({
            "n": n,
            "radius": *SOUP_RADIUS.read(),
            "seed": *SOUP_SEED.read(),
            "morphology": ex.morphology,
        });
        let name = ex.name;
        *SOUP_RUNNING.write() = true;
        *SOUP_ERROR.write() = None;
        *SOUP_STATUS.write() = Some(format!("{name}: spawning the soup…"));
        spawn(async move {
            match post_json::<_, SoupResp>("/compute/soup", &req).await {
                Ok(r) if r.ok => {
                    *SOUP_STATUS.write() = Some(format!(
                        "{name}: {} particles hosted — Geometric (GPU) assembling; reloading…",
                        r.n_nodes
                    ));
                    // The server now hosts the soup as its active graph.
                    crate::reload_graph(ctx).await;
                    *SOUP_STATUS.write() = Some(format!(
                        "{name}: {} particles — Geometric (GPU) assembling on the worker",
                        r.n_nodes
                    ));
                }
                Ok(r) => {
                    *SOUP_ERROR.write() = Some(r.error.unwrap_or_else(|| "soup failed".into()));
                    *SOUP_STATUS.write() = None;
                }
                Err(e) => {
                    *SOUP_ERROR.write() = Some(e);
                    *SOUP_STATUS.write() = None;
                }
            }
            *SOUP_RUNNING.write() = false;
        });
    };

    rsx! {
        div { class: "gen",
            // ── Example UI-states (self-assembly demos) — examples_picker ──
            div { class: "gen-label", "Examples (self-assembly)" }
            div { class: "gen-hint",
                "Load a full demo: Geometric (GPU) + validated bonding regime + \
                 soup generator + seed. Assemble hosts the soup server-side and \
                 starts bonding; the matching generator expression is staged in \
                 the editor below."
            }
            // PARITY GAP: the egui picker swaps in a complete AppState
            // (camera follow-centroid + fit, style size_mul 0.8, Initial-seed
            // strategy + custom seed source, opens Generate+Layout, preserves
            // the snapshot ring and stamps a timeline entry). Camera/style/
            // seed/panel staging is owned by sibling panels and is not
            // replicated here; the engine + regime selection happens
            // server-side via /compute/soup instead.
            select {
                class: "gen-select",
                value: "{soup_idx}",
                onchange: move |e| {
                    if let Ok(i) = e.value().parse::<usize>() {
                        if let Some(ex) = SOUP_EXAMPLES.get(i) {
                            *SOUP_EXAMPLE.write() = i;
                            *SOUP_N.write() = ex.soup_nodes;
                            // Stage the matching generator expr (egui contract).
                            *SOURCE.write() = generator_expr(ex);
                            *ERROR.write() = None;
                            *STATUS.write() = None;
                            *SOUP_STATUS.write() = None;
                            *SOUP_ERROR.write() = None;
                            persist();
                        }
                    }
                },
                for (i, ex) in SOUP_EXAMPLES.iter().enumerate() {
                    option { key: "{ex.name}", value: "{i}", title: "{ex.description}", "{ex.name}" }
                }
            }
            div { class: "gen-hint", "{SOUP_EXAMPLES[soup_idx].description}" }
            div { class: "gen-params",
                label {
                    "particles"
                    input {
                        r#type: "number",
                        min: "1",
                        max: "{MAX_SOUP_NODES}",
                        value: "{SOUP_N}",
                        oninput: move |e| {
                            if let Ok(v) = e.value().parse::<u32>() {
                                *SOUP_N.write() = v;
                                persist();
                            }
                        },
                    }
                }
                label {
                    title: "half-extent of the initial scatter cube",
                    "radius"
                    input {
                        r#type: "number",
                        min: "1",
                        step: "1",
                        value: "{SOUP_RADIUS}",
                        oninput: move |e| {
                            if let Ok(v) = e.value().parse::<f32>() {
                                *SOUP_RADIUS.write() = v;
                                persist();
                            }
                        },
                    }
                }
                label {
                    title: "deterministic scatter seed",
                    "seed"
                    input {
                        r#type: "number",
                        min: "0",
                        value: "{SOUP_SEED}",
                        oninput: move |e| {
                            if let Ok(v) = e.value().parse::<u64>() {
                                *SOUP_SEED.write() = v;
                                persist();
                            }
                        },
                    }
                }
            }
            button {
                class: "btn",
                disabled: soup_running,
                title: "POST /compute/soup — host the soup and select Geometric (GPU) \
                        with the validated membrane regime, then reload the graph",
                onclick: assemble,
                if soup_running { "assembling…" } else { "Assemble" }
            }
            if let Some(s) = SOUP_STATUS.read().as_ref() {
                div { class: "gen-status", "{s}" }
            }
            if let Some(e) = SOUP_ERROR.read().as_ref() {
                div { class: "gen-error", "{e}" }
            }
            hr { class: "gen-sep" }

            // ── Execution backend picker — backend_picker ──────────────────
            div { class: "gen-label", "Execution backend" }
            div { class: "gen-hint",
                "Where the expression is evaluated. Server (async HTTP to graph-api) \
                 keeps the browser responsive for large graphs. Auto uses Server when \
                 reachable, else a local fallback."
            }
            select {
                class: "gen-select",
                value: "{backend.key()}",
                onchange: move |e| {
                    *BACKEND.write() = Backend::from_key(&e.value());
                    persist();
                },
                for b in ALL_BACKENDS {
                    option {
                        key: "{b.key()}",
                        value: "{b.key()}",
                        title: if b == Backend::LocalWorker {
                            "Offline Web Worker eval. Currently falls back to the local \
                             executor — the trunk worker bundle is feasibility-gated \
                             (see tvix-worker)."
                        } else {
                            ""
                        },
                        "{b.label()}"
                    }
                }
            }
            hr { class: "gen-sep" }

            // ── NixExtension chrome: hint / examples / editor / action ─────
            div { class: "gen-hint",
                "Write a Nix expression that evaluates to toGraphJSON's \
                 {{ nodes = [...]; links = [...]; }} shape. Evaluating replaces \
                 the live graph."
            }
            hr { class: "gen-sep" }
            div { class: "gen-label", "Examples" }
            select {
                class: "gen-select",
                value: "",
                onchange: move |e| {
                    if let Ok(i) = e.value().parse::<usize>() {
                        if let Some(d) = DEMOS.get(i) {
                            *SOURCE.write() = d.expr.to_string();
                            *ERROR.write() = None;
                            *STATUS.write() = None;
                            persist();
                        }
                    }
                },
                option { value: "", disabled: true, selected: true, "Load an example…" }
                for (i, d) in DEMOS.iter().enumerate() {
                    option { key: "{d.name}", value: "{i}", "{d.name}" }
                }
            }
            hr { class: "gen-sep" }
            div { class: "gen-label", "Nix expression" }
            textarea {
                class: "gen-editor",
                rows: "14",
                spellcheck: false,
                placeholder: "import /jc/src/graph.nix {{}} ...",
                value: "{SOURCE}",
                oninput: move |e| {
                    *SOURCE.write() = e.value();
                    persist();
                },
            }
            div { class: "gen-actions",
                button {
                    class: "btn",
                    disabled: !has_src || running,
                    title: "Evaluate the expression and replace the live graph",
                    onclick: evaluate,
                    "Evaluate"
                }
            }
            if let Some(s) = STATUS.read().as_ref() {
                div { class: "gen-status", "{s}" }
            }
            if let Some(e) = ERROR.read().clone() {
                hr { class: "gen-sep" }
                div { class: "gen-label", "Evaluation error" }
                div { class: "gen-error",
                    for (i, line) in e.lines().enumerate() {
                        div { key: "{i}", "{line}" }
                    }
                }
            }
        }
    }
}
