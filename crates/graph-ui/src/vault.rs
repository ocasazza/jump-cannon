use std::path::PathBuf;
use bevy::prelude::*;
use vault_data::VaultGraph;

#[derive(Resource, Default)]
pub struct VaultGraphResource {
    pub graph: VaultGraph,
    pub vault_root: PathBuf,
    pub loaded: bool,
}

pub fn load_vault_system(mut res: ResMut<VaultGraphResource>) {
    if res.loaded { return; }

    let root = if res.vault_root.as_os_str().is_empty() {
        // Default: look for vault/ sibling of the executable, else cwd
        let cwd = std::env::current_dir().unwrap_or_default();
        let candidate = cwd.join("vault");
        if candidate.is_dir() { candidate } else { cwd }
    } else {
        res.vault_root.clone()
    };

    info!("Loading vault from {:?}", root);
    let result = vault_links::extract_vault(&root);
    let mut graph = result.graph;
    graph_metrics::compute_all(&mut graph);

    info!(
        "Vault loaded: {} nodes, {} edges, {} communities",
        graph.node_count(), graph.edge_count(), graph.num_communities
    );

    res.graph = graph;
    res.loaded = true;
}
