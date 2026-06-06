//! Shared test helpers for the GPU analytics tests.

use graph_compute::EngineCtx;

/// Build a wgpu `EngineCtx`, or signal "skip" by returning `None`.
///
/// On a dev box without a GPU this returns `None` so the test passes (skips).
/// **But** when `GPU_PAGERANK_REQUIRE_ADAPTER` is set — which the Hydra/CI
/// correctness check does, having wired Metal (darwin builder) or lavapipe
/// software-Vulkan (Linux sandbox) — a missing adapter is a HARD failure. That
/// prevents a misconfigured software adapter from making the correctness suite
/// silently skip and masquerade as a pass (the exact failure mode flagged in
/// the testing research).
pub fn gpu_ctx_or_skip(label: &str) -> Option<EngineCtx> {
    let ctx = EngineCtx::try_new_gpu();
    if ctx.gpu.is_some() {
        return Some(ctx);
    }
    if std::env::var("GPU_PAGERANK_REQUIRE_ADAPTER").is_ok() {
        panic!(
            "{label}: GPU_PAGERANK_REQUIRE_ADAPTER is set but no wgpu adapter was \
             found — Metal/lavapipe is not wired correctly in this CI environment"
        );
    }
    eprintln!("Skipping {label} (no GPU adapter)");
    None
}
