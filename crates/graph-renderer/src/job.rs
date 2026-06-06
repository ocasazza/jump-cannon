//! Reusable background-job abstraction for long-running UI commands.
//!
//! The motivating case is the Generate (tvix) panel: `tvix_wasm::eval_graph`
//! is a single synchronous call that can take a noticeable wall-clock chunk
//! for a large graph. Running it straight from the button click-handler froze
//! the browser tab (no paint, no progress) until it finished. This module is
//! the poll-based escape hatch — the same shape the existing fetch slots use
//! (`App::node_fetch` etc.): kick off the work, the UI polls a shared result
//! slot each frame, shows a spinner/progress bar + logs while it's pending,
//! and consumes the result when ready.
//!
//! ## Threading model (honest, per target)
//!
//! * **Native**: the work runs on a real [`std::thread`]. Genuinely
//!   off-thread — the egui thread stays fully responsive while it runs.
//!
//! * **WASM**: eframe/egui in the browser is single-threaded. True parallelism
//!   would need a Web Worker hosting a second wasm instance (SharedArrayBuffer
//!   + COOP/COEP headers + an atomics build, or message-passing to a worker
//!   loader). NONE of that infrastructure exists in this repo today (no
//!   COOP/COEP, no `+atomics`, no `wasm-bindgen-rayon`), and a worker loader
//!   would need a second JS file beyond the ≤50-line trunk shim — an explicit
//!   blocker under the Rust-only rule. So WASM uses a **paint-first-then-run**
//!   strategy: the job sits `Queued` for one frame so egui can paint a
//!   "working…" frame (spinner + progress bar + log line), then the work runs
//!   synchronously inside the next [`BackgroundJob::poll`]. This does NOT make
//!   a single big synchronous call (like `eval_graph`) non-blocking — that one
//!   call still blocks the frame it runs on — but the UI is responsive and
//!   shows honest coarse progress before and after, instead of a dead tab with
//!   no feedback. Genuine non-blocking on WASM needs the worker/atomics
//!   follow-up (see the module docs above).
//!
//! ## API
//!
//! ```ignore
//! // Kick off (returns immediately on both targets):
//! let job = BackgroundJob::spawn(
//!     progress.sink(),      // where progress/log events go
//!     "generate",           // task group (shows in footer + debug console)
//!     "evaluate expression",// task label
//!     move |p| {            // the work; reports coarse progress via `p`
//!         p.info("generate", "evaluating…");
//!         p.set_fraction(0.5);        // drives a determinate footer bar
//!         tvix_wasm::eval_graph(&src) // Result<T, String>
//!     },
//! );
//!
//! // Each frame, poll for completion:
//! if let Some(result) = job.poll() {
//!     match result { Ok(v) => /* consume */, Err(e) => /* show error */ }
//! }
//! ```
//!
//! The work closure receives a [`ProgressSink`] so it can emit coarse,
//! honest progress (queued → working → done + counts). For an opaque
//! single call like `eval_graph` that's just a couple of phase log lines —
//! coarse honest progress beats a frozen tab.

use std::sync::{Arc, Mutex};

use crate::ui::progress::{ProgressSink, TaskId};

/// Which executor runs a [`BackgroundJob`]'s work.
///
/// The job's `spawn`/`poll` interface and the footer/console progress UI are
/// IDENTICAL across backends — routing only picks where the work runs. This
/// mirrors the local-vs-remote *layout* engine pattern already in the repo.
///
/// * [`Inline`](ExecutionBackend::Inline) — the original fallback:
///   native runs the closure on a real [`std::thread`] (genuinely off-thread);
///   WASM runs it paint-first-then-run inside the first `poll` (the single
///   synchronous call still blocks the frame it runs on, but the UI paints a
///   busy frame + progress first). See [`BackgroundJob::spawn`].
/// * [`Server`](ExecutionBackend::Server) — the PRIMARY WASM non-freeze: the
///   work is an async future (an HTTP call to graph-api's `/generate`), driven
///   off the egui thread via `spawn_local`/tokio. On WASM this genuinely does
///   NOT block the browser thread because the eval runs server-side and the
///   client call is async. See [`BackgroundJob::spawn_future`].
/// * [`LocalWorker`](ExecutionBackend::LocalWorker) — a standalone/offline Web
///   Worker hosting a second wasm instance running `eval_graph` off the main
///   thread, message-passing only (no SharedArrayBuffer / COOP-COEP / atomics).
///   This is the OFFLINE non-freeze: like `Server` it keeps the egui thread
///   responsive, but without a reachable graph-api.
///
///   IMPLEMENTED on wasm: the worker is the `tvix-worker` bundle (built by
///   trunk via `data-type="worker"` from `crates/tvix-worker`, see
///   `assets/index.html`); the renderer spawns it from a Blob bootstrap and
///   exchanges plain strings (expr → `{nodes,links}` JSON) — see
///   [`crate::worker`]. No hand-written `.js` file: trunk generates the worker
///   glue and the bootstrap is a Rust string whose only logic is "load wasm".
///   The earlier blocker (a presumed `wasm-bindgen` toolchain skew) did not
///   actually gate this path — `just wasm`/trunk manages its own wasm-bindgen
///   and builds both bundles cleanly; the nix `wasm-bindgen-cli` pin only
///   affects `nix build`. On NATIVE there is no Web Worker, so `LocalWorker`
///   falls back to the `Inline` executor (which already runs on a real thread).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ExecutionBackend {
    /// Run on the local executor (native thread / wasm paint-first).
    Inline,
    /// Evaluate server-side over async HTTP (the non-freezing default when a
    /// graph-api/compute URL is reachable).
    #[default]
    Server,
    /// Run in a local Web Worker (offline non-freeze on wasm; falls back to
    /// the local executor on native). See [`crate::worker`].
    LocalWorker,
}

impl ExecutionBackend {
    /// All variants, for a UI picker.
    pub const ALL: [ExecutionBackend; 3] = [
        ExecutionBackend::Server,
        ExecutionBackend::LocalWorker,
        ExecutionBackend::Inline,
    ];

    /// Short human label for the picker.
    pub fn label(self) -> &'static str {
        match self {
            ExecutionBackend::Inline => "Inline (local)",
            ExecutionBackend::Server => "Server (graph-api)",
            ExecutionBackend::LocalWorker => "Local worker",
        }
    }
}

/// The shared result slot. `None` = still running; `Some(Ok/Err)` = done.
type Slot<T> = Arc<Mutex<Option<Result<T, String>>>>;

/// Progress handle handed to a job's work closure.
///
/// Wraps the shared [`ProgressSink`] plus the job's auto-allocated footer
/// `TaskId`, so the work can drive a **determinate** progress bar on its own
/// task (`set_fraction`) and emit free-form log lines (`info`) into the debug
/// console — without juggling the task id itself. For an opaque single call
/// (`eval_graph`) the bar is coarse (e.g. 0.1 queued → 0.5 evaluating → 1.0
/// done); coarse honest progress beats a frozen tab.
#[derive(Clone)]
pub struct JobProgress {
    sink: ProgressSink,
    task: TaskId,
}

impl JobProgress {
    /// Set the determinate progress fraction (0..=1) on this job's footer
    /// task — the footer renders a `ProgressBar` once a fraction is set.
    pub fn set_fraction(&self, p: f32) {
        self.sink.set_progress(self.task, p);
    }

    /// Emit an info log line into the debug console under the given group.
    pub fn info(&self, group: impl Into<String>, msg: impl Into<String>) {
        self.sink.info(group, msg);
    }

    /// The underlying sink, for callers that want the raw warn/error API.
    pub fn sink(&self) -> &ProgressSink {
        &self.sink
    }
}

/// A single in-flight background job producing a `Result<T, String>`.
///
/// `T: Send + 'static` so the native `std::thread` path can move the work
/// across the thread boundary; on WASM the `Send` bound is harmless (the
/// work never actually crosses a thread).
pub struct BackgroundJob<T: Send + 'static> {
    slot: Slot<T>,
    /// On WASM the work is deferred to the first `poll` after spawn so egui
    /// can paint a busy frame first. `None` once it has run (or on native,
    /// where the work runs on a thread immediately).
    #[cfg(target_arch = "wasm32")]
    deferred: Option<DeferredWork<T>>,
    /// Set once `poll` has handed the caller the result, so a second poll
    /// after consumption returns `None` (the caller is expected to drop the
    /// handle, but this guards a double-take).
    consumed: bool,
}

#[cfg(target_arch = "wasm32")]
struct DeferredWork<T> {
    progress: JobProgress,
    work: Box<dyn FnOnce(&JobProgress) -> Result<T, String>>,
}

impl<T: Send + 'static> BackgroundJob<T> {
    /// Spawn a background job. Returns immediately on both targets.
    ///
    /// `sink` receives progress/log events: a `Start`/`Finish` task pair is
    /// emitted automatically around the work (so the footer shows a running
    /// task + the debug console gets the lifecycle lines). The work closure
    /// receives a [`JobProgress`] handle so it can drive a determinate
    /// progress BAR on its own task (`set_fraction`) and log coarse phases.
    pub fn spawn<F>(
        sink: ProgressSink,
        group: impl Into<String>,
        label: impl Into<String>,
        work: F,
    ) -> Self
    where
        F: FnOnce(&JobProgress) -> Result<T, String> + Send + 'static,
    {
        let slot: Slot<T> = Arc::new(Mutex::new(None));
        let group = group.into();
        let label = label.into();

        #[cfg(not(target_arch = "wasm32"))]
        {
            // Native: genuinely off-thread. The footer task is opened here so
            // it appears the instant the job is spawned, and closed by the
            // worker thread when the work returns.
            let task = sink.start(group.clone(), label);
            let progress = JobProgress {
                sink: sink.clone(),
                task,
            };
            // Seed a tiny non-zero fraction so the footer shows a determinate
            // bar immediately rather than a bare spinner.
            progress.set_fraction(0.05);
            let slot_t = slot.clone();
            std::thread::spawn(move || {
                let result = work(&progress);
                match &result {
                    Ok(_) => {
                        progress.set_fraction(1.0);
                        progress.sink.finish(task);
                    }
                    Err(e) => progress.sink.fail(task, e.clone()),
                }
                if let Ok(mut g) = slot_t.lock() {
                    *g = Some(result);
                }
            });
            Self {
                slot,
                consumed: false,
            }
        }

        #[cfg(target_arch = "wasm32")]
        {
            // WASM: paint-first-then-run. Open a "queued" task now (so the
            // footer/console show it this frame), but defer the actual work to
            // the first `poll` so egui paints a busy frame before the
            // (blocking) call. `update_label` flips it to the running label
            // when the work starts.
            let task = sink.start(group.clone(), format!("{label} (queued)"));
            // Coarse determinate bar: 0.1 while queued, the work bumps it, 1.0
            // on completion — an honest "something is happening" affordance.
            sink.set_progress(task, 0.1);
            let progress = JobProgress {
                sink: sink.clone(),
                task,
            };
            let label_run = label.clone();
            let work: Box<dyn FnOnce(&JobProgress) -> Result<T, String>> =
                Box::new(move |p: &JobProgress| {
                    p.sink.update_label(task, label_run.clone());
                    p.set_fraction(0.5);
                    let result = work(p);
                    match &result {
                        Ok(_) => {
                            p.set_fraction(1.0);
                            p.sink.finish(task);
                        }
                        Err(e) => p.sink.fail(task, e.clone()),
                    }
                    result
                });
            Self {
                slot,
                deferred: Some(DeferredWork { progress, work }),
                consumed: false,
            }
        }
    }

    /// Spawn a background job whose work is an **async future** — the
    /// [`ExecutionBackend::Server`] path. The future is driven off the egui
    /// thread (`spawn_local` on wasm, the shared tokio runtime on native), and
    /// its `Result` lands in the SAME slot `poll` reads. The `spawn`/`poll`
    /// interface and the footer/console progress are unchanged.
    ///
    /// This is the genuine WASM non-freeze: because the future is an async HTTP
    /// call to graph-api (where the heavy `eval_graph` actually runs), the
    /// browser's single egui thread is never blocked. A determinate footer bar
    /// is opened immediately and finished/failed when the future resolves.
    ///
    /// On native the future runs on the shared tokio runtime; `poll` picks the
    /// result up on a later frame, exactly like the threaded `spawn` path.
    #[cfg(target_arch = "wasm32")]
    pub fn spawn_future<Fut>(
        sink: ProgressSink,
        group: impl Into<String>,
        label: impl Into<String>,
        fut: Fut,
    ) -> Self
    where
        Fut: std::future::Future<Output = Result<T, String>> + 'static,
    {
        Self::spawn_future_impl(sink, group, label, fut)
    }

    /// Native variant: the shared tokio runtime requires a `Send` future (the
    /// `reqwest` client used by `generate_remote` is `Send`, so this holds).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn spawn_future<Fut>(
        sink: ProgressSink,
        group: impl Into<String>,
        label: impl Into<String>,
        fut: Fut,
    ) -> Self
    where
        Fut: std::future::Future<Output = Result<T, String>> + Send + 'static,
    {
        Self::spawn_future_impl(sink, group, label, fut)
    }

    #[cfg(target_arch = "wasm32")]
    fn spawn_future_impl<Fut>(
        sink: ProgressSink,
        group: impl Into<String>,
        label: impl Into<String>,
        fut: Fut,
    ) -> Self
    where
        Fut: std::future::Future<Output = Result<T, String>> + 'static,
    {
        Self::spawn_future_body(sink, group, label, fut)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn spawn_future_impl<Fut>(
        sink: ProgressSink,
        group: impl Into<String>,
        label: impl Into<String>,
        fut: Fut,
    ) -> Self
    where
        Fut: std::future::Future<Output = Result<T, String>> + Send + 'static,
    {
        Self::spawn_future_body(sink, group, label, fut)
    }

    /// Shared body. The `spawn_job_future` it calls is the cfg-gated boundary:
    /// `spawn_local` on wasm (no `Send` needed), the tokio runtime on native
    /// (requires `Send`, which the callers' bounds guarantee).
    #[cfg(target_arch = "wasm32")]
    fn spawn_future_body<Fut>(
        sink: ProgressSink,
        group: impl Into<String>,
        label: impl Into<String>,
        fut: Fut,
    ) -> Self
    where
        Fut: std::future::Future<Output = Result<T, String>> + 'static,
    {
        let slot: Slot<T> = Arc::new(Mutex::new(None));
        let group = group.into();
        let label = label.into();

        // Open the footer task now so it appears the instant the job is spawned;
        // seed a small determinate fraction so the bar shows immediately.
        let task = sink.start(group, label);
        sink.set_progress(task, 0.1);

        let slot_t = slot.clone();
        let sink_t = sink.clone();
        spawn_job_future(async move {
            let result = fut.await;
            match &result {
                Ok(_) => {
                    sink_t.set_progress(task, 1.0);
                    sink_t.finish(task);
                }
                Err(e) => sink_t.fail(task, e.clone()),
            }
            if let Ok(mut g) = slot_t.lock() {
                *g = Some(result);
            }
        });

        Self {
            slot,
            #[cfg(target_arch = "wasm32")]
            deferred: None,
            consumed: false,
        }
    }

    /// Native counterpart of [`Self::spawn_future_body`] (Send-bounded for the
    /// tokio runtime). Identical body; the trait bound differs per target so it
    /// can't be one function.
    #[cfg(not(target_arch = "wasm32"))]
    fn spawn_future_body<Fut>(
        sink: ProgressSink,
        group: impl Into<String>,
        label: impl Into<String>,
        fut: Fut,
    ) -> Self
    where
        Fut: std::future::Future<Output = Result<T, String>> + Send + 'static,
    {
        let slot: Slot<T> = Arc::new(Mutex::new(None));
        let group = group.into();
        let label = label.into();

        let task = sink.start(group, label);
        sink.set_progress(task, 0.1);

        let slot_t = slot.clone();
        let sink_t = sink.clone();
        spawn_job_future(async move {
            let result = fut.await;
            match &result {
                Ok(_) => {
                    sink_t.set_progress(task, 1.0);
                    sink_t.finish(task);
                }
                Err(e) => sink_t.fail(task, e.clone()),
            }
            if let Ok(mut g) = slot_t.lock() {
                *g = Some(result);
            }
        });

        Self {
            slot,
            consumed: false,
        }
    }

    /// Poll for completion. Returns `Some(result)` exactly once, on the first
    /// poll after the work finishes; `None` while still running (or after the
    /// result has already been taken).
    ///
    /// On WASM this also DRIVES the work: the first poll runs the deferred
    /// closure (after one paint-first frame), so callers must keep polling
    /// every frame for the job to make progress.
    pub fn poll(&mut self) -> Option<Result<T, String>> {
        if self.consumed {
            return None;
        }

        #[cfg(target_arch = "wasm32")]
        {
            // First poll after spawn: run the deferred work now. egui has
            // already painted the "queued" frame between spawn and this call.
            if let Some(d) = self.deferred.take() {
                let result = (d.work)(&d.progress);
                if let Ok(mut g) = self.slot.lock() {
                    *g = Some(result);
                }
            }
        }

        let taken = self.slot.lock().ok().and_then(|mut g| g.take());
        if taken.is_some() {
            self.consumed = true;
        }
        taken
    }

    /// `true` once the result has been handed out via `poll`. Callers
    /// typically drop the handle at that point.
    pub fn is_consumed(&self) -> bool {
        self.consumed
    }
}

/// Cross-target future spawner for the [`ExecutionBackend::Server`] path.
/// Delegates to the shared `app::spawn_async` (`spawn_local` on wasm, a
/// long-lived tokio runtime on native) so the async work runs off the egui
/// thread and its result lands in the job's slot.
#[cfg(target_arch = "wasm32")]
fn spawn_job_future<F: std::future::Future<Output = ()> + 'static>(f: F) {
    crate::app::spawn_async(f);
}

#[cfg(not(target_arch = "wasm32"))]
fn spawn_job_future<F: std::future::Future<Output = ()> + Send + 'static>(f: F) {
    crate::app::spawn_async(f);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::progress::Progress;

    /// A job that succeeds. On native it runs on a thread, so we spin the poll
    /// until it lands; on WASM-shaped logic the first poll runs it inline.
    #[test]
    fn job_yields_ok_result() {
        let progress = Progress::new();
        let mut job = BackgroundJob::spawn(progress.sink(), "test", "add", |_sink| {
            Ok::<i32, String>(2 + 2)
        });

        // Poll until the result lands (native: thread; wasm-logic: first poll).
        let out = poll_to_completion(&mut job);
        assert_eq!(out, Some(Ok(4)));
        // Second poll after consumption is None (no double-take).
        assert!(job.poll().is_none());
        assert!(job.is_consumed());
    }

    #[test]
    fn job_yields_err_result() {
        let progress = Progress::new();
        let mut job: BackgroundJob<i32> =
            BackgroundJob::spawn(progress.sink(), "test", "fail", |_sink| {
                Err("boom".to_string())
            });

        let out = poll_to_completion(&mut job);
        assert_eq!(out, Some(Err("boom".to_string())));
    }

    /// Poll a job until it yields a result, yielding the OS thread between
    /// polls so the native worker thread can actually run (a tight busy-loop
    /// can otherwise starve it on a single-core test runner).
    fn poll_to_completion<T: Send + 'static>(
        job: &mut BackgroundJob<T>,
    ) -> Option<Result<T, String>> {
        for _ in 0..100_000 {
            if let Some(r) = job.poll() {
                return Some(r);
            }
            #[cfg(not(target_arch = "wasm32"))]
            std::thread::yield_now();
        }
        None
    }

    /// The job emits a Start/Finish task pair onto the sink so the footer +
    /// debug console reflect its lifecycle.
    #[test]
    fn job_emits_progress_lifecycle() {
        let mut progress = Progress::new();
        let mut job = BackgroundJob::spawn(progress.sink(), "test", "work", |sink| {
            sink.info("test", "mid");
            Ok::<(), String>(())
        });
        for _ in 0..100_000 {
            if job.poll().is_some() {
                break;
            }
            progress.drain_sink();
            #[cfg(not(target_arch = "wasm32"))]
            std::thread::yield_now();
        }
        progress.drain_sink();
        // At least one log line was emitted (start + mid + finish all log).
        assert!(progress.log_len() >= 1, "job should have logged lifecycle");
    }
}
