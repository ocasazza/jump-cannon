//! Env-gated continuous CPU profiling for jump-cannon test workloads.
//!
//! Starts a [pyroscope] agent that streams 100 Hz CPU samples to the
//! configured Pyroscope server for the lifetime of the process. All
//! configuration is environment-driven so workload binaries and test
//! harnesses need no per-run knowledge:
//!
//! * `PYROSCOPE_URL` — server base URL (`scheme://host:port`), the same
//!   variable the jump-cannon-perf wrapper uses for criterion pprof uploads.
//!   Unset or empty disables profiling entirely (local `just bench` etc. are
//!   unaffected).
//! * `JUMP_CANNON_TEST_NAME` — the dashboard test row this process belongs
//!   to (e.g. `performance-bench-static-layouts`). Becomes the `test`
//!   profile label.
//! * `JUMP_CANNON_RUN_ID` — unique run identifier; defaults to `$HOSTNAME`
//!   (the pod name on cluster CronJob runs). Becomes the `run` label.
//!
//! The agent uploads every 10 s, so a profile is recorded even if the
//! process is killed mid-run; an unreachable server degrades to log noise,
//! never a test failure.
//!
//! Benchmarks bind the returned agent for the rest of `main`. Fuzz harnesses
//! use [`ctor`] + [`std::mem::forget`] because libtest owns `main`:
//!
//! ```no_run
//! #[test_profiling::ctor]
//! fn _start_profiling() {
//!     if let Some(agent) = test_profiling::start_from_env() {
//!         std::mem::forget(agent);
//!     }
//! }
//! ```

#![deny(missing_docs)]

#[cfg(not(target_arch = "wasm32"))]
pub use ctor::ctor;

#[cfg(not(target_arch = "wasm32"))]
use pyroscope::backend::{pprof_backend, BackendConfig, PprofConfig};
#[cfg(not(target_arch = "wasm32"))]
use pyroscope::pyroscope::{PyroscopeAgent, PyroscopeAgentBuilder, PyroscopeAgentRunning};

/// A running profiler agent. Keep the value alive for the profiling window
/// (dropping it stops the sampler and its upload thread).
#[cfg(not(target_arch = "wasm32"))]
pub type Agent = PyroscopeAgent<PyroscopeAgentRunning>;

/// Profiling is unavailable on wasm; the stub keeps call sites uniform.
#[cfg(target_arch = "wasm32")]
pub type Agent = ();

/// Start the env-gated profiler, or return `None` when profiling is not
/// requested (or the agent failed to initialize — never fatal).
#[cfg(not(target_arch = "wasm32"))]
pub fn start_from_env() -> Option<Agent> {
    let url = std::env::var("PYROSCOPE_URL").ok()?;
    if url.is_empty() {
        return None;
    }

    let test_name = std::env::var("JUMP_CANNON_TEST_NAME").unwrap_or_default();
    let run_id = std::env::var("JUMP_CANNON_RUN_ID")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| std::env::var("HOSTNAME").unwrap_or_else(|_| "local".into()));

    let mut tags: Vec<(&str, &str)> = vec![("run", run_id.as_str())];
    if !test_name.is_empty() {
        tags.insert(0, ("test", test_name.as_str()));
    }

    let agent = PyroscopeAgentBuilder::new(
        url,
        "jump-cannon",
        100,
        "rustspy",
        env!("CARGO_PKG_VERSION"),
        pprof_backend(PprofConfig::default(), BackendConfig::default()),
    )
    .tags(tags)
    .build()
    .ok()?;

    agent.start().ok()
}

/// Stub for wasm builds (profiling is a native-only concern).
#[cfg(target_arch = "wasm32")]
pub fn start_from_env() -> Option<Agent> {
    None
}
