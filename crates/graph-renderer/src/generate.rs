//! Client-side graph generation: turn a tvix-evaluated [`GeneratedGraph`]
//! into a renderable [`data::Bootstrap`].
//!
//! `tvix-wasm` evaluates a user Nix expression into a `GeneratedGraph`
//! (`{ nodes: [{ id, kind? }], edges: [{ source, target }] }`, a typed
//! projection of `toGraphJSON`'s `{ nodes, links }` shape). That type knows
//! nothing about the renderer — node ids are arbitrary strings and edges are
//! string id pairs. This module is the explicit adapter that bridges into the
//! renderer's GPU bootstrap world:
//!
//!   * dense node indices via a stable `id -> idx` map (first-seen order),
//!   * edges rewritten as `Vec<u32>` index pairs `[s, t, s, t, ...]`,
//!   * positions seeded with [`data::spawn_on_unit_sphere`],
//!   * a synthesized [`proto::Init`] (`n_nodes`, `n_edges`, default palette),
//!   * client-side metric buffers — `degree` (continuous) and `wcc`
//!     (weakly-connected component, categorical) — computed here so a generated
//!     graph supports colour-by / size-by without a server round-trip.
//!
//! The dependency direction is one-way: `graph-renderer -> tvix-wasm`.
//! `tvix-wasm` never depends on `graph-renderer`, so there is no cycle and the
//! evaluator stays usable headless.

use crate::data::{self, Bootstrap};
use crate::proto;
use crate::ui::state::SeedStrategy;
use tvix_wasm::GeneratedGraph;

/// Radius of the seed sphere shell, matching the network-bootstrap path in
/// `app.rs` so a generated graph fits the camera the same way.
const SPAWN_RADIUS: f32 = 800.0;

/// Convert a [`GeneratedGraph`] into a [`Bootstrap`] ready to hand to
/// `GraphPipelines::load` (via `LoadState::Ready`).
///
/// Node identity: nodes keep their string `id`. Indices are assigned in
/// first-seen order over `graph.nodes`; the returned `Bootstrap.ids` is that
/// dense `idx -> id` table. Duplicate node ids collapse to the first
/// occurrence (a later node with an already-seen id is dropped) so the index
/// space stays dense and unambiguous.
///
/// Edges: an edge is kept only when **both** endpoints resolve to a known node
/// id; an edge naming a missing endpoint is silently dropped (generated graphs
/// can reference ids they never declared, and dropping is friendlier than
/// failing the whole evaluation). Each kept edge contributes two `u32`s
/// `[source_idx, target_idx]` to `Bootstrap.edges`.
///
/// Positions: seeded on a sphere shell via [`data::spawn_on_unit_sphere`]
/// (`3 * n_nodes` floats). The force sim takes over within a few frames.
///
/// Metrics: generated graphs get no server-computed metrics (PageRank, Louvain
/// community, …), but two are cheap to derive from the topology here and make
/// the Style panel's colour-by / size-by useful immediately: `degree`
/// (continuous) and `wcc` (weakly-connected component id, categorical — keyed
/// the same way the server path keys it, so it cycles the palette). Other metric
/// keys remain absent and the renderer's style code falls back to defaults.
pub fn bootstrap_from_generated(graph: &GeneratedGraph) -> Bootstrap {
    // Dense id -> idx, first-seen order. Dedup collapses repeated ids.
    let mut id_to_idx: std::collections::HashMap<&str, u32> =
        std::collections::HashMap::with_capacity(graph.nodes.len());
    let mut ids: Vec<String> = Vec::with_capacity(graph.nodes.len());
    for n in &graph.nodes {
        if !id_to_idx.contains_key(n.id.as_str()) {
            let idx = ids.len() as u32;
            id_to_idx.insert(n.id.as_str(), idx);
            ids.push(n.id.clone());
        }
    }

    let n_nodes = ids.len();

    // Edges as index pairs; drop any edge whose endpoint is unknown.
    let mut edges: Vec<u32> = Vec::with_capacity(graph.edges.len() * 2);
    for e in &graph.edges {
        let (Some(&s), Some(&t)) = (
            id_to_idx.get(e.source.as_str()),
            id_to_idx.get(e.target.as_str()),
        ) else {
            continue;
        };
        edges.push(s);
        edges.push(t);
    }
    let n_edges = (edges.len() / 2) as u32;

    let positions = data::spawn_on_unit_sphere(n_nodes, SPAWN_RADIUS);

    // Client-side metrics derived from the topology (no server round-trip).
    let degree = degrees(n_nodes, &edges);
    let (wcc, num_wcc) = wcc_labels(n_nodes, &edges);
    let mut metrics: std::collections::HashMap<String, Vec<f32>> =
        std::collections::HashMap::new();
    metrics.insert("degree".to_string(), degree);
    metrics.insert("wcc".to_string(), wcc);

    // Synthesize a minimal Init. Palette matches the renderer's default
    // (Tableau20) so canvas swatches line up with the rest of the app. No
    // community partition is computed client-side (Louvain is server-only), but
    // wcc is, so `num_wcc` reflects the real component count.
    let palette: Vec<f32> = data::palette_table(data::PaletteId::Tableau20)
        .iter()
        .flat_map(|rgb| rgb.iter().copied())
        .collect();
    let init = proto::Init {
        n_nodes: n_nodes as u32,
        n_edges,
        num_communities: 0,
        num_wcc,
        palette,
    };

    Bootstrap {
        init: Some(init),
        ids,
        positions,
        edges,
        metrics,
    }
}

/// Minimal "No seed" placement for a freshly generated graph: a small
/// deterministic jitter so nodes aren't coincident (degenerate), WITHOUT the big
/// pre-spread sphere — the force sim builds the layout from here. Radius is a few
/// units, growing slowly with `n` so a large graph isn't pathologically dense.
fn jitter_positions(n: usize) -> Vec<f32> {
    let r = 2.0 + (n as f32).max(1.0).cbrt();
    let mut out = vec![0.0f32; 3 * n];
    for (i, slot) in out.iter_mut().enumerate() {
        // SplitMix64 finaliser on the index → deterministic unit in [0,1).
        let mut z = (i as u64).wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        let unit = (z >> 40) as f32 / (1u64 << 24) as f32;
        *slot = (unit * 2.0 - 1.0) * r;
    }
    out
}

/// Resolve the INITIAL positions for a freshly generated graph of `n` nodes from
/// the active Initial-seed `strategy` (Layout panel) — instead of always applying
/// the default sphere shell. This is what makes "No seed" actually mean *no
/// pre-arranged seed* on generation:
///   - `None` → a minimal jitter (the sim arranges from there),
///   - `BuiltIn(i)` → the embedded `seed_demos()[i]` strategy via `eval_seed`,
///   - `Custom` → the user's Nix seed expression via `eval_seed`.
/// Any eval failure / wrong-length result falls back to the jitter.
pub fn seed_positions_for(strategy: &SeedStrategy, custom_source: &str, n: usize) -> Vec<f32> {
    let expr: Option<String> = match strategy {
        SeedStrategy::None => None,
        SeedStrategy::BuiltIn(i) => {
            tvix_wasm::seed_demos().get(*i).map(|d| d.expr.to_string())
        }
        SeedStrategy::Custom => Some(custom_source.to_string()),
    };
    match expr {
        None => jitter_positions(n),
        Some(src) => match tvix_wasm::eval_seed(&src, n) {
            Ok(p) if p.len() == n => p.into_iter().flatten().collect(),
            _ => jitter_positions(n),
        },
    }
}

/// Per-node undirected degree as an `f32` metric buffer (each edge increments
/// both endpoints).
fn degrees(n_nodes: usize, edges: &[u32]) -> Vec<f32> {
    let mut d = vec![0.0f32; n_nodes];
    for pair in edges.chunks_exact(2) {
        d[pair[0] as usize] += 1.0;
        d[pair[1] as usize] += 1.0;
    }
    d
}

/// Weakly-connected-component label per node — dense `[0, k)` ids as `f32` plus
/// the component count `k` — via union-find over the undirected edge set.
/// Isolated nodes each form their own component.
fn wcc_labels(n_nodes: usize, edges: &[u32]) -> (Vec<f32>, u32) {
    let mut parent: Vec<u32> = (0..n_nodes as u32).collect();
    fn find(parent: &mut [u32], x: u32) -> u32 {
        let mut root = x;
        while parent[root as usize] != root {
            root = parent[root as usize];
        }
        // Path compression.
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
    // Compact roots to dense [0, k) in first-seen order.
    let mut remap: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
    let mut labels = vec![0.0f32; n_nodes];
    let mut next = 0u32;
    for i in 0..n_nodes as u32 {
        let root = find(&mut parent, i);
        let id = *remap.entry(root).or_insert_with(|| {
            let v = next;
            next += 1;
            v
        });
        labels[i as usize] = id as f32;
    }
    (labels, next)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tvix_wasm::eval_graph;

    /// A star graph through the embedded combinator library + toGraphJSON:
    /// center `n0` plus 4 spokes, 4 hub->spoke edges. `graph-combinators.nix`
    /// takes `{ graph }` explicitly (the tvix closure workaround).
    const STAR_EXPR: &str = r#"
        let
          g  = import /jc/src/graph.nix {};
          gc = import /jc/src/graph-combinators.nix { graph = g; };
        in g.toGraphJSON (gc.starGen { nodes = 5; prefix = "n"; })
    "#;

    #[test]
    fn star_demo_bootstrap_shape() {
        let graph = eval_graph(STAR_EXPR).expect("star eval should succeed");
        let bs = bootstrap_from_generated(&graph);

        let init = bs.init.as_ref().expect("init synthesized");
        assert_eq!(init.n_nodes, 5);
        assert_eq!(init.n_edges, 4);

        assert_eq!(bs.ids.len(), 5);
        assert_eq!(bs.positions.len(), 5 * 3);
        assert_eq!(bs.edges.len(), 4 * 2);

        // Client-side metrics: degree (hub=4, each spoke=1) and a single wcc.
        let n0_pos = bs.ids.iter().position(|s| s == "n0").unwrap();
        let degree = bs.metrics.get("degree").expect("degree metric present");
        assert_eq!(degree.len(), 5);
        assert_eq!(degree[n0_pos], 4.0, "hub degree");
        assert_eq!(degree.iter().filter(|&&d| d == 1.0).count(), 4, "four spokes deg 1");
        let wcc = bs.metrics.get("wcc").expect("wcc metric present");
        assert!(wcc.iter().all(|&c| c == 0.0), "star is one connected component");
        assert_eq!(bs.init.as_ref().unwrap().num_wcc, 1);

        // The center node "n0" gets index 0 (first-seen), and every edge in a
        // star originates from the center -> its source index is 0.
        let n0_idx = bs
            .ids
            .iter()
            .position(|s| s == "n0")
            .expect("n0 present") as u32;
        for pair in bs.edges.chunks_exact(2) {
            assert_eq!(pair[0], n0_idx, "every star edge sources from the hub");
            assert!((pair[1] as usize) < bs.ids.len(), "target index in range");
            assert_ne!(pair[1], n0_idx, "spoke is not the hub");
        }

        // The set of target indices is exactly the 4 non-hub nodes.
        let mut targets: Vec<u32> = bs.edges.chunks_exact(2).map(|p| p[1]).collect();
        targets.sort_unstable();
        targets.dedup();
        assert_eq!(targets.len(), 4, "four distinct spokes");
    }

    #[test]
    fn inline_graph_exact_index_pairs() {
        // Hand-built toGraphJSON-shaped attrset with a known layout:
        //   a -> b, b -> c, a -> c. Node order a, b, c => idx 0, 1, 2.
        let expr = r#"{
            nodes = [ { id = "a"; } { id = "b"; } { id = "c"; } ];
            links = [
              { source = "a"; target = "b"; }
              { source = "b"; target = "c"; }
              { source = "a"; target = "c"; }
            ];
        }"#;
        let graph = eval_graph(expr).expect("inline graph parses");
        let bs = bootstrap_from_generated(&graph);

        assert_eq!(bs.ids, vec!["a", "b", "c"]);
        // (0,1), (1,2), (0,2)
        assert_eq!(bs.edges, vec![0, 1, 1, 2, 0, 2]);
        assert_eq!(bs.init.as_ref().unwrap().n_edges, 3);
    }

    #[test]
    fn missing_endpoint_edge_is_dropped() {
        // "z" is never declared as a node; the b->z edge must be dropped while
        // the valid a->b edge survives.
        let expr = r#"{
            nodes = [ { id = "a"; } { id = "b"; } ];
            links = [
              { source = "a"; target = "b"; }
              { source = "b"; target = "z"; }
            ];
        }"#;
        let graph = eval_graph(expr).expect("inline graph parses");
        let bs = bootstrap_from_generated(&graph);

        assert_eq!(bs.ids, vec!["a", "b"]);
        assert_eq!(bs.edges, vec![0, 1], "only the a->b edge survives");
        assert_eq!(bs.init.as_ref().unwrap().n_edges, 1);
    }

    #[test]
    fn duplicate_node_id_collapses() {
        // Repeated id "a" collapses to one node; the duplicate-targeting edge
        // resolves to the first-seen index.
        let expr = r#"{
            nodes = [ { id = "a"; } { id = "b"; } { id = "a"; } ];
            links = [ { source = "a"; target = "b"; } ];
        }"#;
        let graph = eval_graph(expr).expect("inline graph parses");
        let bs = bootstrap_from_generated(&graph);

        assert_eq!(bs.ids, vec!["a", "b"], "duplicate id collapsed");
        assert_eq!(bs.edges, vec![0, 1]);
    }

    #[test]
    fn wcc_metric_separates_components() {
        // Two disjoint edges a-b and c-d => two weakly-connected components.
        let expr = r#"{
            nodes = [ { id = "a"; } { id = "b"; } { id = "c"; } { id = "d"; } ];
            links = [
              { source = "a"; target = "b"; }
              { source = "c"; target = "d"; }
            ];
        }"#;
        let graph = eval_graph(expr).expect("inline graph parses");
        let bs = bootstrap_from_generated(&graph);

        let wcc = bs.metrics.get("wcc").expect("wcc present");
        assert_eq!(bs.init.as_ref().unwrap().num_wcc, 2, "two components");
        // a,b share a label; c,d share a different label.
        assert_eq!(wcc[0], wcc[1], "a,b same component");
        assert_eq!(wcc[2], wcc[3], "c,d same component");
        assert_ne!(wcc[0], wcc[2], "the two edges are separate components");

        // Every node has degree 1 here.
        let degree = bs.metrics.get("degree").unwrap();
        assert!(degree.iter().all(|&d| d == 1.0));
    }

    #[test]
    fn no_seed_generates_minimal_jitter_not_the_big_sphere() {
        // The bug: generation always applied an 800-radius sphere regardless of
        // the Initial-seed strategy. "No seed" must NOT impose that — a minimal
        // jitter near the origin instead, with distinct (non-coincident) nodes.
        let n = 64;
        let pos = seed_positions_for(&SeedStrategy::None, "", n);
        assert_eq!(pos.len(), 3 * n);
        let max_abs = pos.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(max_abs < 50.0, "No-seed jitter must be small, not the 800 sphere (max {max_abs})");
        // Non-degenerate: not all nodes at the same point.
        assert!(pos[0..3] != pos[3..6], "jittered nodes must be distinct");
    }

    #[test]
    fn builtin_seed_strategy_drives_generated_positions() {
        // A chosen built-in seed (sphere) places the generated nodes via eval_seed,
        // producing n positions at a real (non-jitter) radius.
        let sphere_idx = tvix_wasm::seed_demos()
            .iter()
            .position(|d| d.name.to_lowercase().contains("sphere"))
            .expect("a sphere seed demo exists");
        let n = 48;
        let pos = seed_positions_for(&SeedStrategy::BuiltIn(sphere_idx), "", n);
        assert_eq!(pos.len(), 3 * n);
        let max_abs = pos.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(max_abs > 50.0, "the sphere seed should place nodes at its real radius (max {max_abs})");
    }

    #[test]
    fn empty_graph_is_well_formed() {
        let expr = r#"{ nodes = []; links = []; }"#;
        let graph = eval_graph(expr).expect("empty graph parses");
        let bs = bootstrap_from_generated(&graph);
        assert_eq!(bs.ids.len(), 0);
        assert!(bs.positions.is_empty());
        assert!(bs.edges.is_empty());
        assert_eq!(bs.init.as_ref().unwrap().n_nodes, 0);
        assert_eq!(bs.init.as_ref().unwrap().n_edges, 0);
    }
}
