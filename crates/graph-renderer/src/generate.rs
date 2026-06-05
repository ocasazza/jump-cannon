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
//!   * a synthesized [`proto::Init`] (`n_nodes`, `n_edges`, default palette).
//!
//! The dependency direction is one-way: `graph-renderer -> tvix-wasm`.
//! `tvix-wasm` never depends on `graph-renderer`, so there is no cycle and the
//! evaluator stays usable headless.

use crate::data::{self, Bootstrap};
use crate::proto;
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
/// `metrics` is empty (no server-side metric buffers for a generated graph);
/// the renderer's style code already falls back to defaults when a metric is
/// absent.
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

    // Synthesize a minimal Init. Palette matches the renderer's default
    // (Tableau20) so canvas swatches line up with the rest of the app; there
    // are no community/wcc partitions for a freshly generated graph.
    let palette: Vec<f32> = data::palette_table(data::PaletteId::Tableau20)
        .iter()
        .flat_map(|rgb| rgb.iter().copied())
        .collect();
    let init = proto::Init {
        n_nodes: n_nodes as u32,
        n_edges,
        num_communities: 0,
        num_wcc: 0,
        palette,
    };

    Bootstrap {
        init: Some(init),
        ids,
        positions,
        edges,
        metrics: std::collections::HashMap::new(),
    }
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
        assert!(bs.metrics.is_empty());

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
