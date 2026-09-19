# Performance Benchmarks and Target-Specific Outcomes

## Executive Summary

This section maps the performance characteristics of water-energetic docking methods against specific target families, then connects each computational strategy to analogous algorithms in **jump-cannon's GPU graph layout engine**. The mapping is not metaphorical — it identifies concrete algorithmic parallels between:

- **WaterMap's GCMC sampling** → jump-cannon's GPU negative-sampling repulsion (SNAP-tFDP)
- **Nwat-MMGBSA's closest-water selection** → jump-cannon's Barnes-Hut octree nearest-neighbor traversal
- **MM-GBSA ensemble averaging** → jump-cannon's adaptive-speed FA2 swing/traction controller
- **Glide WS's calibration layer** → jump-cannon's edge-strength Jaccard/CorrectedOverlap scoring
- **Multilevel coarsening** → the shared FM³ / Walshaw ancestor both domains inherit

## Target Analysis and Decision Impact

### Penicillopepsin & HIV-1 Protease: Water-Mediated Binding as Primary Driver

In aspartic proteases, water-mediated bridging is the dominant binding mechanism. The catalytic aspartate dyad is flanked by structurally conserved water molecules that mediate inhibitor contacts. Standard docking (Glide SP/XP) produces scores that are dominated by the geometric fit of the inhibitor in the active site, correlating poorly with experimental affinity (r² ≈ 0.3).

**Nwat-MMGBSA transformation**: By retaining the N closest explicit waters per MD frame, the rescoring protocol captures the thermodynamic contribution of the hydration shell. The coefficient of determination jumps from r² ≈ 0.3 to r² ≈ 0.8. This is not incremental improvement — it transforms the ranking from noise into signal.

**Jump-cannon parallel: Louvain community detection + edge-strength scoring**. Just as Nwat-MMGBSA identifies structurally conserved waters that "belong" to the binding site, jump-cannon's Louvain community detection identifies nodes that "belong" to a cluster. The edge_strength module's Jaccard/CorrectedOverlap metrics perform the analogous role: quantifying whether an edge (interaction) is intra-cluster (structurally embedded, like a conserved water bridge) or inter-cluster (a global shortcut, like a solvent-exposed hydrophobic contact).

### AmpC β-Lactamase: False Positive Filtration

AmpC β-lactamase is a classic case where hydrophobic decoys score well in implicit-solvent docking. Decoys with isopropyl or chloropropyl groups bury nonpolar surface area in the active site — standard scoring rewards this. But the groups overlap with crystallographic water positions that cannot be displaced. Nwat-MMGBSA penalizes these decoys because the explicit water shell reveals the steric clash.

**Key mechanism**: The "penalty signal" comes from waters that are consistently among the N closest to the ligand across the trajectory. A decoy that overlaps a conserved water site forces that water into a higher-energy configuration, which the MM-GBSA energy evaluation captures.

**Jump-cannon parallel: Force-directed repulsion with Barnes-Hut octree**. In the `fa2-bh` engine, the Barnes-Hut octree performs a spatial partitioning that is structurally analogous to Nwat-MMGBSA's water selection. The octree cells that intersect the binding site are like the N closest waters — they define the local environment that determines whether a node (ligand pose) is "compatible" with its surroundings. A node that overlaps a dense octree cell (analogous to overlapping a conserved water) experiences strong repulsion — the spatial equivalent of a desolvation penalty.

The `geometric` engine takes this further: its class-based exclusion/affinity matrix (`ClassSource`) assigns per-node radii and cross-class affinities. This is the direct algorithmic analog of classifying waters as displaceable (high-energy, class A) vs. retained (low-energy, class B) and applying different interaction rules.

### Rac1-Tiam1 PPI: Scaling to Large Interfaces

Protein-Protein Interaction interfaces are large (1500–3000 Å²), flat, and solvent-exposed. Standard rescoring fails because the binding site definition is ambiguous — where does the "site" end and bulk solvent begin? The architectural insight from Nwat-MMGBSA is to scale the water count up (Nwat = 60–100) to capture the broad hydration environment.

This is a **scaling problem**, not a precision problem. The algorithmic challenge is: how do you handle O(N) explicit waters without the computation becoming O(N²)?

**Jump-cannon parallel: NegativeSampling + t-FDP for large graphs**. The jump-cannon force layout faces the identical scaling challenge. Exact O(n²) repulsion is fine for <1,000 nodes but breaks down at vault scale (10,000+ nodes). The solution space maps directly:

| Nwat-MMGBSA Challenge | Jump-Cannon Solution |
|---|---|
| N=100 waters × 1000 frames = expensive | NegativeSampling: K random partners per node |
| Water-water interactions are O(N²) | t-FDP force law with degree-weighted repulsion |
| Need representative sampling, not exhaustive | SNAP-tFDP edge-centric sampler (arXiv:2608.01907) |
| Convergence requires enough samples | K=8 samples/step, validated against exact baseline |

The SNAP-tFDP estimator in `force.wgsl` proves mathematically (arXiv:2608.01907, eq. 6) that its edge-centric sampler optimizes the expectation of the t-FDP repulsion term. This is exactly the same statistical architecture as Nwat-MMGBSA's closest-water selection: both replace O(N²) exact computation with O(N·K) sampled computation, where K is chosen to preserve the statistical properties of the full ensemble.

## Deployment Funnel: The Multi-Resolution Pipeline

The research document proposes a hierarchical deployment strategy:

```
Initial Screening (Millions) → Glide SP          [Ultra-fast, implicit solvent]
Hit Filtering (Thousands)    → Glide WS           [Water-aware, explicit sampling]
Lead Optimization (Hundreds) → Nwat-MMGBSA         [Ensemble MD, explicit solvent]
Final Validation             → ABFEP+             [Rigorous free energy, gold standard]
```

**Jump-cannon parallel: The multi-engine layout registry.** Jump-cannon's `EngineRegistry` is architecturally identical to this funnel. Each engine represents a different resolution of the same fundamental problem (placing nodes in space / placing ligands in a binding site):

| Docking Funnel | Jump-Cannon Engine | Resolution |
|---|---|---|
| Glide SP (fastest, coarsest) | `cpu-spring` engine | Spring-only, no repulsion, instant |
| Glide XP (medium precision) | `fa2-brute` engine | O(n²) repulsion, adaptive speed, ~10k nodes |
| Glide WS (water-aware) | `fa2-bh` engine | Barnes-Hut O(n log n), same layout as brute |
| Nwat-MMGBSA (ensemble) | `sgd-stress` engine | Sparse stress, pivot-sampled, converged |
| ABFEP+ (gold standard) | `geometric` engine | Class-aware, coordination-constrained, crystalline |

The `multilevel` wrapper engine is the direct analog of the entire funnel: it coarsens (Glide SP equivalent), solves at the coarsest level (Glide XP equivalent), prolongs (Glide WS equivalent), and refines level-by-level (Nwat-MMGBSA equivalent). The cascade is the same algorithm applied to different substrates — molecular graphs vs. knowledge graphs.

## Lean Formal Verification of the Deployment Funnel

The deployment funnel's correctness can be stated as a refinement property in Lean 4:

```lean
-- Each stage refines the previous: the set of compounds 
-- surviving stage n+1 is a subset of those surviving stage n,
-- and the scoring at stage n+1 has lower expected error.
structure Stage where
  name : String
  throughput : Nat          -- compounds per hour
  error_mue : Float         -- mean unsigned error (kcal/mol)
  water_model : WaterModel

def refines (s1 s2 : Stage) : Prop :=
  s2.throughput < s1.throughput ∧ s2.error_mue < s1.error_mue

-- The funnel is a chain of refinements:
theorem funnel_valid :
  refines glide_sp glide_xp ∧
  refines glide_xp glide_ws ∧
  refines glide_ws nwat_mmgbsa ∧
  refines nwat_mmgbsa abfep := ...
```

The jump-cannon engine registry satisfies an identical property: `refines engine_a engine_b` when engine_b produces a lower-stress layout than engine_a on the same graph, at higher computational cost.

## Topological Insights from the Docking → Graph Mapping

### The Deep Analogy: Binding Site = Subgraph

The binding site of a protein is a **subgraph** — a localized region of the protein's interaction network where residues (nodes) are connected by spatial proximity and hydrogen-bond networks (edges). The ligand is a new node seeking to minimize its energy in this subgraph.

This reframes every docking technique as a graph algorithm:

| Docking Concept | Graph Analog |
|---|---|
| Binding site | Induced subgraph of protein interaction network |
| Ligand pose | Node placement in 3D embedding of that subgraph |
| Water molecule | Transient node with dynamic edges to protein/ligand |
| Desolvation penalty | Cost of breaking existing node connections |
| Hydrogen bond | Weighted edge with angular constraint |
| Water bridge | Path of length 2 (protein–water–ligand) |
| Hydrophobic enclosure | Clustering coefficient of the ligand's neighborhood |
| Magic methyl effect | Single-node insertion that reconfigures local topology |

### WaterMap as Graph Coarsening

WaterMap's Grand Canonical Monte Carlo simulation identifies "hydration sites" — positions where water molecules are thermodynamically favorable. This is **graph coarsening**: the continuous 3D density of water positions is discretized into a set of "super-nodes" (hydration sites) that represent the aggregate behavior of many water molecules.

In jump-cannon, `coarsen()` and `topo_fisheye::coarsen()` perform exactly this operation on graph topology. The WaterMap scoring function (ΔG_hyd for each hydration site) is analogous to the edge-strength metrics (`EdgeStrengthKind::Jaccard`, `EdgeStrengthKind::CorrectedOverlap`) that score each edge's "embeddedness" in its local neighborhood.

### The cpptraj closest Command as Nearest-Neighbor Search

The Nwat-MMGBSA protocol's central operation is:

```bash
cpptraj -p topology.prmtop << EOF
trajin trajectory.nc
closest N :LIG closestout closest.dat
EOF
```

This selects the N water molecules closest to the ligand in each frame. Algorithmically, this is a **nearest-neighbor search** over a dynamic point set.

Jump-cannon implements this exact operation in two places:
1. **`octree.wgsl`**: The fully-GPU Barnes-Hut octree that sorts bodies by Morton order and supports stackless nearest-neighbor traversal
2. **`geometric_bonding_gpu.rs`**: The GPU dynamic-edge bonding stage that uses a 3D spatial hash grid (cell-based neighbor stencil) to find candidate bonding pairs — identical to finding "close waters" in a 27-cell stencil

The `geometric_bonding_gpu` module's design is especially relevant: it splits work into GPU-parallel candidate generation (the heavy O(n·27) geometry pass) and host-serial deterministic bond decisions. This is the same seam Nwat-MMGBSA splits: GPU-parallel MD trajectory generation, then host-serial MM-GBSA energy evaluation.

### GPU Multilevel as MM-GBSA Ensemble Averaging

The Nwat-MMGBSA protocol averages MM-GBSA energies over an ensemble of MD snapshots. This reduces the variance from any single frame's instantaneous configuration.

Jump-cannon's `multilevel.wgsl` shader performs **GPU multilevel coarsening**: it builds a cascade of coarser graphs by heavy-edge matching (HEM), lays out the coarsest level, and prolongs back down. This is ensemble averaging in graph space: each level captures the structure at a different resolution, and the prolongation step interpolates between them.

The key insight: both are **variance-reduction techniques**. MD ensemble averaging reduces the variance of a single energy estimate; multilevel coarsening reduces the variance of a single layout solution (local minima are less severe at coarse levels).

## Performance Benchmarks: Cross-Domain Metrics

### Accuracy-Performance Tradeoff Curves

The research document reports specific benchmarks:

| Method | ROC AUC | Throughput (ligands/hr) | Cost per ligand |
|---|---|---|---|
| Glide SP | 0.65 | 1,800 | ~0.5s |
| Glide XP | 0.75 | 360 | ~10s |
| Glide WS | 0.80 | 60 | ~60s |
| Nwat-MMGBSA | 0.85+ | 0.5–2 | ~2 hrs |
| ABFEP+ | 0.90+ | 0.01–0.05 | ~24 hrs |

**Jump-cannon parallel engine benchmarks** (from `docs/layout-algorithms.md` and inline benchmarks):

| Engine | Stress (lower=better) | Nodes/sec | Memory | 
|---|---|---|---|
| `cpu-spring` | baseline | ~10K | O(n) |
| `fa2-brute` | good | ~5K (GPU) | O(n²) |
| `fa2-bh` | same as brute | ~50K (GPU) | O(n log n) |
| `sgd-stress` | lower stress | ~1K (CPU) | O(k·n) |
| `geometric` | best (crystalline) | ~500 (CPU) | O(n²·k) |
| `multilevel(fa2-bh)` | lower stress + faster | ~100K (GPU) | O(n log n) |

The curves have identical shape: each step up in accuracy costs roughly an order of magnitude in throughput.

### The "Water Penalty" as Edge-Strength Damping

One of the most elegant parallels: the thermodynamic penalty for displacing a conserved water molecule maps directly to jump-cannon's **edge-strength damping**. In both cases:

- **Docking**: A ligand that tries to displace a low-energy water pays a penalty proportional to the water's ΔG_hyd. The modified scoring function is: `Score_final = Score_dock - Σ ΔG_hyd(displaced_waters)`
- **Jump-cannon**: An edge with low structural strength (Jaccard ≈ 0) acts as a weak spring between clusters. The force layout's effective spring constant is: `k_eff = k_base × edge_strength`

In both domains, the penalty/weighting term comes from a **local neighborhood analysis** that determines whether the interaction (water position / graph edge) is structurally embedded or incidental.

## References

- Maffucci et al. (2018). "An Efficient Implementation of the Nwat-MMGBSA Method to Rescore Docking Results in Medium-Throughput Virtual Screenings." *Front. Chem.* **6**:43.
- Harder et al. (2016). "Evaluation and Comparison of the WScore Method for Docking and Scoring with Explicit Waters." *J. Chem. Inf. Model.* **56**(11): 2338–2352.
- Murphy et al. (2016). "WScore: A Flexible and Accurate Treatment of Explicit Water Molecules in Ligand–Receptor Docking." *J. Med. Chem.* **59**(9): 4364–4384.
- Gansner, Koren, North (2004). "Topological Fisheye Views for Visualizing Large Graphs." *IEEE InfoVis 2004*. — The §4–§6 multilevel pipeline shared by both domains.
- Zhong et al. (2023). "t-FDP: A Student-t-based Force-Directed Graph Layout Algorithm." arXiv:2303.03964. — The force law behind jump-cannon's `RepulsionMode::NegativeSampling`.
- Jacomy et al. (2014). "ForceAtlas2, a Continuous Graph Layout Algorithm for Handy Network Visualization." *PLOS ONE* **9**(6):e98679. — The adaptive-speed controller behind `fa2_speed.rs`.
