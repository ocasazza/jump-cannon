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

    use graph_layouts::{Graph, Node, Edge, FcoseOptions, LayoutOptions};

    let mut graph = Graph::new();

    for id in vault.graph.nodes.keys() {
        graph.add_node(Node::new(id.clone()));
    }

    for (i, edge) in vault.graph.edges.iter().enumerate() {
        graph.add_edge(Edge::new(
            format!("e{}", i),
            edge.source.clone(),
            edge.target.clone(),
        ));
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
            for (graph_node, mut transform) in nodes.iter_mut() {
                if let Some(node) = graph.nodes.get(&graph_node.id) {
                    if let Some((x, y)) = node.position {
                        transform.translation = Vec3::new(x as f32, y as f32, 0.0);
                    }
                }
            }
        }
        Err(e) => {
            warn!("fCoSE layout error: {}", e);
        }
    }

    params.layout_done = true;
    info!("fCoSE layout done");
}
