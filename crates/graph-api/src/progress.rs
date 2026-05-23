//! Backend mirror of `graph-renderer::ui::progress::ProgressEvent`.
//!
//! Wire shape: `GET /progress?since=<seq>` returns JSON
//!
//! ```json
//! { "next_seq": 42,
//!   "server_ms": 1736_000_000_000,
//!   "events": [ { "seq": 12, "ts_ms": 1736…, "event": { … } }, … ] }
//! ```
//!
//! The renderer polls this every ~250ms while it has any in-flight task
//! (or every 2s as a heartbeat). It replays new events through its
//! `ProgressSink`, which feeds the existing footer UI — same task list,
//! same log buffer, same labels.
//!
//! `Instant` doesn't serialize, so timestamps go over the wire as
//! milliseconds since the unix epoch. The renderer reconstructs an
//! "elapsed since now" by mapping into `web_time::Instant::now()` minus
//! `(server_ms - ts_ms)`; the footer only displays elapsed durations, so
//! sub-second skew is invisible to the user.
//!
//! The backend keeps a rolling capped log (default 1024 events). Clients
//! that fall too far behind get `next_seq` advanced past their `since`
//! cursor and receive only the still-resident tail.

use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Cap on the rolling event log. Older events fall off the front once
/// this fills; matches the renderer's `LOG_CAP` headroom.
const EVENT_CAP: usize = 1024;

pub type TaskId = u64;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Info,
    Warn,
    Error,
}

/// Tagged-union mirror of `graph-renderer::ui::progress::ProgressEvent`.
/// Field names match the renderer's enum so the frontend can deserialize
/// straight into the existing sink path.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProgressEvent {
    Start { id: TaskId, group: String, label: String },
    SetProgress { id: TaskId, progress: f32 },
    UpdateLabel { id: TaskId, label: String },
    Finish { id: TaskId },
    Fail { id: TaskId, reason: String },
    Log { level: LogLevel, group: String, message: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Stamped {
    pub seq: u64,
    pub ts_ms: u64,
    pub event: ProgressEvent,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProgressResponse {
    pub next_seq: u64,
    pub server_ms: u64,
    pub events: Vec<Stamped>,
}

/// Append-only sequenced event log. `Arc<ProgressLog>` is the public
/// handle — clone it freely.
pub struct ProgressLog {
    inner: Mutex<Inner>,
}

struct Inner {
    next_seq: u64,
    next_task_id: u64,
    events: std::collections::VecDeque<Stamped>,
}

impl Default for ProgressLog {
    fn default() -> Self {
        Self::new()
    }
}

impl ProgressLog {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                next_seq: 1,
                next_task_id: 1,
                events: Default::default(),
            }),
        }
    }

    /// Allocate a new task id. Independent of `seq` so labels can update
    /// the same task across many events.
    pub fn alloc_task(&self) -> TaskId {
        let mut g = self.inner.lock().unwrap();
        let id = g.next_task_id;
        g.next_task_id = g.next_task_id.wrapping_add(1);
        id
    }

    /// Push an event. Returns the seq assigned. Trims to `EVENT_CAP`.
    pub fn push(&self, event: ProgressEvent) -> u64 {
        let ts_ms = unix_ms();
        let mut g = self.inner.lock().unwrap();
        let seq = g.next_seq;
        g.next_seq = g.next_seq.wrapping_add(1);
        g.events.push_back(Stamped { seq, ts_ms, event });
        while g.events.len() > EVENT_CAP {
            g.events.pop_front();
        }
        seq
    }

    pub fn start(&self, group: impl Into<String>, label: impl Into<String>) -> TaskId {
        let id = self.alloc_task();
        self.push(ProgressEvent::Start {
            id,
            group: group.into(),
            label: label.into(),
        });
        id
    }

    pub fn set_progress(&self, id: TaskId, progress: f32) {
        self.push(ProgressEvent::SetProgress { id, progress });
    }

    pub fn update_label(&self, id: TaskId, label: impl Into<String>) {
        self.push(ProgressEvent::UpdateLabel { id, label: label.into() });
    }

    pub fn finish(&self, id: TaskId) {
        self.push(ProgressEvent::Finish { id });
    }

    pub fn fail(&self, id: TaskId, reason: impl Into<String>) {
        self.push(ProgressEvent::Fail { id, reason: reason.into() });
    }

    pub fn info(&self, group: impl Into<String>, message: impl Into<String>) {
        self.push(ProgressEvent::Log {
            level: LogLevel::Info,
            group: group.into(),
            message: message.into(),
        });
    }

    pub fn warn(&self, group: impl Into<String>, message: impl Into<String>) {
        self.push(ProgressEvent::Log {
            level: LogLevel::Warn,
            group: group.into(),
            message: message.into(),
        });
    }

    pub fn error(&self, group: impl Into<String>, message: impl Into<String>) {
        self.push(ProgressEvent::Log {
            level: LogLevel::Error,
            group: group.into(),
            message: message.into(),
        });
    }

    /// Snapshot events with `seq >= since`.
    pub fn since(&self, since: u64) -> ProgressResponse {
        let g = self.inner.lock().unwrap();
        let events: Vec<Stamped> = g
            .events
            .iter()
            .filter(|e| e.seq >= since)
            .cloned()
            .collect();
        ProgressResponse {
            next_seq: g.next_seq,
            server_ms: unix_ms(),
            events,
        }
    }
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
