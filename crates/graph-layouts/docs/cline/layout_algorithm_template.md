# Template for Implementing a New Layout Algorithm

Here's a step-by-step template for implementing a new layout algorithm in the knowledge-base-graph project:

## 1. Create the Algorithm Module

Create a new file in `shared/src/layout/algorithms/[algorithm_name].rs` with this structure:

```rust
use crate::types::{Graph, [AlgorithmName]LayoutOptions};
use crate::layout::traits::{LayoutEngine, [AppropriateTrait]};

pub struct [AlgorithmName]LayoutEngine {
    options: [AlgorithmName]LayoutOptions,
}

impl [AlgorithmName]LayoutEngine {
    pub fn new(options: [AlgorithmName]LayoutOptions) -> Self {
        Self { options }
    }
}

impl LayoutEngine for [AlgorithmName]LayoutEngine {
    fn apply_layout(&self, graph: &mut Graph) -> Result<(), String> {
        // Main layout algorithm implementation
        // ...
        
        Ok(())
    }
    
    fn name(&self) -> &'static str {
        "[Human-readable name]"
    }
    
    fn description(&self) -> &'static str {
        "[Description of the algorithm]"
    }
}

impl [AppropriateTrait] for [AlgorithmName]LayoutEngine {
    // Implement trait-specific methods
    // ...
}

impl [AlgorithmName]LayoutEngine {
    // Private helper methods
    // ...
}

/// Public interface for applying the [AlgorithmName] layout algorithm
pub fn apply_layout(graph: &mut Graph, options: &[AlgorithmName]LayoutOptions) -> Result<(), String> {
    let engine = [AlgorithmName]LayoutEngine::new(options.clone());
    engine.apply_layout(graph)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Node, Edge};

    #[test]
    fn test_basic_layout() {
        // Test implementation
        // ...
    }
}
```

## 2. Update the Algorithm Module Exports

Add your new algorithm to `shared/src/layout/algorithms/mod.rs`:

```rust
pub mod [algorithm_name];
// ...

// Re-export the apply_layout functions
pub use [algorithm_name]::apply_layout as [algorithm_name]_apply_layout;
// ...
```

## 3. Add Layout Options to Types

Add your layout options to `shared/src/types.rs`:

```rust
/// [AlgorithmName] layout options
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct [AlgorithmName]LayoutOptions {
    pub base: BaseLayoutOptions,
    // Algorithm-specific options
    // ...
}

impl Default for [AlgorithmName]LayoutOptions {
    fn default() -> Self {
        Self {
            base: BaseLayoutOptions::default(),
            // Default values for algorithm-specific options
            // ...
        }
    }
}

// Update the LayoutAlgorithm enum
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LayoutAlgorithm {
    // Existing algorithms...
    [AlgorithmName]([AlgorithmName]LayoutOptions),
}
```

## 4. Update the Main Layout Module

Update the `apply_layout` function in `shared/src/layout/mod.rs`:

```rust
pub fn apply_layout(graph: &mut Graph, layout: &LayoutAlgorithm) -> Result<(), String> {
    match layout {
        // Existing algorithms...
        LayoutAlgorithm::[AlgorithmName](options) => algorithms::[algorithm_name]::apply_layout(graph, options),
    }
}
```

## 5. Implement the Appropriate Trait

Choose or create an appropriate trait in `shared/src/layout/traits.rs` for your algorithm type:

- `ForceDirectedLayout` - For force-directed algorithms
- `CircularLayout` - For circular layouts
- `LayeredLayout` - For hierarchical/layered layouts
- `HierarchicalLayout` - For tree-like layouts

If needed, create a new trait for your algorithm's specific approach.

## 6. Add Tests

Create tests for your algorithm in the module's test section and consider adding integration tests in the frontend.

## 7. Update Frontend (Optional)

If you need specific frontend functionality, update the frontend layout module to support your new algorithm.

---

This template follows the pattern established in the refactoring, ensuring consistency across all layout algorithms. When implementing a new algorithm, you'll need to:

1. Understand the algorithm's approach (force-directed, circular, hierarchical, etc.)
2. Define appropriate options for configuring the algorithm
3. Implement the core layout logic
4. Add tests to verify the algorithm works correctly

The shared library architecture allows both frontend and backend to use the same layout algorithms, improving code reusability and maintainability.
