use crate::layout::apply_layout;
use shared::types::{Graph, Node, Edge, LayoutAlgorithm, KlayLayeredLayoutOptions};
use shared::layout::algorithms::klay::KlayLayoutEngine;
use shared::layout::traits::LayeredLayout;

#[test]
pub
fn test_simple_chain() {
    // Create a simple chain: A -> B -> C
    let mut graph = Graph::new();
    
    let node_a = Node::new("A");
    let node_b = Node::new("B");
    let node_c = Node::new("C");
    
    graph.add_node(node_a)
         .add_node(node_b)
         .add_node(node_c);
    
    let edge1 = Edge::new("e1", "A", "B");
    let edge2 = Edge::new("e2", "B", "C");
    
    graph.add_edge(edge1)
         .add_edge(edge2);
    
    // Apply layout
    let options = KlayLayeredLayoutOptions::default();
    let layout = LayoutAlgorithm::KlayLayered(options);
    apply_layout(&mut graph, &layout).unwrap();
    
    // Verify positions
    let a_pos = graph.nodes.get("A").unwrap().position.unwrap();
    let b_pos = graph.nodes.get("B").unwrap().position.unwrap();
    let c_pos = graph.nodes.get("C").unwrap().position.unwrap();
    
    // Verify layer ordering (y-coordinates)
    assert!(a_pos.1 < b_pos.1);
    assert!(b_pos.1 < c_pos.1);
}

#[test]
pub
fn test_branching() {
    // Create a branching structure:
    //     B
    //    /
    // A
    //    \
    //     C
    let mut graph = Graph::new();
    
    let node_a = Node::new("A");
    let node_b = Node::new("B");
    let node_c = Node::new("C");
    
    graph.add_node(node_a)
         .add_node(node_b)
         .add_node(node_c);
    
    let edge1 = Edge::new("e1", "A", "B");
    let edge2 = Edge::new("e2", "A", "C");
    
    graph.add_edge(edge1)
         .add_edge(edge2);
    
    // Apply layout
    let options = KlayLayeredLayoutOptions::default();
    let layout = LayoutAlgorithm::KlayLayered(options);
    apply_layout(&mut graph, &layout).unwrap();
    
    // Verify positions
    let a_pos = graph.nodes.get("A").unwrap().position.unwrap();
    let b_pos = graph.nodes.get("B").unwrap().position.unwrap();
    let c_pos = graph.nodes.get("C").unwrap().position.unwrap();
    
    // Verify layer ordering
    assert!(a_pos.1 < b_pos.1);
    assert!(a_pos.1 < c_pos.1);
    
    // Verify B and C are in the same layer
    assert_eq!(b_pos.1, c_pos.1);
    
    // Verify B and C are horizontally separated
    assert!(b_pos.0 != c_pos.0);
}

#[test]
pub
fn test_cycle() {
    // Create a cycle: A -> B -> C -> A
    let mut graph = Graph::new();
    
    let node_a = Node::new("A");
    let node_b = Node::new("B");
    let node_c = Node::new("C");
    
    graph.add_node(node_a)
         .add_node(node_b)
         .add_node(node_c);
    
    let edge1 = Edge::new("e1", "A", "B");
    let edge2 = Edge::new("e2", "B", "C");
    let edge3 = Edge::new("e3", "C", "A");
    
    graph.add_edge(edge1)
         .add_edge(edge2)
         .add_edge(edge3);
    
    // Apply layout
    let options = KlayLayeredLayoutOptions::default();
    let layout = LayoutAlgorithm::KlayLayered(options);
    apply_layout(&mut graph, &layout).unwrap();
    
    // Verify all nodes have positions
    assert!(graph.nodes.get("A").unwrap().position.is_some());
    assert!(graph.nodes.get("B").unwrap().position.is_some());
    assert!(graph.nodes.get("C").unwrap().position.is_some());
}

#[test]
pub
fn test_layer_assignment() {
    // Create a simple graph: A -> B -> C
    let mut graph = Graph::new();
    
    let node_a = Node::new("A");
    let node_b = Node::new("B");
    let node_c = Node::new("C");
    
    graph.add_node(node_a)
         .add_node(node_b)
         .add_node(node_c);
    
    let edge1 = Edge::new("e1", "A", "B");
    let edge2 = Edge::new("e2", "B", "C");
    
    graph.add_edge(edge1)
         .add_edge(edge2);
    
    // Create KLay engine
    let engine = KlayLayoutEngine::new(KlayLayeredLayoutOptions::default());
    
    // Get layers
    let layers = engine.assign_layers(&graph).unwrap();
    
    // Verify layer count
    assert_eq!(layers.len(), 3);
    
    // Verify layer contents
    assert_eq!(layers[0], vec!["A"]);
    assert_eq!(layers[1], vec!["B"]);
    assert_eq!(layers[2], vec!["C"]);
}

#[test]
pub
fn test_cycle_breaking() {
    // Create a cycle: A -> B -> A
    let mut graph = Graph::new();
    
    let node_a = Node::new("A");
    let node_b = Node::new("B");
    
    graph.add_node(node_a)
         .add_node(node_b);
    
    let edge1 = Edge::new("e1", "A", "B");
    let edge2 = Edge::new("e2", "B", "A");
    
    graph.add_edge(edge1)
         .add_edge(edge2);
    
    // Create KLay engine
    let engine = KlayLayoutEngine::new(KlayLayeredLayoutOptions::default());
    
    // Get initial layers
    let mut layers = engine.assign_layers(&graph).unwrap();
    
    // Break cycles
    engine.break_cycles(&mut graph, &mut layers).unwrap();
    
    // Verify one edge was reversed
    let mut forward_count = 0;
    let mut backward_count = 0;
    
    for edge in graph.edges.values() {
        if edge.source == "A" && edge.target == "B" {
            forward_count += 1;
        } else if edge.source == "B" && edge.target == "A" {
            backward_count += 1;
        }
    }
    
    assert_eq!(forward_count + backward_count, 2);
    assert!(forward_count == 2 || backward_count == 2);
}

#[test]
pub
fn test_crossing_minimization() {
    // Create a graph with potential crossings:
    // A -> C
    // B -> D
    let mut graph = Graph::new();
    
    let node_a = Node::new("A");
    let node_b = Node::new("B");
    let node_c = Node::new("C");
    let node_d = Node::new("D");
    
    graph.add_node(node_a)
         .add_node(node_b)
         .add_node(node_c)
         .add_node(node_d);
    
    let edge1 = Edge::new("e1", "A", "C");
    let edge2 = Edge::new("e2", "B", "D");
    
    graph.add_edge(edge1)
         .add_edge(edge2);
    
    // Create layers
    let mut layers = vec![
        vec!["A".to_string(), "B".to_string()],
        vec!["D".to_string(), "C".to_string()], // Intentionally ordered to create crossing
    ];
    
    // Create KLay engine
    let engine = KlayLayoutEngine::new(KlayLayeredLayoutOptions::default());
    
    // Minimize crossings
    engine.minimize_crossings(&mut layers, &graph).unwrap();
    
    // Verify crossing was minimized
    assert_eq!(layers[1], vec!["C".to_string(), "D".to_string()]);
}

#[test]
pub
fn test_coordinate_assignment() {
    // Create a simple graph: A -> B
    let mut graph = Graph::new();
    
    let node_a = Node::new("A");
    let node_b = Node::new("B");
    
    graph.add_node(node_a)
         .add_node(node_b);
    
    let edge = Edge::new("e1", "A", "B");
    graph.add_edge(edge);
    
    // Apply layout
    let options = KlayLayeredLayoutOptions::default();
    let layout = LayoutAlgorithm::KlayLayered(options.clone());
    apply_layout(&mut graph, &layout).unwrap();
    
    // Verify positions were assigned
    let a_pos = graph.nodes.get("A").unwrap().position.unwrap();
    let b_pos = graph.nodes.get("B").unwrap().position.unwrap();
    
    // Verify vertical spacing is reasonable
    assert!(b_pos.1 > a_pos.1);
}
