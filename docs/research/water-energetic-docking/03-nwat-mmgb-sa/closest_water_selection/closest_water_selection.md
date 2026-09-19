# Closest Water Selection: The cpptraj closest Algorithm

## The Core Algorithm

The `closest` command in cpptraj is the central operation in the Nwat-MMGBSA protocol. It solves a deceptively simple problem: "for each frame of an MD trajectory, select the N water molecules closest to the ligand."

### Algorithmic Steps

```
For each frame f:
  1. Compute ligand centroid = mean position of all ligand heavy atoms
  2. For each water molecule w in the system:
     a. Compute water centroid = oxygen position (TIP3P model)
     b. d_w = || centroid_w - centroid_ligand ||
  3. Sort waters by d_w (ascending)
  4. Select waters[0:N] as the "closest" set
  5. Record water indices in closest.dat for reproducibility
```

### Complexity

For a system with W waters and F frames:
- Distance computation: O(F × W) — typically W ≈ 10,000, F ≈ 1,000
- Sorting per frame: O(W log W)
- Total: O(F × W log W) ≈ 1,000 × 10,000 × log₂(10,000) ≈ 130 million operations

On a modern CPU, this takes a few seconds — negligible compared to the MD simulation.

## Why Not a Distance Cutoff?

The most common alternative — "select all waters within X Å of the ligand" — has several statistical disadvantages:

| Property | closest N | Distance cutoff (e.g., 5 Å) |
|---|---|---|
| Water count per frame | Constant (= N) | Variable |
| Energy normalization | Automatic (constant N) | Must normalize by water count |
| Ensemble average | Balanced (equal weight per frame) | Weighted toward high-water-count frames |
| Sensitivity to box fluctuations | None (position-based selection) | Some (box expansion can move waters outside cutoff) |
| Reproducibility | High (deterministic for given N) | Lower (cutoff depends on system size) |

The constant-N property is the decisive advantage. MM-GBSA computes a sum of per-residue energy contributions. If different frames contribute different numbers of residues (waters), the ensemble average is biased toward frames with more waters. Constant N eliminates this bias.

## The N Dependency

Maffucci et al. (2018) performed a systematic study of how N affects Nwat-MMGBSA performance:

| N | HIV-1 PR r² | AmpC r² | Notes |
|---|---|---|---|
| 0 (implicit) | 0.55 | 0.58 | MM-GBSA baseline (no explicit waters) |
| 10 | 0.68 | 0.65 | First solvation shell incomplete |
| 20 | 0.74 | 0.71 | Most first-shell waters captured |
| 30 | 0.78 | 0.72 | **Sweet spot for enzyme targets** |
| 40 | 0.79 | 0.72 | Diminishing returns begin |
| 60 | 0.78 | 0.71 | Second-shell waters add noise |
| 100 | 0.76 | 0.68 | Too many bulk waters dilute signal |

The optimal N is target-dependent because it depends on the size and geometry of the binding site. A deep, narrow pocket (AmpC) needs fewer waters to fully solvate the ligand. A flat, extended interface (Rac1-Tiam1) needs more.

## Jump-Cannon Algorithmic Parallel: Octree Traversal as closest N

The octree traversal in jump-cannon's `octree.wgsl` and `fa2_bh.rs` performs the same fundamental operation in reverse:

| Operation | cpptraj closest | Jump-Cannon Octree |
|---|---|---|
| Query | "Closest N waters to ligand" | "All bodies within repulsion radius of node i" |
| Data structure | Sorted distance array | Morton-sorted octree |
| Distance metric | Euclidean distance | Euclidean distance |
| Selection criterion | Sort by distance, take N | s/d < θ (Barnes-Hut acceptance) |
| Output cardinality | Exactly N | Variable (depends on spatial distribution) |
| Rebuild frequency | Every frame | Every step |

The octree could be adapted to implement the `closest N` operation directly: instead of accumulating COM contributions for all cells passing the θ criterion, stop the traversal after accumulating N contributions (sorted by cell distance). This would give GPU-accelerated closest-N selection for any point in space.

### GPU closest-N Pseudocode

```wgsl
// Adapting octree.wgsl for closest-N selection
fn closest_n(query_point: vec3<f32>, n: u32, oct_root: u32) -> array<u32, N> {
    // Priority queue of (distance, node_idx), initialized with root
    // While queue not empty and results < N:
    //   Pop closest node
    //   If leaf and body_idx != OCT_BODY_INTERNAL:
    //     Add body_idx to results
    //   Else:
    //     Push all children onto priority queue
    // Return results[0:N]
}
```

This would enable on-GPU water selection without CPU readback, analogous to the fully-GPU octree build pipeline.

## The closestout Format

The `closestout` file records per-frame water selections for reproducibility:

```
#Frame  W1  W2  W3  ...  WN
1       234 567 123 ... 890
2       234 568 123 ... 890
...
```

This enables post-hoc analysis: which waters are consistently selected? A water selected in >90% of frames is "structurally conserved" (analogous to a crystallographic water). A water selected in <30% of frames is "transient" (bulk-like).

This per-water occupancy analysis is the MD equivalent of WaterMap's thermodynamic analysis: both identify which waters are structurally important.
