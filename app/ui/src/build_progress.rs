//! Shared build-progress plumbing for a still-indexing importer source.
//!
//! When an alternate source is applied (or the boot server is still building
//! the default source), graph-api answers the graph routes with `202` + a
//! JSON status and streams granular stage/page events into that source's own
//! progress log. Two surfaces render that wait — the Importers panel's apply
//! status and the Progress panel — so the event fold, the elapsed formatter,
//! and the progress-bar markup live here and both panels reuse them.

use std::collections::VecDeque;

use dioxus::prelude::*;

use crate::api;

/// Most recent progress lines kept per build. A short tail is enough: the
/// panels show a live feed, not a scrollback.
const MAX_LINES: usize = 8;

/// A compact, newest-last tail of one source's progress log. `since` is the
/// server sequence cursor for the next `/importers/sources/{id}/progress`
/// poll; `lines` is the folded, display-ready event tail.
#[derive(Clone, Default)]
pub struct BuildFeed {
    pub since: u64,
    pub lines: VecDeque<String>,
}

impl BuildFeed {
    /// Fold one progress response into the feed: advance the cursor and append
    /// each event's display line, dropping the oldest past [`MAX_LINES`].
    pub fn fold(&mut self, resp: &api::ProgressResponse) {
        self.since = resp.next_seq;
        for stamped in &resp.events {
            if let Some(line) = event_line(&stamped.event) {
                self.lines.push_back(line);
                while self.lines.len() > MAX_LINES {
                    self.lines.pop_front();
                }
            }
        }
    }
}

/// The Progress panel's feed for the source named by `Ctx::building`. Fed by
/// the App-level poller (see `main.rs`), read by `progress_panel`; the
/// Importers panel keeps its own feed anchored to its apply generation.
pub static BUILD_FEED: GlobalSignal<BuildFeed> = Signal::global(BuildFeed::default);

/// One progress event's display line, or `None` for events that carry nothing
/// worth a row (blank labels/messages).
fn event_line(event: &api::ProgressEvent) -> Option<String> {
    let line = match event {
        api::ProgressEvent::Start { group, label, .. } => format!("{group}: {label}"),
        api::ProgressEvent::UpdateLabel { label, .. } => label.clone(),
        api::ProgressEvent::SetProgress { progress, .. } => format!("{:.0}%", progress * 100.0),
        api::ProgressEvent::Finish { .. } => "done".to_string(),
        api::ProgressEvent::Fail { reason, .. } => format!("failed: {reason}"),
        api::ProgressEvent::Log { message, .. } => message.clone(),
    };
    let line = line.trim().to_string();
    (!line.is_empty()).then_some(line)
}

/// Format an elapsed-milliseconds reading as `m:ss`. Imports run for minutes
/// at the product's target scale, so minutes never wrap to hours.
pub fn fmt_elapsed(ms: Option<u64>) -> String {
    let secs = ms.unwrap_or(0) / 1000;
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// The build progress bar: a determinate `<progress>` when the server reports
/// a fraction, otherwise a CSS-animated indeterminate bar (no JavaScript).
pub fn progress_bar(fraction: Option<f32>) -> Element {
    match fraction {
        Some(f) => {
            let f = f.clamp(0.0, 1.0);
            rsx! {
                progress {
                    class: "build-progress-bar",
                    max: "1",
                    value: "{f}",
                }
            }
        }
        None => rsx! {
            div {
                class: "build-progress-bar",
                "indeterminate": "true",
                role: "progressbar",
                aria_label: "importing",
            }
        },
    }
}
