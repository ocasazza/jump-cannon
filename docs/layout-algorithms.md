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
| 1 | **Barnes-Hut FA2** | force-directed | O(n log n) | *Identical* to FA2, just 100×+ faster | ★★★ | ★★ | **Landed** — tree built on the GPU in `graph-layouts/octree.wgsl` |
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
  Our `graph-layouts/octree.wgsl` builds the tree on the GPU (Morton sort +
  level-linear emission; see "Scale ladder" below).
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

## Scale ladder — measured ceilings and what is beyond them (2026-09)

Second research pass, every number below read from the cited paper's own
text (search engines hallucinated several arXiv IDs during this pass; none of
those are cited). Headline: **no published work lays out 10⁹ nodes.** The
largest *measured* layouts are ~10⁷; 10⁸–10⁹ exists only as high-dimensional
node embeddings (which still need a projection step) or as aggregate /
community layouts.

| Paper | What it measured |
|---|---|
| [2409.00876] GPU pangenome layout (SC'24) | path-guided SGD on CUDA; human Chr.1 **11.1 M nodes**, 6 B pair updates/iter; **57.3×** over multithreaded CPU ODGI; RTX A6000 + A100; profile is memory-bound |
| [2608.01907] SNAP-tFDP (2026) | degree-weighted t-FDP + edge-centric negative sampling, O(\|E\|·k); **com-lj 4 M nodes / 34 M edges in 9.3 s using 0.8 GB VRAM**; lock-free "bundle by source node" parallelism |
| [2303.03964] t-FDP (TVCG'23) | Student-t force, FFT-interpolated repulsion; 1 order faster than DRGraph on CPU, **2 orders on GPU** (RTX 2080) |
| [2108.00529] BigGraphVis | Count-Min sketch + GPU SCoDA communities, then FA2 on the aggregate; **3 M nodes / 34 M edges ≈ 5 min** |
| [2002.08233] BatchLayout | Flan_1565 **1.56 M / 114 M edges**, 5000 iters in 49 min on CPU (FA2-BH OOM'd) |
| [2008.07799] DRGraph (VIS'20) | sparse BFS distance + negative sampling + multilevel; Flan_1565 823 s → 171 s on 8 threads |
| [2008.12336] GOSH · [2110.10049] | **65 M vertices / 1.8 B edges on one GPU in < 1 h / < 30 min** — coarsening-based *embedding*, not layout |
| [1903.00757] GraphVite · [1903.12287] PBG | 66 M / 1.8 B on 4 GPUs in ~20 h; PBG Freebase model 48.5 GB, partitioning −88 % memory |
| [1506.06745] GraphMaps | LOD tiles, ≤ 60 nodes per tile in their setting; 38 k nodes took < 6 h preprocessing (edge routing bound) |
| [0907.2585] GMap (GD'09) | embed → cluster → Delaunay/Voronoi → merge cells per cluster; O(\|V\| log \|V\|); **440 k vertices in 4 min** with n_r=\|V\| random + n_a=40\|V\| artificial points |
| [1804.03329] hyperbolic tradeoffs | f32 "struggles"; perfect MAP needed a **512-bit** solver — Riemannian hyperbolic optimisation is not a WGSL job |

### Hard walls in this engine (verified against wgpu-types 23 + the WebGPU spec)

Fixed in the same change that added this section:

- **Dispatch cap.** `maxComputeWorkgroupsPerDimension = 65535` × 64 lanes =
  4 194 240 invocations. A 1-D `dispatch_workgroups(ceil(n/64),1,1)` was
  rejected above that. `dispatch_1d` now spills into Y and every kernel
  recovers its index through `linear_index()`.
- **Buffer caps.** `Limits::downlevel_defaults()` pins storage bindings to
  128 MiB and buffers to 256 MiB; `using_resolution` only lifts texture
  limits. That capped positions at 8.4 M nodes and CSR at 16.7 M edges.
  `gpu_force_device_limits()` now asks for the adapter's buffer limits (both
  the owned device and the renderer's device use it).
- **Dead voxel grid.** `RepulsionMode::Grid` had fallen through to the naive
  O(n²) loop after its bindings were dropped for the 10-storage-buffer cap,
  and `from_str` sent every unknown string there. The grid build is removed;
  mode 0 is now the honest `Exact` reference, and unknown strings fall back
  to the default backend.

### 10⁹-node budget with this engine's buffer layout

positions vec4 16 B + velocities 16 B + energy 4 B + CSR offsets 4 B +
CSR neighbours 2m·4 B (avg degree 10 → 40 B) + spring partials 16 B =
**96 B/node → 89 GiB at 10⁹**. A 24 GiB GPU holds ~268 M nodes at that
layout; a wasm32 heap (4 GiB) holds ~119 M nodes of host mirror at 36 B/node
with no adjacency. Above ~10⁷ the graph must be coarsened and rendered as
aggregates; this is arithmetic, not tuning.

### Landed

- `ForceModel::TFdp` in both kernels (`spring_step` attraction, all three
  repulsion backends) with the paper defaults α=0.1, β=8, γ=2.
- SNAP-tFDP as a *node-centric* estimator: under `NegativeSampling` + t-FDP
  each sampled pair is weighted `(deg_i + deg_j)/2` and scaled by `tfdp_k/K`,
  which is exactly the expectation the paper's eq. 6 derives for its
  edge-centric sampler. Their "bundle by source node" scheme *is* one thread
  per CSR row — the shape this engine already had — so no racing writes to
  other nodes' positions are introduced (WGSL has no f32 atomics).
- **GPU octree build.** The Barnes-Hut tree is now constructed entirely on the GPU via a 30-bit Morton-key 8-pass 4-bit LSD radix sort with multi-workgroup reduce/scan/fixup exclusive scan, order-preserving u32 float atomics for bounding-box reduction, and prefix-sum caching of mass and mass-weighted position. The kernel pipeline emits nodes in DFS order with next/skip ropes; zero host readback per layout step. Build scratch is ~45 bytes per node plus a 48-byte OctNode array at 2N+16 capacity. Multi-body max-depth leaves with identical Morton keys subtract the body's own contribution via exact cell containment.
- **Region map (GMap-style Voronoi aggregate view).** A GPU seed pass projects nodes through the live camera and writes (packed cell, cluster id) into a 512×512 grid with atomicMin. Ten jump-flood passes (distances 256 down to 1) compute nearest-seed Voronoi; a prune pass makes cells beyond `radius_cells` from any seed transparent; a fullscreen draw fills each cell with a palette color at user-set fill opacity and darkens outlines where 4-neighbor cluster IDs differ. Cluster IDs flow from the Style panel's `community` metric; cost per frame is independent of node count except the seed pass, making this the view for graphs too large for node-by-node rendering.

### Measured on this engine

Benchmarks on an Apple M5 Max (Metal backend), preferential-attachment graphs, mean degree 8, per-step wall time including one position readback per run:

| Nodes / Edges | BH Spring | BH t-FDP | NS Spring | NS t-FDP |
|---|---|---|---|---|
| 100k / 400k | 1.05 ms | 0.97 ms | 0.45 ms | 0.61 ms |
| 1M / 4M | 17.3 ms | 14.0 ms | 8.1 ms | 6.9 ms |

BH = Barnes-Hut; NS = Negative Sampling (K=8); t-FDP defaults k=3.

### Remaining, in dependency order

1. **Multilevel on the device.** `coarsen.rs` is CPU-only and only seeds
   frame 0. GOSH's single-GPU 65 M/1.8 B result is coarsening doing the work.
2. **Out-of-core positions/CSR** (PBG/GOSH partition staging) — only after
   region-map levels converge, since the aggregate view decides what has to be
   resident.

Not planned: pairwise SGD stress inside a compute shader (single-pair
updates conflict; the GPU-viable form is the sampled SGD of [2409.00876]),
and hyperbolic optimisation in f32 (see [1804.03329]); hyperbolic
*rendering* of a host-computed embedding is fine.

Test-harness note: the `graph-layouts` *library* builds wgpu with
`default-features = false` (no Metal/Vulkan/DX12 — native consumers bring
those), and its GPU tests return early as a pass when no adapter exists. Until
this change `cargo test -p graph-layouts` therefore never touched a device, and
`unit_gpu_force_star_hub_stable` had been failing on every real GPU behind
that skip (its hub-near-origin assertion pinned an equilibrium of a
deliberately asymmetric seeding, not the Tigr hub-split contract). A
native-only dev-dependency now enables `metal`/`dx12` for this crate's own
test binaries, and the test asserts what it defends: finite positions, no
stall, leaves bound to the hub. The 4.2 M-node dispatch test stays
`#[ignore]`d (≈1 GB host allocation); run it with
`cargo test -p graph-layouts --release --lib dispatch_past_cap -- --ignored`.

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
- [2409.00876] Rapid GPU-Based Pangenome Graph Layout (SC'24). <https://arxiv.org/abs/2409.00876>
- [2608.01907] SNAP-tFDP: Massively Scalable Graph Layouts via Sparse Negative Sampling. <https://arxiv.org/abs/2608.01907>
- [2303.03964] Force-Directed Graph Layouts Revisited: A New Force Based on the T-Distribution (TVCG 2023). <https://arxiv.org/abs/2303.03964>
- [2108.00529] BigGraphVis. <https://arxiv.org/abs/2108.00529>
- [2002.08233] BatchLayout. <https://arxiv.org/abs/2002.08233>
- [2008.07799] DRGraph (VIS 2020). <https://arxiv.org/abs/2008.07799>
- [2008.12336] GOSH: Embedding Big Graphs on Small Hardware. <https://arxiv.org/abs/2008.12336>
- [2110.10049] Boosting Graph Embedding on a Single GPU. <https://arxiv.org/abs/2110.10049>
- [1903.00757] GraphVite. <https://arxiv.org/abs/1903.00757>
- [1903.12287] PyTorch-BigGraph. <https://arxiv.org/abs/1903.12287>
- [1506.06745] GraphMaps: Browsing Large Graphs as Interactive Maps. <https://arxiv.org/abs/1506.06745>
- [0907.2585] GMap: Drawing Graphs as Maps (Gansner, Hu, Kobourov). <https://arxiv.org/abs/0907.2585> · <https://graphviz.org/documentation/GHK09.pdf>
- [1804.03329] Representation Tradeoffs for Hyperbolic Embeddings. <https://arxiv.org/abs/1804.03329>
- [2506.02219] Stochastic Barnes-Hut Approximation for Fast Summation on the GPU. <https://arxiv.org/abs/2506.02219>
- Gansner & Hu, PRISM node-overlap removal (JGAA 2010) — post-process, not a layout engine. <https://graphviz.org/documentation/GH10.pdf>
