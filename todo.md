# jump-cannon — upstream integration TODO

Changes landed here as **general graph-library improvements** (often built in
service of the [lavender](https://github.com/schrodinger/lavender) ingest work,
which we also own). Each may need deeper integration into jump-cannon's own
codebase — the renderer, the `Compute` gRPC service, or the existing CPU
`graph-metrics` — rather than living as a standalone module. Tracked here so the
integration isn't forgotten.

## GPU graph analytics (`crates/graph-compute/src/analytics/`)

Added: `gpu_pagerank` (wgpu/WGSL pull-SpMV PageRank, Metal/Vulkan/DX12),
`cpu_pagerank` (CPU reference + fallback), and the `graph-pagerank` bin
(one-shot CSR→ranks, the lavender notebook entrypoint). Validated vs the
`graph-metrics` CPU oracle and at 2M-node scale on Metal.

Integration still owed in jump-cannon proper:

- [ ] **Expose via the `Compute` gRPC service.** Today analytics are free
      functions + a CLI; the service only streams layout `PositionDelta` frames.
      Add a unary `ComputePageRank` (and future `ComputeComponents`) RPC so the
      renderer/broker can request centrality without shelling out to the bin.
- [ ] **Surface ranks in the renderer.** Node sizing / colouring by PageRank
      (and connected-component id) is the natural consumer — wire ranks into
      `graph-renderer` node attributes.
- [ ] **Reconcile the two PageRank oracles.** `graph-metrics::compute_pagerank`
      (directed, f64, mutates `VaultGraph`) and `analytics::cpu_pagerank`
      (undirected/symmetric, f32, over `CsrGraph`) now coexist. Decide the
      canonical one or document why both exist; the cross-oracle test in
      `graph-compute/tests/gpu_pagerank_cross_oracle.rs` pins their agreement on
      symmetrized graphs.
- [ ] **Directed + dangling on GPU.** `gpu_pagerank` rejects dangling (degree-0)
      nodes and is undirected/symmetric. Directed PageRank (in-edge CSR +
      out-degree `inv_deg`) and global dangling-mass redistribution are the
      follow-on needed before it can fully replace cuGraph's *directed* path in
      lavender's `backend.py`.

## Testing / CI (found while wiring the Hydra regression suite)

- [ ] **nextest profile `filter` key is silently ignored.** `.config/nextest.toml`
      uses `filter = "..."` on the `unit`/`integration`/`e2e` profiles, but
      nextest does not recognize that key (it warns: "ignoring unknown
      configuration keys: profile.unit.filter ..."). So those profiles do **not**
      scope their test sets — they only differ in fail-fast/test-threads, and the
      `tests-unit`/`tests-integration`/`tests-e2e` crane checks all run the same
      full suite (GPU/cluster tests just skip in the sandbox). The correct key is
      `default-filter` (used by the new `gpu` profile). Migrate the others, or
      collapse the redundant checks.
- [ ] **Confirm Metal works inside the aarch64-darwin Nix build sandbox.** The
      `tests-gpu` check + any bench-as-derivation rely on a wgpu adapter being
      reachable from within a `nix build` on the darwin Hydra builders. If the
      sandbox blocks Metal/IOKit, those jobs would need `__noChroot`/an impure
      runner (or run via the nix-darwin GitHub Actions runner instead). The Linux
      lavapipe path is sandbox-safe and is the gating correctness path regardless.
