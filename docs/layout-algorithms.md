# Graph-layout algorithm families

A survey of bleeding-edge and classic graph-layout algorithms, scoped to **what
we can run on a wgpu/WGSL compute backend that should scale horizontally**. The
goal is a menu of algorithms that differ along two axes the user cares about:

1. **Visual behavior** — does it *look* different from ForceAtlas2?
2. **Performance / scaling** — complexity, GPU-friendliness, shardability.

> Provenance: seeded by a verified deep-research pass (110 agents, 27 sources,
> 25 adversarially-verified claims, 0 refuted). Citations are inline and
> collected under [References](#references). Where a claim was *not* verified by
> a primary source it is marked **(unverified)**.

---

## TL;DR — what to build, in priority order

| # | Algorithm | Family | Complexity | Visual behavior vs FA2 | GPU | Shards | Effort |
|---|---|---|---|---|---|---|---|
| 1 | **Barnes-Hut FA2** | force-directed | O(n log n) | *Identical* to FA2, just 100×+ faster | ★★★ | ★★ | Low — tree already host-built in `graph-layouts/octree.wgsl` |
| 2 | **SGD stress** (`s_gd2`) | stress | O(n²) full / **O(kn) pivot** | **Very different** — honors shortest-path distances, untangles structure | ★★★ | ★★★ | Medium |
| 3 | **Multilevel wrapper** (sfdp / FM³ / Walshaw) | multiscale | O(n log n) | Sharpens *any* inner solver; better global structure | ★★ | ★★ | Medium — coarsening exists in `coarsen.rs` |
| 4 | **maxent-stress** | stress + entropy | O(n log n) w/ BH | Even node spread, fewer clumps than stress | ★★ | ★★ | Medium |
| 5 | **PivotMDS** | DR / spectral | O(k²n + k³) ~one-shot | Fast global skeleton; great *seed* for 1–4 | ★★ | ★ | Low–Med |
| 6 | **tsNET / t-SNE-style** | DR / embedding | O(n log n) w/ BH | **Cluster-emphasizing** — blobs, very distinct | ★★ | ★ | High |

★ = poor, ★★ = workable, ★★★ = excellent. "Shards" = suitability for the
distributed model in [`compute-architecture.md`](compute-architecture.md).

**Recommended first three:** (1) Barnes-Hut FA2 is the lowest-risk perf win and
reuses the octree we already have; (2) SGD stress is the highest-value *new look*
and scales via pivots; (3) the multilevel wrapper multiplies the quality of both.

---

## The one reference that matches our exact stack

Almost every high-performance graph-layout implementation in the literature is
**CUDA, not WGSL** — GPUGraphLayout, tsne-cuda, the Burtscher-Pingali kernels,
exaFMM. Porting them means re-deriving the algorithm in WGSL, not copying code.

The single direct precedent is **GraphWaGu** (harp-lab, IEEE PacificVis 2022): a
force-directed graph-layout system written in **wgpu + WGSL**, running in the
browser, built on **CSR adjacency buffers** with a WGSL Barnes-Hut repulsion
pass — i.e. our exact architecture. Treat it as the reference implementation for
WGSL buffer layout and tree traversal patterns.
[[GraphWaGu repo]][gw-repo] · [[paper]][gw-paper]

---

## 1. Force-directed acceleration (n-body repulsion)

Force-directed layouts (FA2, Fruchterman-Reingold, spring embedders) all spend
their time on **all-pairs repulsion**. Our current `WgpuSim` does this
brute-force: O(n²) per step. The three standard accelerations:

### Barnes-Hut (θ-criterion tree) — **do this first**
Build a spatial tree (quadtree 2D / octree 3D). For each node, treat any tree
cell whose `size / distance < θ` as a single aggregate body. Cuts repulsion to
**O(n log n)**.

- **Update rule:** unchanged force law; repulsion summed over accepted tree
  cells (center-of-mass) instead of individual nodes.
- **GPU:** the canonical GPU construction is Burtscher & Pingali's 6-kernel
  CUDA pipeline (build, COM, sort, force, integrate). [[Burtscher-Pingali]][bp11]
  Our `graph-layouts/octree.wgsl` already follows it (host-built tree today;
  GPU build is "one Rust change away" per its own header comment).
- **Shards:** ★★ — tree is global; in a distributed setting each worker needs a
  coarse copy of remote COMs (see distributed §5).
- **Visual:** *identical* to FA2. This is a pure speedup, not a new look.
- **Verdict:** lowest-risk win. Reuse the existing octree.

### Fast Multipole Method (FMM) / FM³'s multipole-only variant
FMM uses multipole expansions for far-field forces → **O(n)** in principle.
FM³ keeps **only the multipole coefficients** on a tree, giving O(n log n) plus
edge work; a GPU implementation laid out hundreds of thousands of nodes in
seconds, **20–60× over CPU FM³** (2008-era hardware — treat as directional, not
absolute). [[layoutgpu / mgarland]][layoutgpu]

- **GPU:** harder than Barnes-Hut (expansion math, more kernels). Multi-GPU FMM
  is well studied (exaFMM). [[exaFMM]][exafmm]
- **Verdict:** high effort; only worth it past ~1M nodes where θ-BH's log factor
  bites. Barnes-Hut first.

### Grid / PIC / FFT repulsion
Bin nodes into a uniform grid, convolve with the force kernel via FFT
("particle-in-cell"). This is what **FIt-SNE** does. **O(n)** but grid-resolution
sensitive. The browser-portable win here is via t-SNE-style layouts (§4), not
classic force-directed.

---

## 2. Multilevel / multiscale (a *wrapper*, not a standalone)

Coarsen the graph into a hierarchy → lay out the coarsest level → interpolate
positions down → refine at each level. **Solver-agnostic**: it wraps FA2, stress,
or maxent. This is how sfdp, FM³, and Walshaw reach millions of nodes.

- **Coarsening strategies:**
  - **Edge-collapse / matching** (Walshaw) — contract matched edges each level. [[Walshaw]][walshaw]
  - **Solar-system / galaxy** (FM³) — partition into sun+planet clusters. [[FM³ / zaik2006]][fm3]
  - **Algebraic / weighted interpolation** (Hu sfdp). [[sfdp]][sfdp]
- **Complexity:** O(n log n) overall when paired with a tree-accelerated inner
  solver.
- **Distributed:** coarsen *locally* per partition, solve the (small) top level
  on one worker, broadcast, then refine locally. Composes cleanly with §5.
- **We already have coarsening:** `graph-layouts/src/layout/coarsen.rs`
  (`coarsen` / `prolong` / `cpu_fr_layout`) and the topo-fisheye hierarchy
  builder. A multilevel wrapper should reuse these, not reinvent them.
- **Visual:** sharper global structure, fewer tangles, faster convergence than
  flat FA2.

### maxent-stress (multilevel quality leader)
Yifan Hu's maxent-stress augments the stress objective with an **entropy term**
that spreads nodes evenly, avoiding the clumping plain stress produces. Solved by
a **force-augmented majorization**; the repulsive/entropy term is computed with
Barnes-Hut → **O(n log n)**. [[maxent]][maxent]

- **Visual:** even node distribution, "breathing room" — distinct from both FA2
  and plain stress.

---

## 3. Stress-based layout — **the highest-value new look**

Stress layouts minimize
`stress(X) = Σ_{i<j} w_ij (‖x_i − x_j‖ − d_ij)²`
where `d_ij` is the graph-theoretic shortest-path distance and `w_ij = d_ij^-2`.
The layout *honors graph distances*, so it untangles structure that FA2 leaves
clumped — a recognizably different result.

### Stress majorization (Gansner / Koren / North) — the monotonic classic
Iteratively minimizes a quadratic majorizer of the stress; each iteration solves
a system with a **constant weighted Laplacian `L_w`**. **Monotonic** decrease
guaranteed. Sparse-stress variants scale to larger graphs. [[GKN04]][gkn04]

- **GPU:** each majorization step is a sparse mat-vec — GPU-friendly but needs a
  shortest-path / pivot precompute.

### SGD stress (`s_gd2`, Zheng / Pawar / Goodman) — **recommended**
Reformulate stress minimization as **stochastic gradient descent over node
pairs**: sample one pair `(i,j)` per step, move *both* nodes along the gradient
by a annealed step size. Reaches **lower stress, faster, and is far less
sensitive to initialization** than majorization. [[s_gd2]][sgd]

- **Complexity:** O(n²) for full pairs; **O(kn) with pivot/sparse stress**
  (Ortmann et al.) — only compute against `k` landmark pivots.
- **GPU/shard:** ★★★ — pairs are independent; trivially parallel; pivot set is a
  small broadcast. Best-scaling *distinct* solver in this doc.
- **Caveat:** no monotonic-decrease guarantee (it's stochastic); reaches local
  minima. In practice converges well with a standard annealing schedule.

### (SGD)² — multi-criteria
Generalizes `s_gd2` to optimize **any differentiable drawing criterion**
(stress + crossing-angle + neighborhood-preservation + …) via autodiff-style
SGD. [[SGD²]][sgd2] Useful later for "tune the aesthetic"; not a day-one target.

### Low-rank / sparse stress for scale
Low-rank stress majorization and pivot/landmark MDS approximate the full stress
with a small set of pivots → near-linear. [[low-rank SM]][lowrank] This is the
mechanism that makes stress viable past ~50k nodes.

---

## 4. Dimensionality-reduction / embedding layouts

Treat layout as projecting a high-dimensional graph metric into 2D/3D.

### PivotMDS — fast global skeleton / great seed
Classical MDS restricted to a **k × n** submatrix of distances to `k` pivots;
one eigendecomposition. Near **one-shot**, O(k²n + k³). [[PivotMDS]][pivotmds]

- **Best use:** *seed* for FA2 / stress / maxent instead of a random ring — kills
  most of the slow global-untangling phase. Cheap, high leverage.

### tsNET / t-SNE / UMAP-style — cluster-emphasizing, very distinct
tsNET runs t-SNE on graph-theoretic distances; emphasizes **cluster separation**
— produces "blobs" visually unlike any force-directed result. [[tsNET]][tsnet]
GPU t-SNE (tsne-cuda) hits **up to 1200× sklearn** but is CUDA; the FIt-SNE
algorithm is GPU-portable via the grid/FFT trick (§1). [[tsne-cuda]][tsnecuda]

- **Verdict:** highest "looks different" payoff, highest implementation cost
  (perplexity calibration, KL-gradient, FFT repulsion). A later milestone.

---

## 5. Distributed / horizontal scaling

> **Least-verified section** — the research pass under-covered the distributed
> angle relative to the algorithm angles. The communication pattern below is
> consistent across the cited distributed-layout and multi-GPU FMM sources plus
> standard BSP graph-processing practice, but treat specifics as a design
> starting point, not settled fact.

The pattern is consistent across the literature:

```
1. PARTITION   CSR → P blocks (edge-cut, METIS-style: minimize cross-partition edges)
2. GHOST       each worker owns its vertex block + read-only "ghost" copies of
               the boundary neighbors owned by other workers
3. SUPERSTEP   (BSP / bulk-synchronous):
                 a. compute local forces (intra-block + ghost contributions)
                 b. integrate local node positions
                 c. exchange ONLY boundary/ghost positions with neighbors
                 d. barrier; repeat
```

- **Edge-cut vs vertex-cut:** edge-cut (partition vertices, replicate boundary
  edges) is the natural fit for force layout — each vertex has one owner, ghosts
  carry positions. Vertex-cut suits power-law graphs but complicates position
  ownership.
- **Communication = boundary positions only.** Our `PositionDelta` wire format
  (raw LE f32) is already the right shape for halo exchange — see
  [`compute-architecture.md`](compute-architecture.md).
- **Far-field forces** (Barnes-Hut/FMM) need each worker to also hold a *coarse*
  copy of remote centers-of-mass. Multi-GPU FMM does exactly this. [[exaFMM]][exafmm]
- **Precedent:** distributed force-directed layout [[distributed-fdl]][distfdl];
  out-of-core / distributed graph systems for very large graphs [[Jia et al.]][jia].

**Which algorithms shard best:** SGD stress (independent pairs) > Barnes-Hut FA2
(needs COM exchange) > FMM (heavy expansion exchange) > DR-embedding (global
eigendecomposition resists partitioning).

---

## How these map onto the existing code

| Already in-repo | Reuse for |
|---|---|
| `graph-layouts/.../octree.wgsl` + host octree | Barnes-Hut FA2 (#1), maxent repulsion (#4) |
| `graph-layouts/src/layout/coarsen.rs` (`coarsen`/`prolong`) | Multilevel wrapper (#2) |
| `graph-compute/src/topo_fisheye` hierarchy | Coarsening source of truth |
| `StaticLayout`/`PhysicsLayout` + `Dyn*` traits | Registry surface for all of the above (see arch doc) |
| `PositionDelta` / CSR wire format | Halo exchange (#5) |

Caveats carried from verification: GPU FM³ numbers are 2008-era; SGD has no
monotonic guarantee; stress/maxent reach local minima; all cited GPU *code* is
CUDA except GraphWaGu.

---

## References

Quality marked as classified by the research pass (primary = paper/official repo).

- [gw-repo] GraphWaGu — WebGPU/WGSL graph layout (harp-lab). <https://github.com/harp-lab/GraphWaGu>
- [gw-paper] GraphWaGu, IEEE PacificVis 2022. <https://sidharthkumar.io/publications/pacificVisGraphWagu.pdf> · <https://stevepetruzza.io/pubs/graphwagu-2022.pdf>
- [bp11] Burtscher & Pingali, "An Efficient CUDA Implementation of the Tree-based Barnes-Hut n-Body Algorithm" (2011). <https://iss.oden.utexas.edu/Publications/Papers/burtscher11.pdf>
- [layoutgpu] Godiyal/Hoberock/Garland et al., GPU multipole graph layout. <https://mgarland.org/files/papers/layoutgpu.pdf>
- [exafmm] Yunis, Yokota, Ahmadia, multi-GPU/distributed FMM. <https://www.bu.edu/exafmm/files/2012/02/YunisYokotaAhmadia2012.pdf>
- [walshaw] Walshaw, "A Multilevel Algorithm for Force-Directed Graph Drawing." <https://chriswalshaw.co.uk/papers/fulltext/WalshawTR6000.pdf>
- [fm3] Hachul & Jünger, FM³ (Fast Multipole Multilevel Method). <https://kups.ub.uni-koeln.de/54892/1/zaik2006-509.pdf>
- [sfdp] Hu, "Efficient and High Quality Force-Directed Graph Drawing" (sfdp multiscale). <http://yifanhu.net/PUB/graph_draw.pdf>
- [maxent] Gansner, Hu, North, "A Maxent-Stress Model for Graph Layout." <http://yifanhu.net/PUB/maxent.pdf>
- [gkn04] Gansner, Koren, North, "Graph Drawing by Stress Majorization." <https://graphviz.org/documentation/GKN04.pdf>
- [sgd] Zheng, Pawar, Goodman, "Graph Drawing by Stochastic Gradient Descent" (`s_gd2`). <https://arxiv.org/abs/1710.04626>
- [sgd2] Ahmed et al., "(SGD)²: multi-criteria graph drawing by SGD." <https://arxiv.org/abs/2008.07799>
- [lowrank] Khoury et al. / Hu, low-rank stress majorization. <http://yifanhu.net/PUB/lowrank_sm.pdf>
- [pivotmds] Brandes & Pich, "Eigensolver Methods for Progressive Multidimensional Scaling" (PivotMDS), GD 2006. <https://dblp.org/rec/conf/gd/BrandesP06.html>
- [tsnet] Kruiger et al., "Graph Layouts by t-SNE" (tsNET), EuroVis 2017. <http://www2.cs.arizona.edu/~kobourov/tsne-eurovis17.pdf>
- [tsnecuda] Chan et al., tsne-cuda. <https://github.com/CannyLab/tsne-cuda>
- [distfdl] Distributed force-directed graph layout and visualization. <https://www.researchgate.net/publication/262400359_Distributed_force-directed_graph_layout_and_visualization>
- [jia] Jia et al., out-of-core/distributed graph processing, VLDB. <http://www.vldb.org/pvldb/vol11/p297-jia.pdf>
- GPUGraphLayout — GPU-only Barnes-Hut ForceAtlas2. <https://github.com/govertb/GPUGraphLayout>
