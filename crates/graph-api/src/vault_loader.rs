//! Vault load + metric compute. Runs at startup and on every watched-fs
//! reload.
//
// Future: incremental reload — re-extract only changed files via mtime
// tracking. Today every reload is a full re-walk.

use std::path::Path;
use std::sync::Arc;

use vault_data::VaultGraph;

use crate::progress::ProgressLog;

/// Walk the vault, build the graph, populate metrics. Returns the populated
/// graph. Convenience wrapper for callers that don't want a progress feed.
pub fn load(vault_root: &Path) -> VaultGraph {
    load_with_progress(vault_root, None)
}

/// Like [`load`] but emits per-stage progress into a [`ProgressLog`].
/// Stages: "Scanning vault", "Computing metrics", "Seeding positions".
pub fn load_with_progress(vault_root: &Path, progress: Option<&Arc<ProgressLog>>) -> VaultGraph {
    tracing::info!(vault_root = %vault_root.display(), "extracting vault");
    let scan_id = progress.map(|p| p.start("ingest", "Scanning vault"));

    let result = vault_links::extract_vault(vault_root);
    let mut graph = result.graph;

    if let (Some(p), Some(id)) = (progress, scan_id) {
        p.update_label(
            id,
            format!(
                "Scanned vault: {} nodes / {} edges",
                graph.node_count(),
                graph.edge_count()
            ),
        );
        p.finish(id);
    }

    tracing::info!(
        n_nodes = graph.node_count(),
        n_edges = graph.edge_count(),
        unresolved = result.unresolved.len(),
        "vault extracted; computing metrics"
    );

    let metrics_id = progress.map(|p| p.start("ingest", "Computing graph metrics"));
    graph_metrics::compute_all(&mut graph);
    if let (Some(p), Some(id)) = (progress, metrics_id) {
        p.finish(id);
    }

    // Seed deterministic initial positions on a circle.
    let seed_id = progress.map(|p| p.start("ingest", "Seeding layout positions"));
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
    if let (Some(p), Some(id)) = (progress, seed_id) {
        p.finish(id);
    }

    tracing::info!(
        num_communities = graph.num_communities,
        num_wcc = graph.num_wcc,
        "metrics computed"
    );

    graph
}
