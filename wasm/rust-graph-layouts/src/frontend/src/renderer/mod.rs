use eframe::egui;
use egui::{Color32, Stroke, Pos2, Vec2};
use shared::types::{Graph, Node, Edge, GlobalRenderOptions, Viewport};
use crate::utils::hex_to_color32;

/// Graph renderer module
/// This module is responsible for rendering the graph

/// Render a graph to an egui canvas
#[allow(dead_code)]
pub fn render_graph(
    ui: &mut egui::Ui,
    graph: &Graph,
    viewport: &Viewport,
    options: &GlobalRenderOptions,
    selected_nodes: &std::collections::HashSet<String>,
    selected_edges: &std::collections::HashSet<String>,
) {
    let (rect, _) = ui.allocate_exact_size(ui.available_size(), egui::Sense::drag());
    
    let painter = ui.painter();
    
    // Set background color based on dark mode
    let bg_color = if options.dark_mode {
        Color32::from_rgb(30, 30, 30)
    } else {
        Color32::from_rgb(240, 240, 240)
    };
    
    painter.rect_filled(rect, 0.0, bg_color);
    
    // Draw edges
    for edge in graph.edges.values() {
        render_edge(painter, graph, edge, viewport, options, selected_edges);
    }
    
    // Draw nodes
    for node in graph.nodes.values() {
        render_node(painter, node, viewport, options, selected_nodes);
    }
}

/// Render a single node
#[allow(dead_code)]
fn render_node(
    painter: &egui::Painter,
    node: &Node,
    viewport: &Viewport,
    options: &GlobalRenderOptions,
    selected_nodes: &std::collections::HashSet<String>,
) {
    if let Some(position) = node.position {
        let pos = Pos2::new(
            (position.0 * viewport.zoom + viewport.pan_x) as f32,
            (position.1 * viewport.zoom + viewport.pan_y) as f32,
        );
        
        let color = if selected_nodes.contains(&node.id) {
            Color32::YELLOW
        } else if options.dark_mode {
            hex_to_color32(options.node_color.as_str()).unwrap_or(Color32::LIGHT_BLUE)
        } else {
            hex_to_color32(options.node_color.as_str()).unwrap_or(Color32::BLUE)
        };
        
        painter.circle_filled(
            pos,
            options.node_size as f32,
            color,
        );
        
        // Draw labels if enabled
        if options.show_labels {
            render_node_label(painter, node, pos, options);
        }
    }
}

/// Render a node label
#[allow(dead_code)]
fn render_node_label(
    painter: &egui::Painter,
    node: &Node,
    pos: Pos2,
    options: &GlobalRenderOptions,
) {
    let label = node.metadata.get("label")
        .map(|v| match v {
            shared::types::MetadataValue::String(s) => s.clone(),
            _ => node.id.clone(),
        })
        .unwrap_or_else(|| node.id.clone());
    
    let text_color = if options.dark_mode {
        Color32::WHITE
    } else {
        Color32::BLACK
    };
    
    painter.text(
        pos + Vec2::new(0.0, options.node_size as f32 + 5.0),
        egui::Align2::CENTER_TOP,
        label,
        egui::FontId::proportional(options.label_size as f32),
        text_color,
    );
}

/// Render a single edge
#[allow(dead_code)]
fn render_edge(
    painter: &egui::Painter,
    graph: &Graph,
    edge: &Edge,
    viewport: &Viewport,
    options: &GlobalRenderOptions,
    selected_edges: &std::collections::HashSet<String>,
) {
    if let (Some(source), Some(target)) = (
        graph.nodes.get(&edge.source).and_then(|n| n.position),
        graph.nodes.get(&edge.target).and_then(|n| n.position),
    ) {
        let start = Pos2::new(
            (source.0 * viewport.zoom + viewport.pan_x) as f32,
            (source.1 * viewport.zoom + viewport.pan_y) as f32,
        );
        let end = Pos2::new(
            (target.0 * viewport.zoom + viewport.pan_x) as f32,
            (target.1 * viewport.zoom + viewport.pan_y) as f32,
        );
        
        let color = if selected_edges.contains(&edge.id) {
            Color32::YELLOW
        } else if options.dark_mode {
            hex_to_color32(options.edge_color.as_str()).unwrap_or(Color32::GRAY)
        } else {
            hex_to_color32(options.edge_color.as_str()).unwrap_or(Color32::DARK_GRAY)
        };
        
        painter.line_segment(
            [start, end],
            Stroke::new(options.edge_width as f32, color),
        );
    }
}
