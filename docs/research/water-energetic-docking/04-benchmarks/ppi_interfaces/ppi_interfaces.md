# PPI Interfaces: Scaling to Large, Solvent-Exposed Interfaces

## The PPI Scaling Challenge

Protein-Protein Interaction interfaces present a qualitatively different problem from enzyme active sites:

| Property | Enzyme Active Site | PPI Interface |
|---|---|---|
| Surface area | 300–800 Å² | 1500–3000 Å² |
| Geometry | Deep pocket, concave | Flat, extended |
| Solvent exposure | Partially buried | Highly solvent-exposed |
| Water count | 5–30 ordered waters | 60–200+ waters |
| Water ordering | Structurally conserved | Dynamic, bulk-like |
| Binding energy hot spots | Concentrated (catalytic residues) | Distributed ("hot spot" residues) |

The Rac1-Tiam1 interface is a representative PPI target. Standard rescoring fails because:
1. The binding "site" is ambiguous — where does the interface end and bulk solvent begin?
2. Water-mediated interactions are numerous but individually weak
3. The solvent environment is broad and dynamic

## The Nwat Scaling Solution

Maffucci et al. found that PPI interfaces require **Nwat = 60–100** to achieve meaningful discrimination between actives and inactives, compared to Nwat = 20–40 for enzyme active sites. The architectural insight is that the water shell must be large enough to capture the extended hydration environment — the "effective binding site" includes second- and third-shell waters that are thermodynamically coupled to the direct interface waters.

## Jump-Cannon Parallel: NegativeSampling for Large Graphs

The PPI interface problem — "how do you handle a large, diffuse interaction region?" — is jump-cannon's exact problem at vault scale. The `NegativeSampling` repulsion backend is designed for this regime:

```rust
// gpu_force.rs: NegativeSampling for large graphs
// - K random partners per node per step (default K=8)
// - Cost: O(n·K), independent of spatial density
// - With t-FDP: degree-weighted repulsion for hubs
// - The SNAP-tFDP proof (arXiv:2608.01907, eq. 6):
//   E[repulsion_i] = (1/n) Σ_j repulsion_true(i,j)  [unbiased estimator]
```

The parallel to Nwat-MMGBSA is exact:

| PPI Challenge | Jump-Cannon Solution |
|---|---|
| Large interface (many waters) | Many nodes (vault scale) |
| Need representative water sampling | NegativeSampling with K=8 |
| Waters are individually weak but collectively important | t-FDP force law (bounded at short range) |
| Cannot compute all water-water interactions | Cannot compute all n² repulsion pairs |
| Nwat=60–100 is the sweet spot | K=8 samples/step is the sweet spot |

### Why K=8 Works (and Nwat=80 Works)

In both cases, the sample count is chosen to provide **statistical coverage** of the full ensemble:

- **Nwat-MMGBSA**: N=80 waters sampled from ~200+ interface waters. The MM-GBSA average over these 80 provides a representative thermodynamic profile of the full hydration environment.
- **NegativeSampling**: K=8 partners sampled from n nodes. The t-FDP repulsion with degree weighting and the force accumulation across steps provides a representative force profile of the full all-pairs repulsion.

Both are **statistical estimators** whose variance decreases with sample count and increases with the heterogeneity of the sampled population. The "sweet spot" (Nwat=80, K=8) balances variance reduction against computational cost.

## Topological Fisheye as PPI Interface Analysis

The `topo_fisheye` module (Gansner-Koren-North §4) provides a framework for analyzing PPI-like problems in graph space: it identifies which parts of a graph are "locally dense" (like a binding hot spot) and which are "globally connected" (like the extended PPI interface).

```rust
// topo_fisheye.rs: the multilevel coarsening pipeline
// Candidate set = graph edges ∪ filtered Delaunay edges (proximity graph)
// Scoring: weighted combination of proximity, cluster-size penalty,
//   connection strength, neighborhood Jaccard, inverse degree
// 
// PPI analog:
// - "Hot spot" residues = nodes with high connection strength + high Jaccard
// - "Interface periphery" = nodes with high proximity but low Jaccard
// - "Bulk solvent" = nodes dropped from the candidate set entirely
```

The `CoarsenParams::gt_max_hops` parameter (graph-theoretic distance filter for Delaunay edges) is the direct analog of the Nwat cutoff: it controls how far the "effective interface" extends beyond the direct contact region.
