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
