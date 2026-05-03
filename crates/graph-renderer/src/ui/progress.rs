//! Reusable progress / log surface.
//!
//! `Progress` is a plain (non-`Sync`) struct owned by `App`. Subsystems on
//! the egui thread call `start` / `set_progress` / `finish` / `info` etc.
//! directly. Async tasks that need to publish (the bootstrap fetch, in
//! particular) push `ProgressEvent`s onto a [`ProgressSink`] — a shared
//! `Arc<Mutex<Vec<ProgressEvent>>>` — which `App::update` drains each
//! frame via [`Progress::drain_sink`].
//!
//! The matching `crate::ui::status_footer` renders a thin always-visible
//! strip at the bottom of the screen, expanding on click into a list of
//! active tasks + a scrollable log buffer.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use web_time::Instant;

pub type TaskId = u64;

/// How long a finished task lingers in `active()` before being filtered
/// out. Long enough that a sub-second fetch flashes its "✓ 12ms" once.
const FINISHED_LINGER: Duration = Duration::from_secs(2);

/// Cap on the rolling log buffer. Older entries get dropped once we
/// exceed this — the footer's scrollable log is for recent context, not
/// a permanent record.
const LOG_CAP: usize = 500;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogLevel {
    Info,
    Warn,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaskStatus {
    InProgress,
    Done,
    Failed(String),
}

#[derive(Clone, Debug)]
pub struct TaskState {
    pub id: TaskId,
    pub group: String,
    pub label: String,
    /// `None` = indeterminate spinner; `Some(0..=1)` = determinate bar.
    pub progress: Option<f32>,
    pub started_at: Instant,
    pub finished_at: Option<Instant>,
    pub status: TaskStatus,
}

impl TaskState {
    pub fn elapsed(&self) -> Duration {
        match self.finished_at {
            Some(end) => end.saturating_duration_since(self.started_at),
            None => self.started_at.elapsed(),
        }
    }

    pub fn is_finished(&self) -> bool {
        !matches!(self.status, TaskStatus::InProgress)
    }
}

#[derive(Clone, Debug)]
pub struct LogLine {
    pub at: Instant,
    pub level: LogLevel,
    pub group: String,
    pub message: String,
}

/// Async-friendly handoff: things that can't borrow `&mut Progress` push
/// events onto this and the egui thread folds them in each frame.
#[derive(Clone, Debug)]
pub enum ProgressEvent {
    Start { id: TaskId, group: String, label: String, at: Instant },
    SetProgress { id: TaskId, progress: f32 },
    UpdateLabel { id: TaskId, label: String },
    Finish { id: TaskId, at: Instant },
    Fail { id: TaskId, reason: String, at: Instant },
    Log { level: LogLevel, group: String, message: String, at: Instant },
}

/// Shared sink. Cheap to clone (one `Arc`), safe to send to async tasks.
#[derive(Clone, Default)]
pub struct ProgressSink {
    inner: Arc<Mutex<Vec<ProgressEvent>>>,
    /// Monotonic id allocator shared with `Progress` so async-issued
    /// ids never collide with ones created on the egui thread.
    next_id: Arc<std::sync::atomic::AtomicU64>,
}

impl ProgressSink {
    pub fn new() -> Self {
        Self::default()
    }

    fn alloc_id(&self) -> TaskId {
        // Single shared atomic — both `Progress::start` (egui-thread) and
        // `ProgressSink::start` (async) pull from this so ids are unique
        // across both streams.
        self.next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    fn push(&self, ev: ProgressEvent) {
        if let Ok(mut g) = self.inner.lock() {
            g.push(ev);
        }
    }

    /// Start a task; returns the id so the async caller can finish it
    /// later. The actual `TaskState` materialises on the next frame when
    /// the egui thread drains the sink.
    pub fn start(&self, group: impl Into<String>, label: impl Into<String>) -> TaskId {
        let id = self.alloc_id();
        self.push(ProgressEvent::Start {
            id,
            group: group.into(),
            label: label.into(),
            at: Instant::now(),
        });
        id
    }

    pub fn set_progress(&self, id: TaskId, p: f32) {
        self.push(ProgressEvent::SetProgress { id, progress: p });
    }

    pub fn update_label(&self, id: TaskId, label: impl Into<String>) {
        self.push(ProgressEvent::UpdateLabel { id, label: label.into() });
    }

    pub fn finish(&self, id: TaskId) {
        self.push(ProgressEvent::Finish { id, at: Instant::now() });
    }

    pub fn fail(&self, id: TaskId, reason: impl Into<String>) {
        self.push(ProgressEvent::Fail { id, reason: reason.into(), at: Instant::now() });
    }

    pub fn info(&self, group: impl Into<String>, msg: impl Into<String>) {
        self.push(ProgressEvent::Log {
            level: LogLevel::Info,
            group: group.into(),
            message: msg.into(),
            at: Instant::now(),
        });
    }

    pub fn warn(&self, group: impl Into<String>, msg: impl Into<String>) {
        self.push(ProgressEvent::Log {
            level: LogLevel::Warn,
            group: group.into(),
            message: msg.into(),
            at: Instant::now(),
        });
    }

    pub fn error(&self, group: impl Into<String>, msg: impl Into<String>) {
        self.push(ProgressEvent::Log {
            level: LogLevel::Error,
            group: group.into(),
            message: msg.into(),
            at: Instant::now(),
        });
    }
}

pub struct Progress {
    tasks: Vec<TaskState>,
    log: VecDeque<LogLine>,
    sink: ProgressSink,
}

impl Default for Progress {
    fn default() -> Self {
        Self::new()
    }
}

impl Progress {
    pub fn new() -> Self {
        Self {
            tasks: Vec::new(),
            log: VecDeque::new(),
            sink: ProgressSink::new(),
        }
    }

    /// A clone-able sink so async tasks can publish events without
    /// borrowing `&mut self`.
    pub fn sink(&self) -> ProgressSink {
        self.sink.clone()
    }

    /// Drain queued events from the sink into the live task table + log.
    /// Call once per frame from the egui thread.
    pub fn drain_sink(&mut self) {
        let drained: Vec<ProgressEvent> = {
            let Ok(mut g) = self.sink.inner.lock() else { return };
            std::mem::take(&mut *g)
        };
        for ev in drained {
            self.apply_event(ev);
        }
        self.gc_finished();
    }

    fn apply_event(&mut self, ev: ProgressEvent) {
        match ev {
            ProgressEvent::Start { id, group, label, at } => {
                self.tasks.push(TaskState {
                    id,
                    group: group.clone(),
                    label: label.clone(),
                    progress: None,
                    started_at: at,
                    finished_at: None,
                    status: TaskStatus::InProgress,
                });
                self.push_log(LogLine {
                    at,
                    level: LogLevel::Info,
                    group,
                    message: format!("{label}…"),
                });
            }
            ProgressEvent::SetProgress { id, progress } => {
                if let Some(t) = self.task_mut(id) {
                    t.progress = Some(progress.clamp(0.0, 1.0));
                }
            }
            ProgressEvent::UpdateLabel { id, label } => {
                if let Some(t) = self.task_mut(id) {
                    t.label = label;
                }
            }
            ProgressEvent::Finish { id, at } => {
                if let Some(t) = self.task_mut(id) {
                    t.status = TaskStatus::Done;
                    t.finished_at = Some(at);
                    let group = t.group.clone();
                    let label = t.label.clone();
                    let elapsed = t.elapsed();
                    self.push_log(LogLine {
                        at,
                        level: LogLevel::Info,
                        group,
                        message: format!("{label} ✓ {}", fmt_ms(elapsed)),
                    });
                }
            }
            ProgressEvent::Fail { id, reason, at } => {
                if let Some(t) = self.task_mut(id) {
                    t.status = TaskStatus::Failed(reason.clone());
                    t.finished_at = Some(at);
                    let group = t.group.clone();
                    let label = t.label.clone();
                    self.push_log(LogLine {
                        at,
                        level: LogLevel::Error,
                        group,
                        message: format!("{label} ✗ {reason}"),
                    });
                }
            }
            ProgressEvent::Log { level, group, message, at } => {
                self.push_log(LogLine { at, level, group, message });
            }
        }
    }

    fn task_mut(&mut self, id: TaskId) -> Option<&mut TaskState> {
        self.tasks.iter_mut().find(|t| t.id == id)
    }

    fn push_log(&mut self, line: LogLine) {
        if self.log.len() >= LOG_CAP {
            self.log.pop_front();
        }
        self.log.push_back(line);
    }

    /// Drop finished tasks whose `finished_at + FINISHED_LINGER` has
    /// elapsed so the active list doesn't grow unbounded.
    fn gc_finished(&mut self) {
        let now = Instant::now();
        self.tasks.retain(|t| match t.finished_at {
            Some(end) => now.saturating_duration_since(end) < FINISHED_LINGER,
            None => true,
        });
    }

    // ---- Direct (egui-thread) API ---------------------------------------

    pub fn start(&mut self, group: impl Into<String>, label: impl Into<String>) -> TaskId {
        let id = self.sink.alloc_id();
        let ev = ProgressEvent::Start {
            id,
            group: group.into(),
            label: label.into(),
            at: Instant::now(),
        };
        self.apply_event(ev);
        id
    }

    pub fn set_progress(&mut self, id: TaskId, p: f32) {
        self.apply_event(ProgressEvent::SetProgress { id, progress: p });
    }

    pub fn update_label(&mut self, id: TaskId, label: impl Into<String>) {
        self.apply_event(ProgressEvent::UpdateLabel { id, label: label.into() });
    }

    pub fn finish(&mut self, id: TaskId) {
        self.apply_event(ProgressEvent::Finish { id, at: Instant::now() });
    }

    pub fn fail(&mut self, id: TaskId, reason: impl Into<String>) {
        self.apply_event(ProgressEvent::Fail {
            id,
            reason: reason.into(),
            at: Instant::now(),
        });
    }

    pub fn info(&mut self, group: impl Into<String>, msg: impl Into<String>) {
        self.apply_event(ProgressEvent::Log {
            level: LogLevel::Info,
            group: group.into(),
            message: msg.into(),
            at: Instant::now(),
        });
    }

    pub fn warn(&mut self, group: impl Into<String>, msg: impl Into<String>) {
        self.apply_event(ProgressEvent::Log {
            level: LogLevel::Warn,
            group: group.into(),
            message: msg.into(),
            at: Instant::now(),
        });
    }

    pub fn error(&mut self, group: impl Into<String>, msg: impl Into<String>) {
        self.apply_event(ProgressEvent::Log {
            level: LogLevel::Error,
            group: group.into(),
            message: msg.into(),
            at: Instant::now(),
        });
    }

    pub fn clear_log(&mut self) {
        self.log.clear();
    }

    pub fn active(&self) -> impl Iterator<Item = &TaskState> {
        self.tasks.iter()
    }

    /// Subset of `active()` that's still in progress (no `finished_at`).
    /// Used for the "N tasks running" badge.
    pub fn in_progress(&self) -> impl Iterator<Item = &TaskState> {
        self.tasks.iter().filter(|t| !t.is_finished())
    }

    pub fn log(&self) -> impl Iterator<Item = &LogLine> {
        self.log.iter()
    }

    pub fn log_len(&self) -> usize {
        self.log.len()
    }

    pub fn has_active(&self) -> bool {
        self.in_progress().next().is_some()
    }

    /// RAII scope guard — `let _t = progress.scope("layout", "warmup")`
    /// auto-finishes on drop. Useful for CPU-side phases like the
    /// multilevel coarsening.
    pub fn scope(
        &mut self,
        group: impl Into<String>,
        label: impl Into<String>,
    ) -> ScopeGuard {
        let id = self.start(group, label);
        ScopeGuard {
            sink: self.sink.clone(),
            id,
            armed: true,
        }
    }
}

/// RAII guard returned by [`Progress::scope`]. Drops as a `Finish` event
/// on the sink, so the active task is closed even on panic. Use
/// [`ScopeGuard::fail`] to instead drop as a `Fail` event with a reason.
pub struct ScopeGuard {
    sink: ProgressSink,
    id: TaskId,
    armed: bool,
}

impl ScopeGuard {
    /// Update the label mid-scope (e.g. coarsening reports its supernode
    /// count once it's known).
    pub fn update_label(&self, label: impl Into<String>) {
        self.sink.update_label(self.id, label);
    }

    pub fn set_progress(&self, p: f32) {
        self.sink.set_progress(self.id, p);
    }

    /// Disarm the auto-finish and emit a `Fail` event instead.
    pub fn fail(mut self, reason: impl Into<String>) {
        self.sink.fail(self.id, reason);
        self.armed = false;
    }
}

impl Drop for ScopeGuard {
    fn drop(&mut self) {
        if self.armed {
            self.sink.finish(self.id);
        }
    }
}

fn fmt_ms(d: Duration) -> String {
    let ms = d.as_millis();
    if ms < 1000 {
        format!("{ms}ms")
    } else {
        format!("{:.2}s", d.as_secs_f32())
    }
}
