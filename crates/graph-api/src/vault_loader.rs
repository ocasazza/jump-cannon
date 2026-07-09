//! Graph load + metric compute. Runs at startup and on every watched-fs
//! reload.
//!
//! Generic over [`data_loader::Loader`] — the concrete loader (Obsidian vault,
//! tvix expression, etc.) is selected at startup and the rest of the pipeline
//! (metrics, position seeding, binary caches) is loader-agnostic.
//
// Future: incremental reload — re-extract only changed files via mtime
// tracking. Today every reload is a full re-walk.

use std::sync::Arc;

use data_loader::Loader;
use vault_data::VaultGraph;

use crate::progress::ProgressLog;

/// Load a graph through any [`Loader`], compute metrics, and seed initial
/// positions. Convenience wrapper for callers that don't want a progress feed.
pub fn load(loader: &dyn Loader) -> VaultGraph {
    load_with_progress(loader, None)
}

/// Like [`load`] but emits per-stage progress into a [`ProgressLog`].
/// Stages: "Scanning vault" (or "Evaluating tvix"), "Computing metrics",
/// "Seeding positions".
pub fn load_with_progress(
    loader: &dyn Loader,
    progress: Option<&Arc<ProgressLog>>,
) -> VaultGraph {
    let source_name = loader.name();
    tracing::info!(source = %source_name, "loading graph");

    let scan_label = match source_name {
        "obsidian" => "Scanning vault",
        "tvix" => "Evaluating tvix expression",
        other => other,
    };
    let scan_id = progress.map(|p| p.start("ingest", scan_label));

    let result = loader.load();
    let mut graph = result.graph;

    if let (Some(p), Some(id)) = (progress, scan_id) {
        p.update_label(
            id,
            format!(
                "Loaded: {} nodes / {} edges",
                graph.node_count(),
                graph.edge_count()
            ),
        );
        p.finish(id);
    }

    if !result.unresolved.is_empty() {
        tracing::warn!(
            n_nodes = graph.node_count(),
            n_edges = graph.edge_count(),
            unresolved = result.unresolved.len(),
            "graph loaded with unresolved references"
        );
    } else {
        tracing::info!(
            n_nodes = graph.node_count(),
            n_edges = graph.edge_count(),
            "graph loaded; computing metrics"
        );
    }

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