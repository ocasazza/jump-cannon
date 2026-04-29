use bevy::prelude::*;
use crate::vault::VaultGraphResource;

/// Marker component for a graph node entity
#[derive(Component)]
pub struct GraphNode {
    pub id: String,
}

/// Marker for a graph edge (data carrier for the render system)
#[derive(Component)]
pub struct GraphEdge {
    pub source: String,
    pub target: String,
}

/// Simulation / layout parameters
#[derive(Resource)]
pub struct SimParams {
    pub repulsion: f32,
    pub spring_len: f32,
    pub spring_k: f32,
    pub damping: f32,
    pub running: bool,
    /// True once the fCoSE layout has been computed and applied
    pub layout_done: bool,
}

impl Default for SimParams {
    fn default() -> Self {
        Self {
            repulsion: 4500.0,
            spring_len: 80.0,
            spring_k: 0.05,
            damping: 0.85,
            running: true,
            layout_done: false,
        }
    }
}

/// Spawn Bevy entities for every vault node (called once after vault is loaded).
/// Nodes start at the origin; `run_fcose_layout` positions them on the next frame.
pub fn spawn_graph(
    mut commands: Commands,
    vault: Res<VaultGraphResource>,
    existing: Query<Entity, With<GraphNode>>,
) {
    if !vault.loaded { return; }
    // Only spawn once
    if !existing.is_empty() { return; }

    let n = vault.graph.node_count();
    if n == 0 { return; }

    for (id, node) in vault.graph.nodes.iter() {
        let [r, g, b] = vault_data::color::community_color(node.metrics.community);
        let color = Color::srgb(r, g, b);
        let size = 8.0 + node.metrics.degree.min(20) as f32 * 0.5;

        commands.spawn((
            GraphNode { id: id.clone() },
            Transform::from_translation(Vec3::ZERO),
            Sprite {
                color,
                custom_size: Some(Vec2::splat(size)),
                ..default()
            },
        ));
    }

    // Spawn edge markers (invisible data carriers for the render system)
    for edge in &vault.graph.edges {
        commands.spawn(GraphEdge {
            source: edge.source.clone(),
            target: edge.target.clone(),
        });
    }
}

/// Run fCoSE layout once after the graph has been spawned.
/// Uses the native `graph_layouts` API directly (no wasm_bindgen) to compute
/// positions, then writes them back into each `GraphNode`'s `Transform`.
pub fn run_fcose_layout(
    vault: Res<VaultGraphResource>,
    mut params: ResMut<SimParams>,
    mut nodes: Query<(&GraphNode, &mut Transform)>,
) {
    if params.layout_done { return; }
    if !vault.loaded { return; }
    // Wait until nodes have actually been spawned
    if nodes.is_empty() { return; }

    let n = vault.graph.node_count();

    if n <= 500 {
        // Small graph: run fCoSE for quality layout
        use graph_layouts::{Graph, Node, Edge, FcoseOptions, LayoutOptions};
        let mut graph = Graph::new();
        for id in vault.graph.nodes.keys() {
            graph.add_node(Node::new(id.clone()));
        }
        for (i, edge) in vault.graph.edges.iter().enumerate() {
            graph.add_edge(Edge::new(format!("e{}", i), edge.source.clone(), edge.target.clone()));
        }
        let options = FcoseOptions {
            base: LayoutOptions { padding: 30 },
            quality: "default".to_string(),
            node_repulsion: params.repulsion as f64,
            ideal_edge_length: params.spring_len as f64,
            node_overlap: 10.0,
        };
        match graph_layouts::run_fcose_layout_native(&mut graph, &options) {
            Ok(()) => {
                let scale = (n as f32).sqrt() * 8.0;
                for (graph_node, mut transform) in nodes.iter_mut() {
                    if let Some(node) = graph.nodes.get(&graph_node.id) {
                        if let Some((x, y)) = node.position {
                            transform.translation = Vec3::new(x as f32 * scale, y as f32 * scale, 0.0);
                        }
                    }
                }
            }
            Err(e) => warn!("fCoSE layout error: {}", e),
        }
    } else {
        // Large graph: community-grouped radial layout — O(n), instant
        // Inner ring per community, communities arranged in a large circle
        let world_radius = (n as f32).sqrt() * 30.0;
        let num_communities = vault.graph.num_communities.max(1);

        // Count nodes per community
        let mut community_counts = vec![0usize; num_communities];
        for node in vault.graph.nodes.values() {
            let c = node.metrics.community.min(num_communities - 1);
            community_counts[c] += 1;
        }
        let mut community_offsets = vec![0usize; num_communities];

        for (graph_node, mut transform) in nodes.iter_mut() {
            if let Some(vault_node) = vault.graph.nodes.get(&graph_node.id) {
                let c = vault_node.metrics.community.min(num_communities - 1);
                let count = community_counts[c].max(1);
                let offset = community_offsets[c];
                community_offsets[c] += 1;

                // Community center on a big circle
                let community_angle = (c as f32 / num_communities as f32) * std::f32::consts::TAU;
                let cx = community_angle.cos() * world_radius;
                let cy = community_angle.sin() * world_radius;

                // Node position within community cluster
                let node_angle = (offset as f32 / count as f32) * std::f32::consts::TAU;
                let cluster_radius = (count as f32).sqrt() * 20.0;
                let nx = node_angle.cos() * cluster_radius;
                let ny = node_angle.sin() * cluster_radius;

                transform.translation = Vec3::new(cx + nx, cy + ny, 0.0);
            }
        }
        info!("Radial community layout done: {} nodes, {} communities", n, num_communities);
    }

    params.layout_done = true;
    info!("fCoSE layout done");
}
