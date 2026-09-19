# Jump-Cannon Algorithm Mapping: Water-Energetic Docking → GPU Graph Layout

## Overview

This document maps each computational technique from the water-energetic molecular docking workflow onto the concrete algorithms, shaders, and data structures in jump-cannon's GPU compute layer. Every mapping identifies the specific file, function, and binding slot where the analogous computation occurs.

## Mapping Table

| Docking Technique | Jump-Cannon Analog | File | Algorithmic Parallel |
|---|---|---|---|
| WaterMap GCMC sampling | NegativeSampling repulsion | `gpu_force.rs` / `force.wgsl` | Statistical sampling replaces exhaustive enumeration |
| Nwat closest-water selection | Barnes-Hut octree traversal | `octree.wgsl` / `fa2_bh.rs` | Spatial partitioning → O(n log n) neighbor lookup |
| MM-GBSA ensemble averaging | FA2 adaptive-speed controller | `fa2_speed.rs` | Variance reduction via statistical aggregation |
| Glide WS calibration layer | Edge-strength Jaccard/CorrectedOverlap | `edge_strength.rs` | Local neighborhood analysis weights interactions |
| AM1-BCC charge parameterization | PageRank mass assignment | `pagerank.rs` / `geometric.rs` | Global-to-local property propagation |
| Glide SP→XP→WS funnel | Engine registry cascade | `engines/mod.rs` / `multilevel.rs` | Multi-resolution solver selection |
| FM³ multilevel coarsening | Topological fisheye coarsening | `topo_fisheye.rs` / `multilevel.wgsl` | Heavy-edge matching graph contraction |
| Water bridge detection | Louvain community detection | `louvain.rs` | Structural role identification |
| Desolvation penalty | Spring constant damping | `edge_strength.rs` → `force.wgsl` | Local context modifies pairwise interaction strength |
| cpptraj closest command | GPU spatial hash bonding | `geometric_bonding_gpu.rs` | 27-cell stencil neighbor scan |

## Architectural Analysis

### 1. The Sampling Principle: GCMC ↔ NegativeSampling

**Docking domain**: WaterMap uses Grand Canonical Monte Carlo to sample water insertion/deletion moves in the binding site. It does not exhaustively enumerate all water configurations — that would be combinatorially impossible. Instead, it uses statistical sampling with a chemical potential that ensures correct bulk density.

**Jump-cannon domain**: `RepulsionMode::NegativeSampling` in `gpu_force.rs` replaces O(n²) all-pairs repulsion with K random partners per node per step. This is the identical statistical architecture:

```rust
// gpu_force.rs — the sampling mode that mirrors GCMC logic
pub enum RepulsionMode {
    Exact,             // Full O(n²) — like explicit solvent MD
    BarnesHut,          // O(n log n) — like grid-based water placement
    NegativeSampling,   // O(n·K) — like GCMC sampling
}
```

The key insight: both methods replace an intractable exhaustive computation with a statistically unbiased estimator. WaterMap's GCMC converges to the correct water density because the Metropolis criterion ensures detailed balance. Jump-cannon's SNAP-tFDP (arXiv:2608.01907, eq. 6) converges to the correct force because the edge-centric sampler is an unbiased estimator of the full t-FDP repulsion.

**Structural parallel — the estimator proof**: In both cases, convergence is guaranteed by proving that the expectation of the sampled quantity equals the true value:

- **GCMC**: E[water density] = ρ_bulk (chemical potential matching)
- **SNAP-tFDP**: E[Σ_j repulsion(i,j)] over sampled j = (1/n) Σ_j repulsion(i,j) over all j (edge-centric expectation)

The SNAP-tFDP proof (eq. 6) shows that degree-weighting the repulsion by `(d_i + d_j)/2` makes the edge-centric sampler unbiased with respect to the node-centric ground truth. This is the statistical analog of GCMC's chemical potential matching.

### 2. The Closest-N Problem: cpptraj closest ↔ Octree Traversal

**Docking domain**: The `cpptraj closest N :LIG` command selects the N water molecules nearest to the ligand centroid in each MD frame. Algorithmically: (1) compute water-ligand distances for all waters, (2) sort, (3) take top N. This is O(W log W) per frame, where W is the number of waters (thousands).

**Jump-cannon domain**: The `octree.wgsl` Barnes-Hut pipeline performs the analogous operation in reverse: instead of finding N closest waters to a ligand, it finds which bodies are close enough to interact with a given body for repulsion computation. The octree structure (Morton-sorted bodies, stackless rope traversal) enables O(log n) neighbor queries.

The algorithmic parallel is precise:

| Operation | Nwat-MMGBSA | Jump-Cannon |
|---|---|---|
| Data structure | Sorted water-ligand distance array | Morton-sorted octree |
| Query | "Closest N waters" | "Bodies within repulsion radius" |
| Acceptance | distance < threshold | s/d < θ (Barnes-Hut criterion) |
| Output | N water indices | Set of COM contributions |
| Cost per query | O(W log W) sort | O(log n) tree descent |

**Why the octree is superior for the graph case**: The `closest` command must be recomputed for every frame because water positions change. The octree is also rebuilt each step (`octree.wgsl::bbox_clear` → `morton_assign` → `radix_scatter` → `node_emit`), but the rebuild is fully on-GPU and amortized across all repulsion queries. The key structural difference: Nwat-MMGBSA does one query per frame (ligand vs. all waters); jump-cannon does n queries per step (each body vs. all bodies). The octree amortizes the build cost across n queries.

### 3. Ensemble Averaging: MM-GBSA ↔ Adaptive Speed Controller

**Docking domain**: MM-GBSA computes ΔG_bind = ⟨E_MM⟩ + ⟨G_solvation⟩ − T⟨S⟩, where ⟨·⟩ denotes an average over MD snapshots. The averaging is essential — any single snapshot has large fluctuations in the MM energy term, and the ensemble average converges to the true thermodynamic expectation.

**Jump-cannon domain**: The `AdaptiveSpeed` struct in `fa2_speed.rs` maintains a running controller state:

```rust
pub struct AdaptiveSpeed {
    pub speed: f32,           // Global speed s(G) — the ensemble summary
    pub speed_efficiency: f32, // Slow secondary controller tracking coherence
}
```

Each step, the GPU force pass emits per-node `(mass·swing, mass·traction)` stats. The host **sums** them in fixed node order — this summation is mathematically equivalent to the MM-GBSA ensemble average:

- **MM-GBSA**: ⟨E_MM⟩ = (1/T) Σ_t E_MM(t) over T snapshots
- **FA2 speed**: total_swinging = Σ_i mass·swing_i over n nodes

Both transform a per-particle/snapshot fluctuating quantity into a single scalar that controls the next step's behavior:

- **MM-GBSA**: The averaged ΔG_bind ranks ligands
- **FA2**: The averaged swing/traction ratio sets s(G), which bounds displacement for every node

The structural identity: both are **reduction operations** (sum over ensemble → scalar) that feed into a **control decision** (rank ligand / set global speed).

### 4. The Scoring Layer: Glide WS Calibration ↔ Edge-Strength Damping

**Docking domain**: Glide WS's scoring function is anchored by a calibration layer trained on thousands of PDB structures and FEP+ calculations. This layer detects cases where the empirical terms (van der Waals, electrostatics, hydrogen bonds) over- or under-predict affinity. Specifically, it catches "magic methyl" effects — where a single methyl group displaces a high-energy water, contributing +1–2 kcal/mol beyond what the van der Waals term predicts.

**Jump-cannon domain**: The `edge_strength` module in `graph-metrics` computes per-edge structural scores:

```rust
pub enum EdgeStrengthKind {
    Jaccard,           // T / (deg_u + deg_v - 2 - T)
    CorrectedOverlap,  // Batagelj's damped version
}
```

These scores weight edge spring constants in the force layout: `k_eff = k_base × edge_strength(e)`. An edge with Jaccard ≈ 0 (global shortcut, few common neighbors) gets a weak spring; an edge with Jaccard ≈ 1 (embedded in a cluster, many common neighbors) gets a strong spring.

The parallel: both are **context-dependent modifiers** of a base interaction energy:

- **Glide WS calibration**: Modifies the empirical score based on the local water environment
- **Edge strength damping**: Modifies the spring constant based on the local topological environment

The "magic methyl" detection is structurally identical to identifying edges whose Jaccard score is anomalously high for their degree — a single connection that dramatically reconfigures the local topology.

### 5. AM1-BCC: The Middle Path ↔ PageRank Mass Assignment

**Docking domain**: AM1-BCC charges balance the speed of empirical methods (fast but inaccurate for heterocycles) with the accuracy of full QM (accurate but prohibitive for thousands of compounds). They are the "architect's choice" — not the fastest, not the most accurate, but the right tradeoff for medium-throughput rescoring.

**Jump-cannon domain**: The `geometric` engine's `MassSource::PageRank` assigns node masses from PageRank scores. PageRank is the "middle path" for structural importance:

- **Faster**: `MassSource::Degree` — O(1) per node, but can't distinguish hubs from peripheral high-degree nodes
- **More accurate**: `MassSource::Betweeness` — identifies bridge nodes, but O(n·m) expensive
- **Middle path**: `MassSource::PageRank` — O(k·m) with k iterations, captures global structure without the full expense

The parallel is the **hierarchy of approximations**, each trading accuracy for speed at a specific ratio:

| Docking Parameterization | Jump-Cannon Mass Source | Speed | Accuracy |
|---|---|---|---|
| Gasteiger (empirical) | `MassSource::Degree` | Fastest | Coarsest |
| AM1-BCC (semi-empirical QM) | `MassSource::PageRank` | Medium | Good |
| RESP (full QM) | `MassSource::Betweenness` | Slowest | Best |

### 6. The Multilevel Cascade: FM³ ↔ Topological Fisheye

**Docking domain**: The FM³ (Fast Multipole Multilevel Method) by Hachul & Jünger uses a multilevel approach: coarsen the interaction graph, solve at the coarsest level, prolong back down. This is the ancestor of the deployment funnel (Glide SP → XP → WS → MM-GBSA → FEP+).

**Jump-cannon domain**: The `topo_fisheye` module and `multilevel.wgsl` implement the identical cascade:

```rust
// topo_fisheye.rs — the §4 multilevel pipeline (Gansner-Koren-North 2004)
pub fn seed_positions(n_nodes, edges, seed_mode, spring_len) -> Vec<f32> {
    // 1. Bootstrap random ball
    // 2. Coarsen into hierarchy (heavy-edge matching)
    // 3. Layout coarsest level (tiny FR sim)
    // 4. Prolong + relax level by level
}
```

The `multilevel.wgsl` shader takes this fully GPU-resident: heavy-edge matching (HEM) edge contraction, sort-and-scan coarse edge deduplication, and prolongation with jitter — all on-device, no host readback.

**Structural identity**: Both are instances of the **multigrid method** from numerical analysis. The core insight — "solve on a coarse grid, then refine" — applies identically to PDEs, N-body problems, graph layout, and molecular docking. The specific operations differ (force computation vs. free energy evaluation) but the cascade architecture is invariant.

### 7. Water Bridge Detection ↔ Louvain Community Detection

**Docking domain**: A water bridge is a path of length 2: protein–water–ligand. Identifying which waters form bridges (vs. which waters are merely present) requires analyzing the hydrogen-bond network topology.

**Jump-cannon domain**: Louvain community detection identifies which edges are intra-community (bridge local structure) vs. inter-community (connect distinct clusters). A water bridge is the molecular equivalent of an edge whose endpoints are in the same Louvain community: the water "belongs" to both the protein and the ligand's local environment.

The `louvain.rs` algorithm is hierarchical — it produces `community_levels` with multiple resolutions, analogous to identifying water bridges at different length scales (first-shell waters, second-shell waters, bulk).

---

## The Deepest Parallel: Water as a Graph Node

The most powerful reframing is to treat every water molecule as a **transient graph node**:

| Concept | Molecular Domain | Graph Domain |
|---|---|---|
| Fixed nodes | Protein residues | Graph nodes (vault documents) |
| Dynamic nodes | Water molecules in binding site | Nodes added during layout iteration |
| Fixed edges | Covalent bonds, salt bridges | Explicit edges (wikilinks, references) |
| Dynamic edges | Water-mediated H-bonds | Dynamic bonds (`geometric_bonding_gpu.rs`) |
| Node insertion | Ligand binding | Adding a node to an existing layout |
| Node deletion | Water displacement | Removing a transient node |
| Edge creation | New water bridge formed | Dynamic bond formation |
| Edge deletion | Water bridge broken | Dynamic bond breakage |

The `geometric` engine with `DynamicEdge` bonding is the engine that most directly mirrors the molecular docking problem. Its `ClassSource`, `CoordinationSource`, and class affinity matrix are the exact analogs of atom types, hybridization, and the periodic table. The `geometric_bonding_gpu.rs` module's GPU spatial hash (27-cell stencil) is the direct port of the `cpptraj closest N` algorithm to the graph domain.

### Dynamic Bonds as Water Bridges

The `geometric_bonding_gpu.rs` module implements dynamic edge formation based on spatial proximity and class compatibility:

```rust
// geometric_bonding_gpu.rs — the GPU dynamic-edge bonding stage
// Bond formation criterion:
//   1. Spatial proximity: distance < r_bond (class-dependent)
//   2. Class affinity: class_affinity[class_u * dim + class_v] > 0
//   3. Valence cap: max bonds per node respected
```

This is a direct port of the water bridge formation criterion in molecular docking:
1. **Spatial proximity**: Water must be within hydrogen-bonding distance (≈3.0 Å)
2. **Chemical compatibility**: Water can only H-bond with donors/acceptors
3. **Coordination cap**: A single water can form at most 4 hydrogen bonds

The GPU implementation's design — atomics-free, sort-based, deterministic — is forced by WebGPU's lack of f32 atomics. This constraint pushes the architecture toward the same sort-and-scan pattern that `cpptraj closest` uses. The design split (GPU candidate generation, host bond decisions) mirrors the Nwat-MMGBSA split (GPU MD trajectory, host energy evaluation).

---

## Implications for Jump-Cannon Development

### 1. The Missing "WaterMap" Module

Jump-cannon has no module that analyzes the *thermodynamic quality* of spatial positions — which regions of the layout are "high-energy" (unstable, likely to rearrange) vs. "low-energy" (stable, converged). The `energy_threshold` in `GpuForceOptions` is a crude proxy (average kinetic energy), but it doesn't provide per-node or per-region energy decomposition.

**Proposal**: A `layout_watermap` module that runs a short MD-like simulation (multiple layout steps from perturbed positions) and computes per-node variance. High-variance nodes are "high-energy" — candidates for re-layout or user attention. Low-variance nodes are "stable" — converged structure.

### 2. The "Magic Methyl" for Graph Layout

The magic methyl effect — a single-atom change that produces a non-intuitive potency boost — has a direct analog in graph layout: adding a single edge that dramatically reorders the layout. The `edge_strength` module already identifies such edges (Jaccard ≈ 0 edges that are "surprising" given the node degrees), but there's no UI surface for "what edge, if removed, would most change the layout?"

This is the exact analog of WaterMap's displacement analysis: "which water, if displaced, would most change the binding affinity?"

### 3. Topo-Fisheye as the Multilevel Docking Analog

The `SeedMode::TopoFisheye` seeder in `gpu_force.rs` is built on the Gansner-Koren-North (2004) multilevel pipeline — the same mathematical ancestor as FM³, which is the ancestor of the modern multilevel docking funnel. The key difference is that jump-cannon uses the pipeline only for seeding (initial positions), while the docking funnel uses it for the entire workflow.

The `multilevel` wrapper engine (`engines/multilevel.rs`) extends this to continuous operation: coarsen → solve coarsest → prolong → refine → repeat. This is the full FM³ pipeline applied to graph layout. The docking community calls this "hierarchical screening"; the graph layout community calls it "multilevel force-directed layout"; they are the same algorithm.

### 4. GPU Multilevel (`multilevel.wgsl`) as On-Device Docking

The `multilevel.wgsl` shader (713 lines) is the most architecturally ambitious GPU code in jump-cannon. It implements a full multilevel coarsening cascade entirely on the GPU: heavy-edge matching → edge contraction → sort-and-scan coarse edge deduplication → layout at coarse level → prolongation with jitter.

This is the computational equivalent of running the entire docking funnel (Glide SP → XP → WS → MM-GBSA) on a single GPU without host intervention. The design constraints (no f32 atomics, no host readback between levels) force a clean architecture that would translate directly to a GPU-native docking pipeline.

---

## Concrete Code Parallels

### Octree Traversal (`octree.wgsl`) ≈ cpptraj closest

```wgsl
// octree.wgsl: stackless rope traversal — the GPU equivalent of "closest N"
// At each visited node:
//   if leaf or s/d < θ: accumulate COM, jump to skip_idx (next sibling)
//   else:                descend by jumping to next_idx (first child)
// Sentinel OCT_END terminates the walk.
```

```python
# Nwat-MMGBSA equivalent in Python (cpptraj closest logic)
def closest_waters(water_positions, ligand_center, n):
    distances = [(i, norm(p - ligand_center)) for i, p in enumerate(water_positions)]
    distances.sort(key=lambda x: x[1])
    return [i for i, _ in distances[:n]]
```

The octree replaces the O(W) distance computation + O(W log W) sort with an O(log W) tree descent per query, amortizing the O(W log W) build cost across all queries.

### Adaptive Speed (`fa2_speed.rs`) ≈ MM-GBSA Ensemble Average

```rust
// fa2_speed.rs: the host-side reduction that mirrors MM-GBSA averaging
pub fn update(&mut self, n_nodes: u32, jitter_tolerance: f32, stats: &[f32]) -> f32 {
    let mut total_swinging = 0.0f64;  // ← analogous to Σ E_MM(t)
    let mut total_traction = 0.0f64;  // ← analogous to Σ G_solvation(t)
    for pair in stats.chunks_exact(2) {
        total_swinging += pair[0] as f64;
        total_traction += pair[1] as f64;
    }
    // Decision: adjust global speed based on swing/traction ratio
    // ← analogous to ranking ligands by averaged ΔG_bind
}
```

### Edge Strength (`edge_strength.rs`) ≈ WaterMap Thermodynamics

```rust
// edge_strength.rs: per-edge Jaccard score
// T = |N(u) ∩ N(v)| = number of common neighbors
// Jaccard = T / (deg_u + deg_v - 2 - T)
// 
// WaterMap analog per water molecule w:
// ΔG_hyd(w) = ΔH_hyd(w) - TΔS_hyd(w)
// "Jaccard" for water w = (H-bond count with protein) / (max possible H-bonds)
```

Both produce a scalar in [0,1] that quantifies how "embedded" an interaction is in its local environment. The force layout and the docking scoring function both use this scalar to weight interaction strengths.

---

## Summary: The Algorithmic Isomorphism

The water-energetic docking workflow and jump-cannon's GPU graph layout engine are **algorithmically isomorphic** — they solve the same abstract problem (place entities in space to minimize an energy function) using the same computational strategies (multilevel coarsening, statistical sampling, spatial partitioning, ensemble averaging). The domain-specific terminology differs, but the data structures, shaders, and control flow are structurally identical.

This isomorphism means that advances in either domain can be translated to the other with minimal architectural friction. A GPU-accelerated Nwat-MMGBSA implementation could reuse jump-cannon's octree builders and sort-and-scan kernels directly. Conversely, a "water-aware" graph layout that incorporates per-node spatial quality scores (analogous to WaterMap's ΔG_hyd) could improve jump-cannon's ability to identify unstable regions of a layout.
