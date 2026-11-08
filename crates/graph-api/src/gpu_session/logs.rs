//! Bounded compute-pod log tailer (Q7).
//!
//! Polls `pods/log` (no `follow=true` streams) for the Job pod's
//! `graph-compute` container while the session is admitted/starting/ready/
//! parking, and forwards each line into the `gpu-session` progress group so
//! the frontend's Progress panel renders it with zero frontend work.
//!
//! Bounding rules:
//! - `tailLines=100` on the first read of each pod generation, then
//!   `sinceTime=<last seen>` incremental reads.
//! - Per-line truncation at ~500 chars.
//! - Rate cap 60 events/min; overflow collapses into one synthesized
//!   `...N lines elided` warn line.
//! - Pod replacement (new UID) resets the cursor and emits a warn marker.

use std::sync::Arc;
use std::time::{Duration, Instant};

use k8s_openapi::api::core::v1::Pod;
use k8s_openapi::jiff::Timestamp;
use kube::api::{Api, ListParams, LogParams};
use kube::Client;

use crate::progress::ProgressLog;

use super::observe::ComputePodFacts;

/// Progress group every log line lands in (the frontend renders this group
/// in the Progress panel automatically).
pub const LOG_GROUP: &str = "gpu-session";

const CONTAINER: &str = "graph-compute";
const FIRST_READ_TAIL_LINES: i64 = 100;
const MAX_LINE_CHARS: usize = 500;
const RATE_CAP_PER_MINUTE: u32 = 60;
/// Per-call kube timeout; a stuck log read must never wedge the reconcile
/// loop.
const KUBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Cursor + rate window for the tailed container.
struct ContainerCursor {
    /// UID of the compute pod generation this cursor tracks.
    pod_uid: Option<String>,
    /// Timestamp of the last emitted line (sent back as `sinceTime`).
    since: Option<Timestamp>,
    window_start: Instant,
    window_count: u32,
    elided: u64,
}

impl ContainerCursor {
    fn new() -> Self {
        Self {
            pod_uid: None,
            since: None,
            window_start: Instant::now(),
            window_count: 0,
            elided: 0,
        }
    }

    /// True when the rate window still has budget. Rolls the window (and
    /// reports the elided count) when a minute has passed.
    fn admit(&mut self) -> Result<(), u64> {
        if self.window_start.elapsed() >= Duration::from_secs(60) {
            let elided = std::mem::take(&mut self.elided);
            self.window_start = Instant::now();
            self.window_count = 0;
            if elided > 0 {
                return Err(elided);
            }
        }
        if self.window_count >= RATE_CAP_PER_MINUTE {
            self.elided += 1;
            return Err(0);
        }
        self.window_count += 1;
        Ok(())
    }
}

/// Owned by the reconcile loop (single task) — no locking needed.
pub struct LogTailer {
    client: Client,
    namespace: String,
    progress: Arc<ProgressLog>,
    cursor: ContainerCursor,
}

impl LogTailer {
    pub fn new(client: Client, namespace: String, progress: Arc<ProgressLog>) -> Self {
        Self {
            client,
            namespace,
            progress,
            cursor: ContainerCursor::new(),
        }
    }

    /// One tail pass. `pod` is the current compute-pod facts from this
    /// tick's observation; all errors are logged and swallowed (log tailing
    /// must never flap the state machine).
    pub async fn poll(&mut self, pod: &ComputePodFacts) {
        if let Err(e) = self.poll_container(pod).await {
            tracing::warn!(
                container = CONTAINER,
                error = %format!("{e:#}"),
                "gpu-session log tail failed"
            );
        }
    }

    async fn poll_container(&mut self, pod: &ComputePodFacts) -> anyhow::Result<()> {
        let cursor = &mut self.cursor;
        if cursor.pod_uid.as_deref() != Some(pod.uid.as_str()) {
            if cursor.pod_uid.is_some() {
                self.progress.warn(
                    LOG_GROUP,
                    format!("[{CONTAINER}] compute pod replaced (now {}); log cursor reset", pod.name),
                );
            }
            *cursor = ContainerCursor::new();
            cursor.pod_uid = Some(pod.uid.clone());
        }

        let params = LogParams {
            container: Some(CONTAINER.to_string()),
            timestamps: true,
            since_time: cursor.since,
            tail_lines: (cursor.since.is_none()).then_some(FIRST_READ_TAIL_LINES),
            ..Default::default()
        };
        let pods: Api<Pod> = Api::namespaced(self.client.clone(), &self.namespace);
        let text = tokio::time::timeout(KUBE_TIMEOUT, pods.logs(&pod.name, &params))
            .await
            .map_err(|_| anyhow::anyhow!("pods/log read timed out"))??;

        // Collect before mutating the cursor so borrowck stays simple.
        let mut lines: Vec<String> = Vec::new();
        let mut last_ts: Option<Timestamp> = None;
        for raw in text.lines() {
            if raw.is_empty() {
                continue;
            }
            // timestamps=true prefixes `2024-01-01T00:00:00.123456789Z `.
            let (ts, body) = match raw.split_once(' ') {
                Some((prefix, body)) => (prefix.parse::<Timestamp>().ok(), body),
                None => (None, raw),
            };
            // sinceTime is inclusive — drop the line we already emitted.
            if let (Some(ts), Some(since)) = (ts, cursor.since) {
                if ts <= since {
                    continue;
                }
            }
            if ts.is_some() {
                last_ts = ts;
            }
            lines.push(truncate_line(body, MAX_LINE_CHARS));
        }

        for line in lines {
            match cursor.admit() {
                Ok(()) => self.progress.info(LOG_GROUP, format!("[{CONTAINER}] {line}")),
                Err(elided) if elided > 0 => self.progress.warn(
                    LOG_GROUP,
                    format!("[{CONTAINER}] ...{elided} lines elided (rate cap)"),
                ),
                Err(_) => {}
            }
        }
        if let Some(ts) = last_ts {
            cursor.since = Some(ts);
        }
        Ok(())
    }
}

/// Truncate a log line to `max` chars on a char boundary, appending an
/// ellipsis marker when truncated.
pub fn truncate_line(line: &str, max: usize) -> String {
    if line.chars().count() <= max {
        return line.to_string();
    }
    let mut out: String = line.chars().take(max).collect();
    out.push('…');
    out
}

/// Label selector for the Job's pods: the Job controller stamps every pod it
/// creates with `batch.kubernetes.io/job-name=<job name>`.
pub fn compute_pod_selector(job_name: &str) -> String {
    format!("batch.kubernetes.io/job-name={job_name}")
}

/// List the session Job's pods; fact extraction lives in `observe`. Kept here
/// so `ListParams` construction has one home.
pub async fn list_compute_pods(
    client: &Client,
    namespace: &str,
    job_name: &str,
) -> anyhow::Result<Vec<Pod>> {
    let pods: Api<Pod> = Api::namespaced(client.clone(), namespace);
    let lp = ListParams::default().labels(&compute_pod_selector(job_name));
    let list = tokio::time::timeout(KUBE_TIMEOUT, pods.list(&lp))
        .await
        .map_err(|_| anyhow::anyhow!("compute pod list timed out"))??;
    Ok(list.items)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_short_line_unchanged() {
        assert_eq!(truncate_line("hello", 500), "hello");
    }

    #[test]
    fn truncate_long_line_on_char_boundary() {
        let line = "x".repeat(600);
        let out = truncate_line(&line, 500);
        assert_eq!(out.chars().count(), 501);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn truncate_multibyte_safe() {
        let line: String = std::iter::repeat('λ').take(600).collect();
        let out = truncate_line(&line, 500);
        assert_eq!(out.chars().count(), 501);
    }

    #[test]
    fn rate_cap_elides_after_sixty_per_minute() {
        let mut c = ContainerCursor::new();
        for _ in 0..60 {
            assert!(c.admit().is_ok());
        }
        // 61st within the same window is elided.
        assert_eq!(c.admit(), Err(0));
        assert_eq!(c.elided, 1);
    }

    #[test]
    fn compute_pod_selector_uses_the_job_controller_label() {
        assert_eq!(
            compute_pod_selector("jump-cannon-compute"),
            "batch.kubernetes.io/job-name=jump-cannon-compute"
        );
    }
}
