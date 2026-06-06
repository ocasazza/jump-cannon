# Lavender ↔ jump-cannon: hardware-agnostic GPU graph analytics

## 1. Context & decision

**lavender (ingest)** produces the Obsidian vault access/knowledge graph — the `access_graph_edges` and `authz_tuples` that describe who-can-reach-what across the vault. **jump-cannon's `graph-compute`** already runs GPU graph kernels on that *same* vault graph: wgpu/WGSL compute compiled to Metal/Vulkan/DX12, single-buffer packed-CSR uploads, an SpMV-shaped Force-Atlas-2 edge-gather, a distributed CSR/halo scaffold (`partition.rs`), and a CPU analytics oracle (`graph-metrics`: PageRank / Louvain / k-core / betweenness / WCC).

### Decision

**Extend `graph-compute` (wgpu/WGSL). Do NOT build a new CubeCL crate.**

Rationale:

- The CSR upload path, the 12-storage-buffer budget discipline, the ping-pong-buffer iteration pattern, and the gather inner loop **already run on Apple GPU today** (`geometric_gpu.rs`, `geometric_barnes_hut.wgsl`, `sgd_stress_gpu.rs`).
- wgpu is mature and hardware-agnostic by construction — one WGSL source targets Metal, Vulkan, and DX12.
- CubeCL is alpha, has **no sparse support**, and has a known Metal crash surface. Building PageRank/SpMV there would re-derive infrastructure we already have, on a riskier substrate.
- The CPU oracles in `graph-metrics` and `geometric.rs` give us a precise, apples-to-apples validation target — no new correctness reference needs to be invented.

This document covers two tracks against that decision:

- **Part A (near-term):** a CSR-based GPU PageRank / reusable SpMV primitive — a one-shot analytic, validated bit-for-tolerance against the CPU oracle.
- **Part B (scale-out):** `HaloTransport` domain decomposition — sharded PageRank-SpMV under BSP, for graphs that exceed a single device's working set.

---

## 2. Part A — Near-term: GPU PageRank / SpMV kernel

**Area:** GPU-accelerated PageRank + a reusable SpMV primitive in `crates/graph-compute` (wgpu/WGSL), validated against the `graph-metrics` CPU PageRank oracle.

### Design

PageRank is a **one-shot analytic, not a continuous physics sim** — so it is *not* a `LayoutEngine`. It is exposed as a standalone free function:

```rust
pub fn gpu_pagerank(ctx: &EngineCtx, graph: &CsrGraph, damping: f32, iters: u32) -> Result<Vec<f32>, String>
```

mirroring how `gpu_relax_bonds` / `gpu_candidate_pairs` already live as free functions in `geometric_bonding_gpu.rs` rather than behind the `LayoutEngine` trait. Forcing PageRank behind `LayoutEngine` would mean abusing `StepOutput { positions }` to smuggle per-node scalars — the free function is the honest shape and matches existing precedent.

#### What the existing code gives us (cited)

- **CSR layout is settled.** `geometric_gpu.rs:187-192` packs the whole adjacency into ONE storage buffer to respect the 12-storage-buffer budget (`engines/mod.rs:141` sets `max_storage_buffers_per_shader_stage: 12`):
  - `csr[0 ..= n_nodes]` = offsets, each **pre-shifted** by `header = n_nodes+1` so they point directly into the neighbours region.
  - `csr[n_nodes+1 + k]` = the k-th global neighbour id.
  - Neighbours of node `v` are `csr[csr[v] .. csr[v+1]]`. The shader recovers `header` as `csr[0]` because `offsets[0]==0` (`geometric_barnes_hut.wgsl:164-167`).
  - This is exactly the SpMV inner loop. **Reused verbatim.**
- **The CSR is symmetrized/undirected.** `graph-api/src/server.rs:758-759` pushes both `adj[src].push(tgt)` and `adj[tgt].push(src)`; `CsrGraph::path` (`sim.rs:32-46`) is likewise undirected. So in-neighbours == out-neighbours, and a **pull** kernel `r_new[v] = Σ_{u∈N(v)} r[u]/deg(u)` can be evaluated by gathering over v's OWN neighbour list — **race-free, one thread per row, NO atomics.** This is the key reason PageRank is a better first GPU analytic than the scatter (push) form.
- **The CPU oracle(s).**
  - `graph-metrics/src/pagerank.rs:4-45` — the public oracle: **directed** push over `out_neighbors`, damping 0.85, dangling mass spread uniformly, `compute_pagerank(graph, 0.85, 50)`.
  - `geometric.rs:3097-3131` — a CSR-native f32 PageRank with identical math (`next[u]+=rank[v]/out_deg[v]`; `next[v]=teleport+dangling_share+damping*next[v]`). This is the **closest apples-to-apples oracle** since both consume a symmetrized `CsrGraph`, and the precise numeric form the GPU kernel reproduces.
- **Engine wiring patterns to copy.** `sgd_stress_gpu.rs` is the cleanest template: ping-pong buffers (`pos_a`/`pos_b`, `cur` flag, lines 380-399); all sweeps in one encoder with **implicit storage barriers** between adjacent passes (the load-bearing comment at 362-366 — iteration N+1 sees iteration N's writes without a submit per step); `storage_u32`/`storage_f32` helpers that pad empty slices to 1 element (130-148); and `map_async`+`device.poll(Wait)`+`mpsc` readback (402-419). `geometric_bonding_gpu.rs` is the template for "GPU compute that is NOT a LayoutEngine."

#### Module placement

New module `crates/graph-compute/src/analytics/mod.rs` + `analytics/pagerank.rs`, exporting `pub fn gpu_pagerank`. Register `pub mod analytics;` in `lib.rs:29-34` and re-export. **Not** added to `LEAF_ENGINE_IDS` / the registry.

#### SpMV as the reusable primitive

`analytics/spmv.rs`: a generic CSR gather that PageRank's finalize wraps. The WGSL gather loop `for aa in csr[v]..csr[v+1] { acc += contrib(csr[aa]) }` is the SpMV inner loop; PageRank's `contrib(u) = r[u]/deg(u)` is just the plus-times semiring over reals. For v1, SpMV is inlined inside `pagerank.rs` to ship fast, but the WGSL is structured with a clearly-marked `fn spmv_row(v) -> f32` so factoring it into a shared `spmv.wgsl` include is mechanical. The Rust side structures buffer setup as a `CsrCompute` builder others can clone.

#### Two-pass-per-iteration kernel

- **Pass A — `pr_spmv`** (one thread per node): `acc[v] = Σ_{u∈N(v)} r_in[u] / deg[u]`, where `deg[u]` is precomputed host-side as `offsets[u+1]-offsets[u]` and uploaded once. Dangling nodes (deg==0) contribute nothing.
- **Pass B — `pr_finalize`** (one thread per node): `r_out[v] = teleport + damping*dangling_share + damping*acc[v]`, where `dangling_share = (Σ_{u: deg[u]==0} r_in[u]) / n`.

The dangling sum is a **global reduction**. Because **core WGSL has no f32 atomics** (`sgd_stress_gpu.rs:18-26` documents this — atomics are i32/u32 only, no f32 atomic-add), the sum is done via a dedicated `pr_dangling_sum` reduction pass: threadgroup tree-reduction of a masked rank (`r_in[u]*(deg[u]==0)`) into a small `partials` buffer, summed host-side (or by a second tiny pass). Threadgroup scratch is ≤ `workgroup_size` f32 = 256 B at wg=64 — far under Metal's 32 KB threadgroup limit.

Per iteration, in **one command encoder**, ping-ponging `r_a`/`r_b`:

```
pr_dangling_sum (reduction) → pr_spmv (r_in→acc) → pr_finalize (acc→r_out)
```

**Convergence policy:** fixed `iters` (matches the oracle's fixed 50) is the default and the exact-match path; an optional `tol` early-exit is exposed but not used by the parity test.

#### Threadgroup memory + load imbalance (hubs)

- The pull SpMV uses **NO threadgroup memory** in the row-parallel form (each thread owns a row), so the 32 KB limit is irrelevant to the base kernel. The only threadgroup scratch is the dangling reduction (≤256 B).
- **Hub imbalance:** the row-parallel kernel gives one hub-row to a single thread while its warp-mates idle. For v1 (Obsidian vaults: tens of thousands of nodes, modest max degree) row-parallel is fine. The documented escalation path is a **warp-per-row / segment-reduction** kernel `pr_spmv_hybrid`: assign a tile of threads to a hub row and tree-reduce its partial sums in threadgroup scratch (a tile of 256 f32 partials = 1 KB, safe under 32 KB). Gated on a host-side degree threshold that buckets rows into "light" (row-parallel) and "heavy" (warp-per-row) dispatches. This is Phase 2.

#### Directedness reconciliation (correctness subtlety, stated explicitly)

The public oracle (`graph-metrics/pagerank.rs`) is **directed** (out-edges source→target). The `CsrGraph` the GPU consumes is **symmetrized**. The two coincide only when the underlying vault edges are themselves reciprocal. Therefore:

- **Primary numeric gate:** GPU vs the CSR oracle in `geometric.rs` (both symmetrized) → assert `|gpu - cpu| < 1e-4`.
- **Integration gate:** convert the `CsrGraph` to a `VaultGraph`, run the public `graph-metrics::compute_pagerank`, and assert **rank-ORDER agreement** (Spearman / top-k), plus exact match on graphs constructed to be reciprocal.

#### SpMV generalization (cheap follow-ons)

The same `for aa in csr[v]..csr[v+1]` gather is a semiring SpMV:

- **Connected components / label propagation** (min semiring): `label[v] = min(label[v], min_{u∈N(v)} label[u])` — accelerates `graph-metrics/src/wcc.rs` (today a serial union-find, `lib.rs:13`).
- **BFS** (boolean SpMV): `next_frontier[v] = OR_{u∈N(v)} frontier[u]` with a visited mask; level = iteration count.
- **Degree** (all-ones SpMV): trivial, proves the primitive.

All reuse the identical CSR buffer + dispatch scaffold; only the per-edge combine + finalize change.

### Files to create or modify

| Path | Change |
|---|---|
| `crates/graph-compute/src/analytics/mod.rs` | **NEW** — `pub mod pagerank; pub mod spmv; pub use pagerank::gpu_pagerank;` + a shared `CsrCompute` buffer builder (packs the single CSR buffer per `geometric_gpu.rs:187-192`, precomputes `deg`, owns device/queue handles) |
| `crates/graph-compute/src/analytics/pagerank.rs` | **NEW** — `gpu_pagerank(...)` builds pipelines for `pr_dangling_sum`/`pr_spmv`/`pr_finalize`, ping-pongs `r_a`/`r_b`, reads back ranks. Mirrors `sgd_stress_gpu.rs`. Includes `#[cfg(test)]` parity tests vs `geometric::pagerank` |
| `crates/graph-compute/src/analytics/spmv.rs` | **NEW** — reusable `SpmvKernel` wrapper + doc-marked semiring extension points (min-plus for CC/BFS); v1 may re-export the gather used by pagerank |
| `crates/graph-compute/src/shaders/pagerank.wgsl` | **NEW** — three entry points `pr_dangling_sum` (threadgroup tree-reduction into partials), `pr_spmv` (row-parallel pull gather; contains `fn spmv_row(v)->f32`), `pr_finalize`. Bindings: `csr`(ro), `deg`(ro), `r_in`(ro), `acc`(rw), `r_out`(rw), `partials`(rw), `params`(uniform) |
| `crates/graph-compute/src/lib.rs` | **MODIFY** lines 29-46: add `pub mod analytics;` and `pub use analytics::gpu_pagerank;` |
| `crates/graph-compute/Cargo.toml` | **MODIFY** — add `graph-metrics` and `vault-data` as **DEV-dependencies only** (the cross-oracle test needs `compute_pagerank` over a `VaultGraph`; the lib itself stays metrics-free) |
| `crates/graph-compute/tests/gpu_pagerank.rs` | **NEW** — small-graph correctness vs both oracles + millions-scale finite/ordering scale test. Follows the `gpu_or_skip` + `GPU_TEST_LOCK` pattern from `tests/gpu_engines.rs` |

### WGSL sketch — `src/shaders/pagerank.wgsl`

```wgsl
struct PrParams {
    n_nodes: u32,
    damping: f32,
    inv_n: f32,
    _pad: u32,
};

// Single packed CSR buffer — IDENTICAL layout to geometric_barnes_hut.wgsl:
//   csr[0 ..= n]   = offsets pre-shifted by (n+1)
//   csr[n+1 + k]   = neighbour ids;  neighbours of v = csr[csr[v]..csr[v+1]]
@group(0) @binding(0) var<storage, read>       csr:      array<u32>;
@group(0) @binding(1) var<storage, read>       deg:      array<f32>;   // out-degree, 0 for dangling
@group(0) @binding(2) var<storage, read>       r_in:     array<f32>;
@group(0) @binding(3) var<storage, read_write> acc:      array<f32>;
@group(0) @binding(4) var<storage, read_write> r_out:    array<f32>;
@group(0) @binding(5) var<storage, read_write> partials: array<f32>;   // dangling-sum partials, len = n_workgroups
@group(0) @binding(6) var<uniform>             p:        PrParams;

const WG: u32 = 64u;

// ---- SpMV inner loop (the reusable primitive) -------------------------------
// Pull form on a symmetrized CSR == Σ over in-neighbours, reproducing the CSR
// oracle in geometric.rs:3097 with NO scatter / NO atomics.
fn spmv_row(v: u32) -> f32 {
    let beg = csr[v];
    let end = csr[v + 1u];
    var s = 0.0;
    for (var aa: u32 = beg; aa < end; aa = aa + 1u) {
        let u = csr[aa];
        let du = deg[u];
        if (du > 0.0) { s = s + r_in[u] / du; }
    }
    return s;
}

@compute @workgroup_size(WG)
fn pr_spmv(@builtin(global_invocation_id) gid: vec3<u32>) {
    let v = gid.x;
    if (v >= p.n_nodes) { return; }
    acc[v] = spmv_row(v);          // hub rows: single thread (v1). Phase-2: warp-per-row.
}

// ---- Dangling-mass reduction (no f32 atomics → threadgroup tree-reduce) ------
var<workgroup> scratch: array<f32, WG>;
@compute @workgroup_size(WG)
fn pr_dangling_sum(@builtin(global_invocation_id) gid: vec3<u32>,
                   @builtin(local_invocation_id)  lid: vec3<u32>,
                   @builtin(workgroup_id)         wid: vec3<u32>) {
    let v = gid.x;
    var x = 0.0;
    if (v < p.n_nodes && deg[v] == 0.0) { x = r_in[v]; }  // mask: dangling rank only
    scratch[lid.x] = x;
    workgroupBarrier();
    var stride = WG / 2u;
    loop {
        if (stride == 0u) { break; }
        if (lid.x < stride) { scratch[lid.x] = scratch[lid.x] + scratch[lid.x + stride]; }
        workgroupBarrier();
        stride = stride / 2u;
    }
    if (lid.x == 0u) { partials[wid.x] = scratch[0]; }    // host or a 2nd pass sums partials
}

// ---- Finalize: damping + teleport + dangling --------------------------------
// dangling_total is summed host-side from `partials`, then written into params
// before this pass, keeping pr_finalize a pure map (matches geometric.rs:3124-3127).
@compute @workgroup_size(WG)
fn pr_finalize(@builtin(global_invocation_id) gid: vec3<u32>) {
    let v = gid.x;
    if (v >= p.n_nodes) { return; }
    r_out[v] = (1.0 - p.damping) * p.inv_n
             + p.damping * p.inv_n * dangling_total   // from params
             + p.damping * acc[v];
}
```

### Rust wiring sketch — `src/analytics/pagerank.rs`

```rust
pub fn gpu_pagerank(
    ctx: &EngineCtx,
    graph: &CsrGraph,
    damping: f32,
    iters: u32,
) -> Result<Vec<f32>, String> {
    let gpu = ctx.gpu.as_ref().ok_or("gpu_pagerank requires a wgpu device")?;
    let device = &gpu.device; let queue = &gpu.queue;
    let n = graph.n_nodes as usize;
    if n == 0 { return Ok(Vec::new()); }

    // Packed single-buffer CSR — copy of geometric_gpu.rs:187-192.
    let header = (n + 1) as u32;
    let mut csr: Vec<u32> = Vec::with_capacity((n + 1) + graph.neighbors.len());
    for v in 0..=n { csr.push(graph.offsets[v] + header); }
    csr.extend_from_slice(&graph.neighbors);

    let deg: Vec<f32> = (0..n)
        .map(|v| (graph.offsets[v + 1] - graph.offsets[v]) as f32)
        .collect();

    // r_a seeded to 1/n (oracle convention, geometric.rs:3103); r_b zeroed.
    let inv_n = 1.0 / n as f32;
    let mut r_init = vec![inv_n; n];

    // ... create csr_buf, deg_buf, r_a, r_b, acc_buf, partials_buf (len=n_workgroups),
    //     params_buf (UNIFORM, COPY_DST) — same helpers as sgd_stress_gpu storage_*.
    // ... one shader module from include_str!("../shaders/pagerank.wgsl"); three
    //     pipelines share one bgl. Two bind groups for r_a<->r_b ping-pong.

    let wgs = (graph.n_nodes + 63) / 64;
    let mut cur = 0u32;                       // 0 = r_a is r_in
    for _ in 0..iters.max(1) {
        // pass 1: dangling reduction -> partials; submit, read back, sum on host
        let dangling_total: f32 = read_partials_and_sum(...);
        queue.write_buffer(&params_buf, 0, bytemuck::bytes_of(&PrParams{ /* incl dangling_total */ }));
        // pass 2 + 3 in one encoder: pr_spmv (r_in->acc), pr_finalize (acc->r_out)
        // adjacent passes get an implicit storage barrier (sgd_stress_gpu.rs:362-366).
        cur ^= 1;                             // ping-pong
    }
    // copy live rank buffer -> readback -> map_async + poll(Wait) (sgd_stress_gpu.rs:402)
    Ok(ranks)                                 // Vec<f32>, len n, in CsrGraph node order
}
```

### Test sketch — `tests/gpu_pagerank.rs`

```rust
#[test]
fn gpu_pagerank_matches_csr_oracle() {
    let _g = gpu_guard();
    let Some(ctx) = gpu_or_skip("gpu_pagerank_matches_csr_oracle") else { return; };
    let g = dumbbell();                                   // same fixtures as geometric.rs tests
    let gpu = graph_compute::gpu_pagerank(&ctx, &g, 0.85, 50).unwrap();
    let cpu = cpu_csr_pagerank(&g, 0.85, 50);             // mirror of geometric.rs:3097
    for (a, b) in gpu.iter().zip(&cpu) { assert!((a-b).abs() < 1e-4, "{a} vs {b}"); }
    // ordering vs the PUBLIC VaultGraph oracle:
    let mut vg = csr_to_vaultgraph(&g);
    graph_metrics::compute_pagerank(&mut vg, 0.85, 50);
    assert_rank_order_agrees(&gpu, &vg);                  // top-k / Spearman
}

#[test]
fn gpu_pagerank_scales_to_millions() {
    let Some(ctx) = gpu_or_skip("scale") else { return; };
    let g = random_csr(2_000_000, 10_000_000);            // ~10M undirected edges
    let r = graph_compute::gpu_pagerank(&ctx, &g, 0.85, 30).unwrap();
    assert!(r.iter().all(|x| x.is_finite()));
    assert!((r.iter().sum::<f32>() - 1.0).abs() < 1e-2);  // mass conserved
}
```

### Precision (storage vs accumulate)

The kernel is generic over **storage** precision, never over accumulation. WGSL/WebGPU/Metal support **f32** and **f16** (`enable f16;` + the adapter's `shader-f16` feature) but have **no f64** — and Apple GPUs have no fp64 hardware regardless. So:

- **Storage precision** = `S: GpuFloat` ∈ `{f32, f16}` for the matrix values / edge weights. `gpu_pagerank::<S: GpuFloat>(ctx, &CsrGraph, damping, iters) -> Vec<f32>`. f32 is the default; f16 is opt-in, **gated on `wgpu::Features::SHADER_F16`** with automatic f32 fallback (the shader variant is chosen at pipeline creation).
- **Accumulation is always f32.** WGSL loads f16, widens to f32 for the `Σ r[u]/deg(u)` reduction and the damping/teleport finalize, then narrows on store. (`shader-f16` is a feature; two compiled variants or an override-constant `STORE_F16` select the path.)
- **Memory payoff at scale.** f16 halves the dominant **edge-value** term — a weighted CSR entry goes 8 → 6 B/edge (4 B `u32` index + **2 B** f16 value) and the value array halves; combined with ~2× f16 throughput on Apple GPUs this is the difference between fitting and partitioning near the §4 ceiling (matters for weighted/chemical SpMV; for *unweighted* PageRank the win is bandwidth, not capacity).
- **f16 rank-underflow rule (correctness, load-bearing).** At 8M nodes the ranks are ≈ `1/n ≈ 1.3e-7`, **below f16's smallest normal (~6.1e-5)** → they underflow. Therefore **the rank vector stays f32 even when the matrix is f16** (mixed mode), or ranks are rescaled (track `n·r` near 1.0). `gpu_pagerank` defaults to *f16-matrix / f32-rank* for large `n`; pure-f16 ranks are allowed only for small graphs and must pass the oracle tolerance.
- **`Vec<precision>` return.** f16/f32 are genuine GPU-compute precisions; a `Vec<f64>` return is a host-side widening *convenience only* (carries no double-precision compute — documented as such). True f64 would require a NVIDIA-only `cudarc` kernel and is explicitly out of scope for the hardware-agnostic path.

### Milestones (Part A)

- **P0 — primitive + kernel.** Add the `analytics` module (`mod.rs`, `pagerank.rs`, `spmv.rs`) + `pagerank.wgsl` with the three entry points; implement the `CsrCompute` buffer builder reusing `geometric_gpu.rs` single-buffer packing + host-precomputed `deg`; wire `pr_dangling_sum` + `pr_spmv` + `pr_finalize` with `r_a`/`r_b` ping-pong and per-iter params write; add `lib.rs` export + dev-deps; unit test (in `pagerank.rs #[cfg(test)]`) vs an inlined copy of `geometric::pagerank` on triangle/dumbbell.
- **P1 — validation.** `tests/gpu_pagerank.rs`: exact-within-1e-4 vs CSR oracle on small fixtures; ordering/top-k agreement vs `graph-metrics::compute_pagerank` over a converted `VaultGraph`; scale test (2M nodes / ~10M edges synthetic CSR — finite, mass≈1, completes; capture wall-time); optional tol-based early-exit + convergence test.
- **P2 — hub load-balancing + semiring reuse.** `pr_spmv_hybrid` (host buckets rows by degree threshold; light rows row-parallel, heavy rows warp-per-row with ≤1 KB threadgroup segment-reduction); benchmark row-parallel vs hybrid on a power-law graph; add `gpu_connected_components` (min-label SpMV) reusing `CsrCompute` to accelerate `graph-metrics/wcc.rs`; sketch `gpu_bfs` (boolean-frontier SpMV).

### Validation (Part A)

- **Primary numeric gate:** GPU ranks vs the CSR-native f32 oracle (`geometric.rs:3097-3131`) — same symmetrized `CsrGraph`, same teleport/dangling math, fixed `iters=50`, `damping=0.85` — assert per-node `|gpu - cpu| < 1e-4` on triangle/dumbbell/path fixtures already in `geometric.rs` tests.
- **Integration gate:** convert to `VaultGraph`, run public `graph-metrics::compute_pagerank(&mut vg, 0.85, 50)`; assert rank-ORDER agreement (top-k overlap / Spearman) since it is directed vs the symmetrized CSR, plus exact match on reciprocal graphs.
- **Scale gate:** synthetic ~2M-node / ~10M-edge random CSR on the default wgpu (Metal) backend — all ranks finite, total mass ≈ 1.0 (±1e-2), call returns; record wall-clock vs CPU oracle as speedup evidence.
- All GPU tests use the established `gpu_or_skip` + `GPU_TEST_LOCK` harness (`tests/gpu_engines.rs:38-52`) so they skip in the headless sandbox and run for real on a GPU host.
- **Tolerance rationale:** f32 accumulation order differs between serial CPU scatter and parallel GPU gather, so 1e-4 (not bit-exact) is correct — same philosophy as the barnes_hut 5e-2 and sgd 1.5× quality bounds already in the suite.

### Risks (Part A)

- **Directedness mismatch** — public oracle is directed, CSR is symmetrized; exact equality against the public oracle is impossible in general. Mitigation: primary gate is the symmetric CSR oracle; public oracle is ordering-only. Must be stated in the test or a reviewer will think it too weak.
- **No f32 atomics in core WGSL** — the dangling sum cannot use atomic-add; the threadgroup reduction + host partial-sum adds one tiny readback per iteration, which can dominate wall-time at high iteration counts (as per-sweep submits did for SGD). Mitigation: batch all three passes per iter into one encoder, read only the small partials buffer; or add a second GPU reduction pass for full on-device.
- **Hub load imbalance** on power-law vault graphs — v1 ships row-parallel (fine for typical vaults). Mitigation: P2 warp-per-row hybrid.
- **Storage-buffer budget** — device capped at 12 (`engines/mod.rs:141`); PageRank needs 6 (csr, deg, r_in, acc, r_out, partials), comfortably under, but the shared bgl must not balloon if semiring variants add buffers.
- **Dependency coupling** — `graph-metrics`/`vault-data` must stay **dev-only** or the lean native compute crate (and its wasm-exclusion intent) gets muddier.
- **Convergence vs fixed-iters** — matching the oracle requires fixed iters; a tol early-exit changes results and could fail an exact-match test if conflated. Keep fixed-iters as the parity default.

---

## 3. Part B — Scale-out: HaloTransport domain decomposition

**Area:** `graph-compute` HaloTransport / distributed BSP (Phase 6) — PageRank SpMV scale-out.

### What already exists (the premise is half-stale)

`partition.rs` is **not** just a single-process scaffold. Reading the real files:

- The `HaloTransport` trait (`partition.rs:793-803`) has TWO impls: the in-process `LocalTransport` double (`partition.rs:825-836`) AND a real `TonicHaloTransport` (`partition.rs:863-1000`) that bridges the sync BSP loop to an async tonic bidi stream via mpsc channels + a driver task, with out-of-order frame buffering (`pending`, `partition.rs:871`) and a blocking per-frame `collect` (`partition.rs:965-999`).
- `bsp.rs` already has `MeshTransport` (fan-out to P-1 peers, `bsp.rs:104-138`), `LiveHaloProvider` (per-frame live boundary snapshot, `bsp.rs:62-92`), `BspWorker` (`bsp.rs:144-171`), and `run_bsp_mesh` (`bsp.rs:197-225`) with correct two-phase ordering (all workers compute+snapshot, THEN all exchange+barrier).
- `bsp_loop_grpc.rs:217-285` already does EXACTLY the validation we want — partition a graph, run K BSP supersteps with real gRPC halo exchange, reassemble owned positions, assert equivalence to a single-worker reference within `TOL=1e-3` — but only for a `RelaxEngine` whose target is partition-independent. The `ExchangeHalo` RPC is wired in `service.rs:289-314` behind the `HaloProvider` seam.

So the BSP + halo + gRPC machinery is **done and green** over `Compute.ExchangeHalo(stream HaloDelta) returns (stream HaloDelta)` (`compute.proto:28`, `service.rs:289-314`). The edge-cut + FM partitioner (`partition.rs:187-639`) is built and tested. The remaining gaps for chemical-simulation-scale sparse graphs are **five concrete things**:

### Gap 1 (load-bearing): scalar / variable-arity halo payload

`HaloDelta` (`partition.rs:655-667`) and `HaloUpdate` (`mod.rs:333-344`) carry `positions: Vec<f32>` with the hard invariant `positions.len() == 3 * node_ids.len()` (enforced in `decode_bytes`, `partition.rs:745-751`; assumed by `boundary_delta`, `partition.rs:116-155`, which copies x,y,z triples). An iterative SpMV like PageRank exchanges **one scalar per boundary node** (the boundary vertex's current rank), not a 3-vector. The halo must carry a **variable-width per-node value vector** of width `d` (d=3 layout positions, d=1 PageRank rank, d=k block Krylov / multi-vector SpMV).

**Concrete change:** add `value_dim: u32` to `HaloDelta` + `HaloUpdate` + the proto `HaloDelta` message; relax the invariant to `positions.len() == value_dim * node_ids.len()`. Default `value_dim=3` (decoders treat 0 as legacy 3) keeps every existing test byte-compatible because existing call sites set 3. `boundary_delta` gains a `value_dim` arg (or a sibling `boundary_delta_dim`). **Additive and non-breaking.** Keep the field name `positions` for wire stability; document it as "per-node boundary values" (the proto comment `compute.proto:69` already calls it "parallel to node_ids").

### Gap 2: PageRank as a sharded LayoutEngine (SpMV under BSP)

The single-process `pagerank()` over a whole CSR (`geometric.rs:3097-3131`) is NOT a `LayoutEngine` and does not understand shards/ghosts. Build a `PageRankEngine: LayoutEngine` (`mod.rs:352-413`) whose `step` does ONE power-iteration over the **local shard CSR** (`Partition::local`, `partition.rs:397-419`) and whose `apply_halo` writes incoming ghost rank values into the ghost slots of its local rank vector.

The math that makes this exact under BSP (the "exchange only boundary values" pattern applied to a scalar field):

- Each worker holds `rank[0..n_local]`: owned ranks in `[0,n_owned)`, ghost ranks in `[n_owned,n_local)`.
- `step`: push-based SpMV over OWNED rows only. For each owned `v`, distribute `rank[v]/outdeg_global[v]` to each local neighbour `u` (owned or ghost). Owned out-degree must be the **GLOBAL** out-degree — the partitioner already includes cross-partition neighbours as ghosts, so `local.offsets[v+1]-local.offsets[v]` for an owned row == global degree (ghosts appended, owned rows keep all neighbours). Apply damping + teleport on owned nodes.
- `step` returns `StepOutput { positions: <owned rank scalars>, boundary: Some(HaloUpdate{ value_dim:1, node_ids: boundary, positions: boundary owned ranks }) }`. The boundary set (`partition.rs:88`) is exactly the owned nodes some peer ghosts, so we ship strictly the needed scalars.
- `apply_halo`: for each incoming `(gid, rank_scalar)`, map `gid -> local ghost index` and store. Next superstep's SpMV reads fresh ghost ranks.

**Correctness equivalence argument:** push-SpMV partitioned by rows is exact iff every owned node sees, each iteration, the current ranks of ALL its in-neighbours. Locally-owned in-neighbours are read directly; elsewhere-owned ones are ghosts refreshed by `apply_halo` from the owner's previous-superstep value. Because BSP barriers separate supersteps, ghost values at superstep t are the owners' superstep-(t-1) ranks — IDENTICAL to single-process power iteration where iteration t reads iteration-(t-1) ranks everywhere. After K supersteps the distributed rank vector equals single-process `pagerank(g, damping, K)` exactly (mod f32 summation order → compare within `TOL`, as `bsp_loop_grpc.rs:208` does).

**One subtlety — global dangling mass.** `geometric.rs:3124` redistributes dangling rank GLOBALLY each iteration, which boundary-only halos can't do. For graphs with NO dangling nodes (degree ≥ 1 everywhere — true for rings/paths/connected molecular graphs) it drops out; v1 `PageRankEngine` asserts no global dangling (or carries a tiny scalar all-reduce as a second halo channel — see Open Questions).

### Gap 3: spatial / domain-decomposition partitioner for structured sparsity

`partition_csr` (BFS edge-cut) + FM refinement (`partition.rs:546-639`) minimize cut count — ideal for **power-law / scale-free** graphs (Obsidian vaults, citation graphs). But chemical-simulation matrices have **structured, local** sparsity: a node couples only to spatial neighbours (a stencil / cutoff radius), so the optimal partition is a **spatial domain decomposition** — slabs/blocks of a 3D box — giving small, surface-area-proportional ghost regions (the classic MD/PDE halo exchange).

The geometric engine ALREADY has spatial cell-hashing: `cells: HashMap<(i32,i32,i32), Vec<u32>>` keyed by quantized position (`geometric.rs:1978-1988`). Build `partition_spatial(graph, positions, n_partitions)`: (a) bucket nodes into a coarse 3D grid via the same `(x/cell).floor() as i32` hashing; (b) assign contiguous grid-cell ranges (recursive-coordinate-bisection / Morton/Z-order over cell coords) to P partitions, balancing owned-node counts; (c) feed the resulting `owner[]` straight into the EXISTING `build_partitions_from_owner` (`partition.rs:221-239`) so ghost/boundary derivation and every downstream invariant are reused unchanged. Right partitioner when the caller has node positions (a chemical sim always does); `edge_cut` over the spatial partition is far lower than BFS for stencil matrices. Edge-cut stays the default for topology-only / power-law inputs.

### Gap 4: coordinator + multi-GPU-vs-multi-node

Today the "coordinator" is the test harness (`build_mesh`, `bsp_loop_grpc.rs:154-189`) running all workers in one process over duplex pipes. Two deployment shapes:

- **Multi-GPU, one host (the discrete-GPU box case):** one process, P `EngineCtx`s each bound to a different wgpu adapter (`mod.rs:89`, `try_new_gpu` currently grabs ONE HighPerformance adapter — extend to enumerate adapters and pin worker p → adapter p). Halo exchange is a cross-device buffer copy, not a network hop — `LocalTransport` (`partition.rs:809`) is already the right transport; no gRPC. **On unified-memory Apple Silicon "multi-GPU" is moot** (one GPU, one unified pool) — partitioning buys nothing until you exceed the single device's working set (Gap 5), at which point you go multi-NODE. Unified memory is the advantage here: there is no separate VRAM ceiling to hit before the host-RAM ceiling.
- **Multi-node (the >128 GB case):** P OS processes, each a `graph-compute` worker serving `ExchangeHalo` on a real TCP port, plus a `Coordinator` that (a) loads/partitions the global CSR, (b) ships each `Partition` to its worker (new `AssignPartition` RPC or CLI handing each worker a serialized `Partition` block, `compute-architecture.md:252-254`), (c) brokers the peer-address table so each worker can `TonicHaloTransport::connect` (`partition.rs:883`) over TCP instead of duplex pipes, (d) issues `start(n_supersteps)` and collects final `PositionDelta`s. Worker code is UNCHANGED from `bsp.rs` — only transport endpoints (TCP `Channel` vs in-memory duplex) and bootstrap differ. This is the genuine `TODO(two-process)` (`bsp.rs:35`), a thin shell over existing pieces, sandbox-blocked.

### Gap 5: when does partitioning kick in (memory model)

Per-node working set for geometric/FA2: position+velocity+force ≈ 3 vecs × 3 f32 = 36 B/node, plus CSR `neighbors` at 4 B/edge, plus per-engine scratch (octree, cell-hash) ≈ 2-3× the position buffers. PageRank SpMV is lighter: ~3 f32/node (rank, next, outdeg) + 4 B/edge. The dominant term at chemical scale is **EDGES**. See §4 for the full sizing reference.

### How it ties together

- Add `value_dim` to halo (Gap 1) → unlocks scalar SpMV.
- `PageRankEngine: LayoutEngine` (Gap 2) → SpMV under existing `run_bsp_mesh` with ZERO transport changes.
- `partition_spatial` (Gap 3) → reuses `build_partitions_from_owner`, right cut for structured matrices.
- `Coordinator` + TCP transport (Gap 4) → the real multi-node shell over existing `TonicHaloTransport`.
- Memory model (Gap 5) → a `should_partition(n_nodes, n_edges, device_mem)` helper + docs so the coordinator decides.

### Files to create or modify

| Path | Change |
|---|---|
| `crates/graph-compute/proto/compute.proto` | add `uint32 value_dim = 6;` to message `HaloDelta` (default 0 ⇒ treat as 3 for back-compat) |
| `crates/graph-compute/src/partition.rs` | add `value_dim: u32` to `HaloDelta`; relax `decode_bytes` invariant (~line 745) to `positions.len()==value_dim*node_ids.len()`; thread `value_dim` through `encode_proto`/`decode_bytes`/`boundary_delta`; add `boundary_delta_dim(frame, value_dim, owned_values)`; add `pub fn partition_spatial(graph, positions, n_partitions) -> Vec<Partition>` calling `build_partitions_from_owner`; add `should_partition(n_nodes, n_edges, device_bytes) -> bool` |
| `crates/graph-compute/src/engines/mod.rs` | add `value_dim: u32` to `HaloUpdate` (default 3); add `pub mod pagerank;` + `pub use pagerank::PageRankEngine;`; register in `LEAF_ENGINE_IDS` + `construct_leaf` + `leaf_ctor_for` |
| `crates/graph-compute/src/engines/pagerank.rs` | **NEW** — `PageRankEngine` implementing `LayoutEngine`; shard-aware push-SpMV in `step()`; `apply_halo()` writes ghost rank scalars; `value_dim=1` boundary delta |
| `crates/graph-compute/src/coordinator.rs` | **NEW** — `Coordinator` that partitions a global CSR, brokers peer addresses, drives start/collect for the multi-node path; + a `CsrShard` distribution helper |
| `crates/graph-compute/src/lib.rs` | add `pub mod coordinator;` |
| `crates/graph-compute/tests/bsp_pagerank_grpc.rs` | **NEW** — partition a graph, run K BSP supersteps of `PageRankEngine` over the real gRPC mesh, reassemble owned ranks, assert equivalence to single-process `pagerank()` within TOL (mirrors `bsp_loop_grpc.rs`) |
| `crates/graph-compute/tests/partition_spatial.rs` | **NEW** — assert spatial partition of a grid/lattice has lower `edge_cut` than BFS and preserves all `Partition` invariants |
| `docs/compute-architecture.md` | extend §4: scalar/variable-arity halo, PageRank SpMV-under-BSP correctness, spatial decomposition, multi-GPU-vs-multi-node, the `should_partition` memory threshold |

### Rust sketch — `src/engines/pagerank.rs`

```rust
// Shard-aware PageRank: ONE power-iteration per BSP superstep over the LOCAL
// shard CSR, exchanging boundary rank SCALARS (value_dim = 1) each step.
use crate::engines::{CsrShard, EngineCtx, HaloUpdate, LayoutEngine, ShardMeta, StepOutput};
use graph_layouts::LayoutDescriptor;
use std::collections::HashMap;

pub struct PageRankEngine {
    descriptor: LayoutDescriptor,
    damping: f32,
    rank: Vec<f32>,              // [0,n_owned)=owned, [n_owned,n_local)=ghost
    next: Vec<f32>,
    n_owned: usize,
    offsets: Vec<u32>,           // local CSR (owned rows carry remapped owned+ghost adjacency)
    neighbors: Vec<u32>,
    out_deg: Vec<f32>,           // GLOBAL out-degree of each owned row
    global_ids: Vec<u32>,        // local_idx -> global id
    ghost_local_of: HashMap<u32, usize>, // global id -> local ghost slot
    boundary: Vec<u32>,          // owned-that-are-peer-ghosts (ascending)
    owner_id: u32,
}

impl LayoutEngine for PageRankEngine {
    fn descriptor(&self) -> &LayoutDescriptor { &self.descriptor }

    fn init(&mut self, _ctx: &mut EngineCtx, g: &CsrShard, _p: &[f32]) -> Result<(), String> {
        let n_local = g.graph.n_nodes as usize;
        let meta: &ShardMeta = g.shard.as_ref().ok_or("PageRankEngine requires a shard")?;
        self.n_owned = meta.owned_node_ids.len();
        self.owner_id = meta.partition_id;
        self.offsets = g.graph.offsets.clone();
        self.neighbors = g.graph.neighbors.clone();
        // owned rows hold ALL global neighbors (cross-partition ones are ghosts),
        // so local degree == global degree for owned rows.
        self.out_deg = (0..self.n_owned)
            .map(|v| (self.offsets[v + 1] - self.offsets[v]) as f32)
            .collect();
        self.rank = vec![1.0 / self.n_owned.max(1) as f32; n_local];
        self.next = vec![0.0; n_local];
        // global_ids / ghost_local_of / boundary supplied via the Partition.
        Ok(())
    }

    fn step(&mut self, _ctx: &mut EngineCtx) -> StepOutput {
        let inv_n = 1.0 / self.n_owned.max(1) as f32;
        let teleport = (1.0 - self.damping) * inv_n;
        for x in self.next.iter_mut() { *x = 0.0; }
        // push-SpMV over OWNED rows; lands contributions in owned AND ghost slots
        for v in 0..self.n_owned {
            if self.out_deg[v] == 0.0 { continue; } // (no global dangling in v1)
            let share = self.rank[v] / self.out_deg[v];
            let (s, e) = (self.offsets[v] as usize, self.offsets[v + 1] as usize);
            for &u in &self.neighbors[s..e] { self.next[u as usize] += share; }
        }
        // finalize OWNED ranks only (ghosts are overwritten by apply_halo)
        for v in 0..self.n_owned {
            self.next[v] = teleport + self.damping * self.next[v];
        }
        std::mem::swap(&mut self.rank, &mut self.next);
        // boundary halo: ONE scalar per boundary owned node (value_dim = 1)
        let mut node_ids = Vec::with_capacity(self.boundary.len());
        let mut vals = Vec::with_capacity(self.boundary.len());
        for &g in &self.boundary {
            let li = self.global_ids[..self.n_owned].binary_search(&g).unwrap();
            node_ids.push(g);
            vals.push(self.rank[li]);
        }
        StepOutput {
            positions: self.rank[..self.n_owned].to_vec(),
            boundary: Some(HaloUpdate { frame: 0, owner_id: self.owner_id,
                                        node_ids, positions: vals, value_dim: 1,
                                        attributes: None }),
        }
    }

    fn apply_halo(&mut self, h: &HaloUpdate) {
        debug_assert_eq!(h.value_dim, 1);
        for (i, &gid) in h.node_ids.iter().enumerate() {
            if let Some(&li) = self.ghost_local_of.get(&gid) {
                self.rank[li] = h.positions[i]; // refresh ghost rank for next SpMV
            }
        }
    }
}
```

```rust
// crates/graph-compute/src/partition.rs  (additive)
/// Spatial domain decomposition for structured/local sparsity (chemical sims).
/// Reuses the geometric engine's cell-hash quantization + Z-order over cells.
pub fn partition_spatial(graph: &CsrGraph, positions: &[f32], n_partitions: u32)
    -> Vec<Partition>
{
    let n = graph.n_nodes as usize;
    let p = n_partitions.max(1);
    let cell = estimate_cell_size(positions);          // reuse geometric.rs:1988 estimator
    let mut keyed: Vec<(u64, u32)> = (0..n).map(|v| {
        let (x, y, z) = (positions[3*v], positions[3*v+1], positions[3*v+2]);
        let c = ((x/cell).floor() as i32, (y/cell).floor() as i32, (z/cell).floor() as i32);
        (morton3(c), v as u32)                         // Z-order keeps blocks contiguous
    }).collect();
    keyed.sort_unstable();
    let mut owner = vec![0u32; n];
    let per = n.div_ceil(p as usize).max(1);
    for (rank, &(_, v)) in keyed.iter().enumerate() {
        owner[v as usize] = ((rank / per) as u32).min(p - 1);
    }
    build_partitions_from_owner(graph, None, &owner, p)  // existing invariants reused
}

/// Memory model (doc §4): partition once the working set won't fit one device.
pub fn should_partition(n_nodes: u64, n_edges_directed: u64, device_bytes: u64) -> bool {
    const PER_NODE: u64 = 36;     // pos+vel+force f32x3
    const PER_EDGE: u64 = 4;      // CSR neighbor u32
    const SCRATCH_MULT: u64 = 3;  // octree / cell-hash / next-buffers
    let ws = (n_nodes * PER_NODE + n_edges_directed * PER_EDGE) * SCRATCH_MULT;
    ws > (device_bytes * 78) / 100   // ~78% usable headroom
}
```

```rust
// BSP superstep with halo exchange — ALREADY EXISTS verbatim (bsp.rs:197).
// PageRankEngine plugs straight in; only the engine + value_dim differ:
for frame in 0..n_supersteps {
    for w in workers.iter_mut() { w.compute_phase(frame); }   // a/b: SpMV + snapshot
    for (w, (ranks, delta)) in workers.iter_mut().zip(snaps) {
        w.transport.publish(frame, delta);                    // c: ship boundary scalars
        for peer in w.transport.collect(frame) {              // d: barrier
            w.worker.engine.apply_halo(&peer.into_halo());    //    refresh ghost ranks
        }
        last[w.worker.partition.partition_id as usize] = ranks;
    }
}
```

### Milestones (Part B)

- **M1 — scalar halo payload.** Add `value_dim: u32` to `HaloDelta` (partition.rs) + `HaloUpdate` (mod.rs) + proto `HaloDelta`; relax `decode_bytes` invariant to `value_dim*node_ids.len()` (default 0 decodes as 3); thread `value_dim` through `encode_proto`/`decode_bytes`/`from_halo`/`into_halo`/`boundary_delta`; add `boundary_delta_dim`; confirm existing tests (`halo_delta_bytes_roundtrip`, `exchange_halo_grpc`, `bsp_loop_grpc`) stay green unchanged.
- **M2 — PageRank SpMV engine.** Create `engines/pagerank.rs` (`init` from `CsrShard`+`ShardMeta`, push-SpMV `step`, ghost-refresh `apply_halo`); carry `global_ids`/`ghost_local_of`/`boundary` into the engine; register in `LEAF_ENGINE_IDS` / `construct_leaf` / `leaf_ctor_for`; unit test single-shard `PageRankEngine` matches `geometric::pagerank()` on triangle/ring.
- **M3 — distributed equivalence test.** Create `tests/bsp_pagerank_grpc.rs` mirroring `bsp_loop_grpc.rs` (`build_mesh`, `run_bsp_mesh`, reassemble owned ranks); assert per-node distributed rank ≈ `pagerank(g, damping, K)` within `TOL=1e-3` for `path(12)`/`ring(12)` at P=2,3; assert every ghost received a rank halo each superstep (non-vacuous).
- **M4 — spatial partitioner.** Add `partition_spatial` (cell-hash + Morton/Z-order → `build_partitions_from_owner`); `tests/partition_spatial.rs` — on a 3D lattice, `edge_cut(spatial) < edge_cut(BFS)` and all `Partition` invariants hold; document spatial (positions available, local sparsity) vs edge-cut (power-law/topology-only).
- **M5 — coordinator + memory model.** Add `should_partition()` + doc §4 napkin-math table; add `coordinator.rs` (partition global CSR, broker peer addresses, distribute `Partition` blocks, drive start/collect); document multi-GPU-one-host (LocalTransport, adapter-per-worker) vs multi-node (TonicHaloTransport over TCP); leave the OS-process two-node integration as a documented sandbox-blocked TODO (mirrors `bsp.rs:35`).

### When Part B is needed

Below the single-device working-set ceiling (§4), single-device wins — no halo traffic, no barrier. **Above the ~100 GB / single-128 GB-Mac line you MUST partition.** The trigger is the **>~10B-edge / ~100 GB napkin-math threshold** (topology-bound), with a practical engine-bound ceiling ~3-4× lower. This is the chemical-simulation upper bound: structured stencil matrices whose edge count dwarfs a single unified-memory pool. Halo traffic per superstep is `Σ boundary_count × value_dim × 4 B`, which for a spatial decomposition scales as the partition **surface area** (∝ n^{2/3} per block) — negligible vs the n-proportional working set, which is exactly why BSP halo exchange scales.

### Validation (Part B)

Correctness equivalence is the bar, exactly as `bsp_loop_grpc.rs:217-285` establishes for positions. The new `tests/bsp_pagerank_grpc.rs` (M3) partitions a graph into P shards, runs K BSP supersteps of `PageRankEngine` over the REAL `Compute.ExchangeHalo` gRPC mesh (duplex pipes, no TCP — sandbox-safe, reusing `build_mesh` at `bsp_loop_grpc.rs:154-189`), reassembles per-shard owned rank vectors into a global gid→rank map, and asserts it matches the single-process reference `geometric::pagerank(g, damping, K)` within `TOL=1e-3` (same f32 tolerance as the position test at line 208, justified because partitioning only reorders f32 summation). The equivalence is **provable, not just empirical**: BSP barriers guarantee ghost ranks at superstep t are owners' superstep-(t-1) ranks, identical to single-process power iteration, so the only divergence is summation order. A second assertion (mirroring `bsp_loop_grpc.rs:263-284`) confirms every ghost received a rank halo each superstep so the test is non-vacuous. Tested at P=2 (single cut) and P=3 (multi-cut ring). M4 validates the spatial partitioner by `edge_cut` comparison + the full `Partition` invariant suite already codified in `partition.rs`. M1 is validated by every pre-existing halo/BSP test staying green (back-compat proof). Scale is validated by the `should_partition()` unit test pinning the ~3-4B-edge engine-bound threshold on a 128 GB device.

### Risks (Part B)

- **Global dangling-mass reduction** — `geometric::pagerank` redistributes dangling rank GLOBALLY each iteration; a partitioned SpMV can't with boundary-only halos. v1 must assert no global dangling (true for connected molecular/ring/path graphs) OR add a tiny scalar all-reduce as a second halo channel — otherwise distributed != single-process for graphs with degree-0 nodes.
- **Out-degree semantics** — SpMV correctness hinges on owned-row local degree == GLOBAL degree. Holds because the partitioner appends cross-partition neighbours as ghosts and emits FULL owned-row adjacency (`partition.rs:397-419`); but ghost ROWS are emitted EMPTY (`partition.rs:402-403`, `TODO(ghost-adjacency)`). PageRank only pushes from owned rows so it is safe, but must be asserted.
- **`value_dim=0` back-compat default** — decoders must treat 0 as 3; easy to get wrong and silently corrupt either path. Needs explicit d=1 and d=3 round-trip tests.
- **Spatial partitioner needs positions** — topology-only callers (vault import before any layout) must fall back to edge-cut; the coordinator must choose by input availability, not hardcode.
- **Two-process / real-TCP path stays UNVALIDATED under the sandbox** (`bsp.rs:35`). The duplex-mesh test proves the codec+loop but not OS-process bootstrap, peer-address brokering, or TCP failure/timeout (`partition.rs:860 TODO(barrier-policy)`). Genuine multi-node remains a documented gap.
- **Memory napkin constants are rough** — real engine footprints (octree, cell-hash, GPU buffer alignment) vary, so `should_partition` is a heuristic gate, not a guarantee — under-estimating triggers OOM, over-estimating partitions needlessly.

---

## 4. Memory sizing reference

**Nodes are free; the cost is edges.** Sizing the working set:

- **In-CSR int32 ≈ 4 B/edge** for raw topology (`neighbors: u32`). With weights + side structures (deg, rank, next, octree/cell-hash scratch), budget **~8-10 B/edge**.
- Per-node terms are small: PageRank SpMV ≈ 3 f32/node (rank, next, outdeg); geometric/FA2 ≈ 36 B/node (pos+vel+force f32×3). At chemical scale the **EDGE term dominates**.

**The single-128 GB-Mac line** (reserving ~100 GB usable):

| Bound | Formula | Threshold |
|---|---|---|
| Topology-only CSR | `4 B × n_directed_entries` | 100 GB / 4 B = 25 G directed entries ≈ **~12.5 B undirected edges** |
| Engine-bound (3× scratch) | `(n_nodes·36 + n_edges·4)·3` | partition around **~3-4 B undirected edges / ~30-40 B directed entries** |
| Degree-1000 working point | ~10 M nodes × ~degree-1000 | **~10 B edges ≈ the ~100 GB / single-128 GB-Mac line** |

So the quoted ">~10B-edge / ~100 GB on a 128 GB box" threshold is the **topology-bound ceiling**; the **practical engine-bound ceiling is ~3-4× lower**. Below it → single device (no halo traffic, no barrier). Above it → **partition (Part B)**.

The `should_partition(n_nodes, n_edges_directed, device_bytes)` helper encodes this: `(n_nodes·36 + n_edges·4)·3 > 0.78·device_bytes`.

**Unified memory is an advantage over single-GPU VRAM.** On discrete GPUs the working set is capped by a separate VRAM ceiling (often 24-80 GB) *below* host RAM. On Apple Silicon there is one unified pool — the CSR sits in the same memory the GPU reads, so the only ceiling is the host-RAM line above, and there is no host↔device copy tax on the CSR. This is why a single 128 GB Mac reaches the ~10 B-edge working point before any partitioning is required, and why "multi-GPU on one Apple host" is moot (Part B, Gap 4) — you scale by adding NODES, not local adapters.

---

## 5. lavender integration

**How lavender's graph feeds jump-cannon.** lavender (ingest) emits the vault access/knowledge graph as `access_graph_edges` / `authz_tuples`. These are ingested by `graph-api` and built into the symmetrized single-buffer CSR (`graph-api/src/server.rs:758-759` pushes both directions; `build_csr_bin`) that *both* tracks consume:

- **Part A** consumes that `CsrGraph` directly via `gpu_pagerank(ctx, &CsrGraph, 0.85, 50)`.
- **Part B** partitions the same CSR (`partition_csr` for the power-law vault graph, `partition_spatial` only where node positions exist) and shards it across workers.

**Notebook integration — replacing the cuGraph diagnostic.** lavender's notebooks currently run a **cuGraph PageRank diagnostic** (CUDA-only, NVIDIA-bound) over the vault graph. That call site is replaced by the new **hardware-agnostic GPU PageRank**:

- The notebook calls into `graph-compute` (in-process via `graph-api` linking it, or over a one-shot unary RPC — see open questions) and gets back a per-node `Vec<f32>` in CSR node order.
- Because the kernel is wgpu/WGSL, the same notebook diagnostic now runs on Apple Metal (the FHK / GN9 / LDP fleet) and on Vulkan/DX12 hosts — no CUDA dependency, no cuGraph install. This is the concrete payoff of the §1 decision.
- The notebook maps the returned `Vec<f32>` onto vault node ids (the CSR is in stable node order) to reproduce the cuGraph diagnostic's per-document PageRank table.

**CPU oracle wiring.** The validation and fallback path uses the existing CPU oracles:

- `graph-metrics::compute_pagerank` (the public `VaultGraph` oracle) and the CSR-native `geometric.rs:3097` oracle remain the correctness references — the notebook can diff GPU vs CPU on small vaults to confirm parity before trusting GPU at scale.
- For headless / no-GPU CI (the sandbox), the CPU oracle is the drop-in: tests `gpu_or_skip` and fall back to `graph-metrics` so the diagnostic still produces a table.

**API surface decision (recommended).** Keep `gpu_pagerank` a **pure `Vec<f32>`-returning primitive** — do NOT couple `graph-compute` to `vault-data` by writing back into `VaultGraph.node.metrics.pagerank`. A thin adapter in the calling crate (graph-api / the notebook bridge) maps the vector onto vault nodes. If remote access is needed, add a minimal one-shot unary `ComputePageRank` RPC returning per-node scalars alongside the existing layout `Subscribe` stream (the proto has no scalar-per-node response message today).

---

## 6. Phased roadmap (merged) and top risks / open questions

### Merged roadmap

| Wave | Phase | Track | Deliverable |
|---|---|---|---|
| 1 | **P0** | A | Row-parallel pull SpMV + GPU PageRank, fixed-iters, matching the CSR oracle on tiny graphs |
| 1 | **P1** | A | Cross-oracle correctness (1e-4 vs CSR oracle; ordering vs public oracle) + millions-scale Metal stability |
| 2 | **lavender cutover** | A | Notebook diagnostic calls hardware-agnostic GPU PageRank instead of cuGraph; CPU-oracle fallback wired |
| 2 | **P2** | A | `pr_spmv_hybrid` hub load-balancing; `gpu_connected_components` (min-label SpMV); `gpu_bfs` sketch |
| 3 | **M1** | B | Scalar / variable-arity halo (`value_dim`), back-compatible with d=3 layout |
| 3 | **M2** | B | Shard-aware `PageRankEngine` (one power-iteration/superstep, ghost-refresh `apply_halo`) |
| 3 | **M3** | B | Distributed == single-process equivalence over the real gRPC mesh (`TOL=1e-3`, P=2,3) |
| 4 | **M4** | B | `partition_spatial` (cell-hash + Morton/Z-order) for structured/chemical sparsity |
| 4 | **M5** | B | `should_partition` memory model + `Coordinator` (multi-node shell; OS-process path sandbox-blocked TODO) |

Part A (Waves 1-2) ships the immediate lavender win — hardware-agnostic GPU PageRank replacing cuGraph — on a single device. Part B (Waves 3-4) is gated on the §4 memory threshold and is only built when a graph exceeds the single-128 GB-Mac working point.

### Top risks

1. **Directedness mismatch (A)** — public oracle directed, CSR symmetrized; the cross-oracle test must use the symmetric CSR oracle as the numeric gate and the public oracle as ordering-only, or it reads as too weak.
2. **No f32 atomics in core WGSL (A)** — dangling-sum reduction adds a per-iteration readback that can dominate wall-time; mitigate by batching passes per encoder / a second on-device reduction pass.
3. **Hub load imbalance (A)** — power-law vault hubs stall row-parallel warps; mitigate with the P2 warp-per-row hybrid.
4. **Global dangling-mass reduction (B)** — boundary-only halos can't redistribute global dangling mass; v1 asserts no-dangling or adds a scalar all-reduce halo channel.
5. **Out-degree == global-degree invariant (B)** — correctness hinges on owned rows carrying full adjacency; ghost rows are intentionally empty (`partition.rs:402` TODO), safe for push-from-owned PageRank but must be asserted.
6. **Two-process / real-TCP path unvalidated (B)** — the duplex-mesh test proves codec + BSP loop but not OS-process bootstrap, peer brokering, or TCP failure/timeout; genuine multi-node stays a documented sandbox-blocked gap.
7. **Memory heuristic is approximate (B)** — `should_partition` constants are rough; under-estimate → OOM, over-estimate → needless partitioning.

### Open questions

- **API shape:** does `gpu_pagerank` write into `VaultGraph.node.metrics.pagerank` as a drop-in for `graph-metrics::compute_pagerank`, or stay a pure `Vec<f32>` primitive with a thin adapter in the caller? **Recommended: pure primitive** (keeps `graph-compute` decoupled from `vault-data`).
- **Call site:** does lavender want a new unary `ComputePageRank` RPC alongside the layout `Subscribe` stream, or an in-process call (graph-api links graph-compute)? The proto has no scalar-per-node response today; a one-shot unary RPC is the minimal addition if remote access is needed.
- **Directedness in production:** is the production vault CSR always symmetrized, or are there directed call sites where the GPU pull form would silently diverge from intended directed PageRank? Directed PageRank would need a separate CSC (in-neighbour) buffer, doubling upload.
- **Convergence policy:** match the oracle's fixed 50 iters, or expose `tol` early-exit? Affects whether the cross-oracle test can assert exact-within-tol. (Default: fixed-iters for the parity path.)
- **Scale-test ceiling** on 24-36 GB Metal devices vs the 128 GB FHK: bounded by `wgpu max_buffer_size` more than compute — what real upper bound do we advertise?
- **Dangling mass (B):** assert-no-dangling (simplest, fine for connected chemical graphs) vs a scalar all-reduce halo channel (a 1-element `HaloDelta` broadcasting Σdangling each superstep)? The latter generalizes to any global-reduction SpMV but adds a barrier sub-phase.
- **`value_dim` placement (B):** per-message (`HaloDelta`/`HaloUpdate`, flexible — mixed layout+rank) or per-run (engine-uniform, simpler — one field at a time)?
- **Engine shard metadata (B):** where does the engine get `global_ids`/`ghost_local_of`/`boundary` — enrich `ShardMeta` (cleanest, keeps `LayoutEngine` Partition-agnostic) or hand it the whole `Partition`? `ShardMeta` currently lacks the global-id ordering + ghost map (`mod.rs:295-303`).
- **Spatial cell size (B):** derive from mean nearest-neighbour distance (needs a kNN pass) or a target nodes-per-cell heuristic? Reuse the geometric engine's existing estimator (`geometric.rs:1988`) for consistency.
- **Coordinator bootstrap (B):** `AssignPartition` RPC (self-contained, ships Partition blocks + peer table over gRPC) vs file-based startup (each worker loads its `csr.bin` block, reusing the existing `/graph/csr.bin` exporter)?
- **Multi-GPU enumeration (B):** does wgpu reliably enumerate + independently bind multiple adapters across Metal/Vulkan/DX12, and is cross-adapter buffer copy cheaper than going through host memory? Needs a probe on real multi-GPU NVIDIA + Apple hardware (Apple unified memory makes intra-host multi-GPU largely moot).