//! Vault load + metric compute. Runs once at startup.
//
// Future: incremental reload — re-extract only changed files via mtime
// tracking.

use std::path::Path;

use vault_data::VaultGraph;

/// Walk the vault, build the graph, populate metrics. Returns the populated
/// graph.
pub fn load(vault_root: &Path) -> VaultGraph {
    tracing::info!(vault_root = %vault_root.display(), "extracting vault");
    let result = vault_links::extract_vault(vault_root);
    let mut graph = result.graph;

    tracing::info!(
        n_nodes = graph.node_count(),
        n_edges = graph.edge_count(),
        unresolved = result.unresolved.len(),
        "vault extracted; computing metrics"
    );

    graph_metrics::compute_all(&mut graph);

    // Seed deterministic-ish initial positions on a circle. Cosmograph will
    // immediately push these around with its WebGL2 force sim, but giving
    // it non-zero starts avoids the "all-NaN at the origin" first frame.
    let n = graph.node_count();
    if n > 0 {
        let radius = 200.0_f32 + (n as f32).sqrt() * 4.0;
        let step = std::f32::consts::TAU / n as f32;
        for (i, (_, node)) in graph.nodes.iter_mut().enumerate() {
            let theta = i as f32 * step;
            node.x = radius * theta.cos();
            node.y = radius * theta.sin();
        }
    }

    tracing::info!(
        num_communities = graph.num_communities,
        num_wcc = graph.num_wcc,
        "metrics computed"
    );

    graph
}
