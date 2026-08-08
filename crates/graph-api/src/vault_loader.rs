//! Graph load + metric compute. Runs at startup and on every watched-fs
//! reload.
//!
//! Generic over [`data_loader::Importer`] — the concrete source/decoder/mapper
//! pipeline is selected at startup and the rest of the graph materialization
//! path (metrics, position seeding, binary caches) is importer-agnostic.
//
// Future: incremental reload — re-extract only changed files via mtime
// tracking. Today every reload is a full re-walk.

use std::sync::Arc;

use data_loader::LoadResult;
use data_loader::{ImportError, Importer};

use crate::progress::ProgressLog;

/// Load a graph through any [`Importer`], compute metrics, and seed initial
/// positions. Convenience wrapper for callers that don't want a progress feed.
pub async fn load(importer: &dyn Importer) -> Result<LoadResult, ImportError> {
    load_with_progress(importer, None).await
}

/// Like [`load`] but emits per-stage progress into a [`ProgressLog`].
/// Stages identify the selected importer, metric computation, and position
/// seeding.
pub async fn load_with_progress(
    importer: &dyn Importer,
    progress: Option<&Arc<ProgressLog>>,
) -> Result<LoadResult, ImportError> {
    let descriptor = importer.descriptor();
    let source_name = descriptor.id.as_str();
    tracing::info!(source = %source_name, "loading graph");

    let scan_label = match source_name {
        "obsidian" => "Scanning vault",
        "tvix" => "Evaluating tvix expression",
        _ => descriptor.name.as_str(),
    };
    let scan_id = progress.map(|p| p.start("ingest", scan_label));

    let result = match importer.import().await {
        Ok(result) => result,
        Err(error) => {
            if let (Some(progress), Some(id)) = (progress, scan_id) {
                progress.fail(id, error.to_string());
            }
            return Err(error);
        }
    };
    descriptor.schema.validate_result(&result)?;
    let data_loader::LoadResult {
        mut graph,
        search_documents,
        unresolved,
    } = result;

    if let Err(error) = graph.validate() {
        let error = ImportError::Map {
            message: format!("importer {source_name:?} produced an invalid graph: {error}"),
        };
        if let (Some(progress), Some(id)) = (progress, scan_id) {
            progress.fail(id, error.to_string());
        }
        return Err(error);
    }

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

    if !unresolved.is_empty() {
        tracing::warn!(
            n_nodes = graph.node_count(),
            n_edges = graph.edge_count(),
            unresolved = unresolved.len(),
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
    let metrics_result = tokio::task::spawn_blocking(move || {
        graph_metrics::compute_all(&mut graph);
        graph
    })
    .await;
    let mut graph = match metrics_result {
        Ok(graph) => graph,
        Err(error) => {
            let error = ImportError::Map {
                message: format!("graph metric computation panicked: {error}"),
            };
            if let (Some(progress), Some(id)) = (progress, metrics_id) {
                progress.fail(id, error.to_string());
            }
            return Err(error);
        }
    };
    if let (Some(p), Some(id)) = (progress, metrics_id) {
        p.finish(id);
    }

    // Seed deterministic initial positions on a circle.
    let seed_id = progress.map(|p| p.start("ingest", "Seeding layout positions"));
    let seed_result = tokio::task::spawn_blocking(move || {
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
        graph
    })
    .await;
    let graph = match seed_result {
        Ok(graph) => graph,
        Err(error) => {
            let error = ImportError::Map {
                message: format!("initial position seeding panicked: {error}"),
            };
            if let (Some(progress), Some(id)) = (progress, seed_id) {
                progress.fail(id, error.to_string());
            }
            return Err(error);
        }
    };
    if let (Some(p), Some(id)) = (progress, seed_id) {
        p.finish(id);
    }

    tracing::info!(
        num_communities = graph.num_communities,
        num_wcc = graph.num_wcc,
        "metrics computed"
    );

    Ok(LoadResult {
        graph,
        search_documents,
        unresolved,
    })
}

#[cfg(test)]
mod tests {
    use data_loader::{
        Capability, DiscoveryField, DiscoveryFieldType, EdgeTypeSchema, Effect, ImportFuture,
        ImporterDescriptor, ImporterSchema, LoadResult, SearchDocument, TagHierarchySchema,
        Transport,
    };
    use vault_data::VaultGraph;
    use vault_data::{VaultEdge, VaultNode};

    use super::*;

    struct DanglingGraphImporter;

    fn test_schema() -> ImporterSchema {
        ImporterSchema::new(
            vec![
                DiscoveryField::new("id", DiscoveryFieldType::Keyword, true).searchable(2),
                DiscoveryField::new("title", DiscoveryFieldType::Text, true).searchable(4),
                DiscoveryField::new("tags", DiscoveryFieldType::KeywordList, true)
                    .searchable(2)
                    .facetable(),
            ],
            vec![EdgeTypeSchema::directed("reference", "test edge")],
            TagHierarchySchema::slash(),
        )
    }

    impl Importer for DanglingGraphImporter {
        fn descriptor(&self) -> ImporterDescriptor {
            ImporterDescriptor::new(
                "dangling",
                "Dangling test graph",
                "1",
                vec![Capability::new(Effect::Read, Transport::InMemory, "test")],
                test_schema(),
            )
        }

        fn import<'a>(&'a self) -> ImportFuture<'a, Result<LoadResult, ImportError>> {
            Box::pin(async {
                let mut graph = VaultGraph::new();
                graph.add_node(VaultNode {
                    id: "present".into(),
                    meta: vault_data::NodeMeta {
                        source_id: "dangling".into(),
                        title: "Present".into(),
                        ..Default::default()
                    },
                    ..Default::default()
                });
                graph.add_edge(VaultEdge {
                    source: "present".into(),
                    target: "missing".into(),
                });
                Ok(LoadResult {
                    graph,
                    search_documents: vec![SearchDocument::new("present")
                        .with("id", "present")
                        .with("title", "Present")
                        .with("tags", serde_json::json!([]))],
                    unresolved: Vec::new(),
                })
            })
        }
    }

    #[tokio::test]
    async fn rejects_invalid_importer_graph_before_metrics() {
        let error = load(&DanglingGraphImporter).await.unwrap_err();

        assert!(matches!(
            error,
            ImportError::Map { message }
                if message.contains("invalid graph")
                    && message.contains("edge 0")
                    && message.contains("missing")
        ));
    }
}
