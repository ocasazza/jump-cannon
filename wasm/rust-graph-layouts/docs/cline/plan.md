# KLay Layout Implementation Plan

## Overview
Implementing the KLay Layered layout algorithm with frontend-first computation and optional backend support later.

## 1. Shared Types Implementation

### Update BaseLayoutOptions in `shared/src/types.rs`
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LayoutComputeLocation {
    Frontend,
    Backend,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BaseLayoutOptions {
    pub animate: bool,
    pub animation_duration: u32,
    pub fit: bool,
    pub padding: u32,
    pub compute_location: LayoutComputeLocation,
}

impl Default for BaseLayoutOptions {
    fn default() -> Self {
        Self {
            animate: true,
            animation_duration: 500,
            fit: true,
            padding: 30,
            compute_location: LayoutComputeLocation::Frontend,
        }
    }
}
```

### Add KLay Options in `shared/src/types.rs`
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KlayLayeredLayoutOptions {
    pub base: BaseLayoutOptions,
    pub layer_spacing: f64,          // Space between layers
    pub node_spacing: f64,           // Space between nodes in same layer
    pub node_placement: String,      // "SIMPLE", "LINEAR_SEGMENTS", "BRANDES_KOEPF"
    pub cross_minimization: String,  // "LAYER_SWEEP", "INTERACTIVE"
    pub cycle_breaking: String,      // "GREEDY", "INTERACTIVE"
    pub edge_routing: String,        // "ORTHOGONAL", "SPLINES", "POLYLINE"
    pub merge_edges: bool,           // Whether to merge parallel edges
}

impl Default for KlayLayeredLayoutOptions {
    fn default() -> Self {
        Self {
            base: BaseLayoutOptions::default(),
            layer_spacing: 50.0,
            node_spacing: 20.0,
            node_placement: "BRANDES_KOEPF".to_string(),
            cross_minimization: "LAYER_SWEEP".to_string(),
            cycle_breaking: "GREEDY".to_string(),
            edge_routing: "ORTHOGONAL".to_string(),
            merge_edges: false,
        }
    }
}
```

### Update LayoutAlgorithm enum
```rust
pub enum LayoutAlgorithm {
    Fcose(FcoseLayoutOptions),
    CoseBilkent(CoseBilkentLayoutOptions),
    Cise(CiseLayoutOptions),
    Concentric(ConcentricLayoutOptions),
    KlayLayered(KlayLayeredLayoutOptions),
}
```

## 2. Frontend Implementation

### Create KLay Layout Module in `frontend/src/layout/klay.rs`
```rust
pub struct KlayLayeredLayouter {
    options: KlayLayeredLayoutOptions,
}

impl KlayLayeredLayouter {
    // Core layout phases
    fn assign_layers(&self, graph: &Graph) -> HashMap<String, usize>;
    fn break_cycles(&self, graph: &mut Graph);
    fn order_nodes(&self, graph: &Graph, layers: &HashMap<String, usize>) -> Vec<Vec<String>>;
    fn assign_coordinates(&self, graph: &mut Graph, ordered_layers: &Vec<Vec<String>>);
    
    // Helper methods
    fn calculate_node_dimensions(&self, graph: &Graph) -> HashMap<String, (f64, f64)>;
    fn find_cycles(&self, graph: &Graph) -> Vec<Vec<String>>;
    fn calculate_edge_crossings(&self, layers: &[Vec<String>], graph: &Graph) -> usize;
}

impl LayoutEngine for KlayLayeredLayouter {
    fn name(&self) -> &'static str {
        "KLay Layered"
    }
    
    fn description(&self) -> &'static str {
        "Layer-based layout algorithm optimized for directed graphs"
    }
    
    fn apply(&self, graph: &mut Graph) -> Result<(), String> {
        // 1. Assign nodes to layers
        let layers = self.assign_layers(graph);
        
        // 2. Break cycles if needed
        self.break_cycles(graph);
        
        // 3. Order nodes within layers
        let ordered_layers = self.order_nodes(graph, &layers);
        
        // 4. Assign final coordinates
        self.assign_coordinates(graph, &ordered_layers);
        
        Ok(())
    }
}
```

### Update Frontend Layout Module in `frontend/src/layout/mod.rs`
```rust
pub async fn apply_layout(graph: &mut Graph, layout: &LayoutAlgorithm) -> Result<(), String> {
    match layout {
        LayoutAlgorithm::KlayLayered(options) => {
            match options.base.compute_location {
                LayoutComputeLocation::Frontend => {
                    let layouter = KlayLayeredLayouter::new(options.clone());
                    layouter.apply(graph)
                }
                LayoutComputeLocation::Backend => {
                    let request = ApplyLayoutRequest {
                        graph_id: graph.id.clone(),
                        layout: layout.clone(),
                    };
                    let response = send_layout_request(request).await?;
                    *graph = response.graph;
                    Ok(())
                }
            }
        }
        // ... other layouts
    }
}
```

## 3. Testing Strategy

### Create Test Module in `frontend/src/layout/tests/klay.rs`
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_chain() {
        // Test A -> B -> C
        let mut graph = create_test_graph_chain();
        let options = KlayLayeredLayoutOptions::default();
        let layouter = KlayLayeredLayouter::new(options);
        
        layouter.apply(&mut graph).unwrap();
        
        // Verify positions
        assert_node_in_layer(&graph, "A", 0);
        assert_node_in_layer(&graph, "B", 1);
        assert_node_in_layer(&graph, "C", 2);
    }

    #[test]
    fn test_branching() {
        // Test branching structure
        //     B
        //    /
        // A
        //    \
        //     C
        let mut graph = create_test_graph_branching();
        let options = KlayLayeredLayoutOptions::default();
        let layouter = KlayLayeredLayouter::new(options);
        
        layouter.apply(&mut graph).unwrap();
        
        // Verify positions
        assert_node_in_layer(&graph, "A", 0);
        assert_node_in_layer(&graph, "B", 1);
        assert_node_in_layer(&graph, "C", 1);
        assert_nodes_not_overlapping(&graph, "B", "C");
    }

    #[test]
    fn test_cycle() {
        // Test cycle A -> B -> C -> A
        let mut graph = create_test_graph_cycle();
        let options = KlayLayeredLayoutOptions::default();
        let layouter = KlayLayeredLayouter::new(options);
        
        layouter.apply(&mut graph).unwrap();
        
        // Verify cycle handling
        assert_cycle_broken(&graph);
        assert_nodes_properly_spaced(&graph);
    }
}
```

## 4. Future Backend Support

When ready to add backend support:

1. Copy the KLay implementation to `backend/src/layout/klay.rs`
2. Update `backend/src/handlers/graph.rs` to use the implementation
3. Add configuration options for when to use backend computation (e.g., based on graph size)
4. Add caching for computed layouts

## Implementation Order

1. Implement shared types (LayoutComputeLocation, KlayLayeredLayoutOptions)
2. Create basic KlayLayeredLayouter structure
3. Implement core layout phases:
   - Layer assignment
   - Cycle breaking
   - Node ordering
   - Coordinate assignment
4. Add tests for each phase
5. Integrate with existing layout system
6. Add position-based test cases
7. Manual testing with various graph structures
8. Documentation and examples
