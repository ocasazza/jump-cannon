//! Debug panel — Dioxus port of crates/graph-renderer/src/ui/sections/debug.rs.
//!
//! Two-mode panel: an Events view that scrolls the rolling frontend-event
//! log, and a Stats view with the perf charts plus the engine-status summary.
//!
//! The egui app fills a `PerfCollector` from inside its frame loop; this
//! port has no hook into the Dioxus/wgpu frame, so a panel-local ticker
//! samples wall-clock deltas (`Date.now()` — the `web-sys` `Performance`
//! feature isn't enabled and Cargo.toml is owned elsewhere) plus the live
//! renderer state via `crate::render::with_host`. Sampling only runs while
//! the panel is mounted.

use dioxus::prelude::*;
use gloo_storage::{LocalStorage, Storage};
use serde::{Deserialize, Serialize};

use crate::graph_canvas::GraphData;
use crate::render::with_host;
use crate::Ctx;

const CHART_WINDOW_SECS: f64 = 10.0;
/// Same cap as the egui `FrontendEventLog`.
const EVENT_CAP: usize = 500;
/// 600 samples ≈ 10 s @ 60fps — same window as `PerfCollector::default`.
const SAMPLE_CAP: usize = 600;
/// Ticker period. The measured delta (timer + main-thread busy time) is
/// the frame-time proxy; 1000/delta is the FPS proxy.
const SAMPLE_MS: u32 = 16;
/// Publish every Nth sample so the DOM re-renders ~15 Hz, not 60.
const PUBLISH_EVERY: u32 = 4;
const STORE_KEY: &str = "jc_debug_v1";

// PARITY GAP: the egui PerfCollector's per-stage frame instrumentation
// (perf.rs StageId — begin_stage/end_stage around each frame phase) is
// egui-app internal; the Dioxus host's frame() has no stage hooks. The
// legend below keeps the section reading identically (labels + colors
// verbatim) but the series/stats stay empty until the host grows hooks.
#[allow(dead_code)] // legend for the per-stage overlay, pending timestamp-query timing
const STAGES: &[(&str, &str)] = &[
    ("egui central + wgpu cb", "rgb(255,80,80)"),
    ("layout dispatch", "rgb(80,200,120)"),
    ("apply style", "rgb(80,160,255)"),
    ("apply effects", "rgb(255,200,80)"),
    ("apply selection", "rgb(200,120,255)"),
    ("refresh stats", "rgb(120,220,220)"),
    ("ui chrome", "rgb(200,200,200)"),
];

// --- panel-local state ------------------------------------------------------

/// Mirror of `state.rs::DebugViewMode`.
#[derive(Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
enum ViewMode {
    Events,
    // Stats first: the perf time-series charts are the view people open
    // the Debug panel for (FPS / frame ms / render cost / KE).
    #[default]
    Stats,
}

/// Mirror of `state.rs::FrontendEvent`.
#[derive(Clone, PartialEq)]
struct DbgEvent {
    timestamp_ms: u64,
    source: String,
    message: String,
}

#[derive(Clone, Copy, PartialEq, Default)]
struct Sample {
    /// Seconds since the first sample (chart x axis).
    t: f64,
    /// Real rAF frame spacing (mean over the sample window) from
    /// `render::perf_series()` — not the ticker-delta proxy.
    frame_ms: f32,
    /// CPU cost of RenderHost::frame() (encode + submit + present), ms.
    cost_ms: f32,
    max_ke: f32,
}

/// Snapshot the ticker publishes for the Stats view — perf history plus
/// the queryable renderer state (`is_halted` / `sim_running` / counts).
#[derive(Clone, PartialEq, Default)]
struct Perf {
    samples: Vec<Sample>,
    halted: bool,
    sim_running: bool,
    host_live: bool,
    n_nodes: u32,
    n_edges: u32,
}

static VIEW: GlobalSignal<ViewMode> =
    Signal::global(|| LocalStorage::get(STORE_KEY).unwrap_or_default());
static EVENTS: GlobalSignal<Vec<DbgEvent>> = Signal::global(Vec::new);
static PERF: GlobalSignal<Perf> = Signal::global(Perf::default);
/// Epoch (ms) of the first sample ever — keeps chart time monotonic
/// across panel minimize/restore.
static T0_MS: GlobalSignal<f64> = Signal::global(js_sys::Date::now);

/// Mirror of `FrontendEventLog::push` (timestamp + cap-driven eviction).
/// Only this panel feeds the log — see the PARITY GAP note in `panel`.
fn push_event(source: &str, message: String) {
    let mut ev = EVENTS.write();
    ev.push(DbgEvent {
        timestamp_ms: js_sys::Date::now() as u64,
        source: source.into(),
        message,
    });
    let overflow = ev.len().saturating_sub(EVENT_CAP);
    if overflow > 0 {
        ev.drain(..overflow);
    }
}

// --- entry -------------------------------------------------------------------

// PARITY GAP: `state.snapshot_source = Some("Debug")` (egui snapshot-name
// plumbing) has no Dioxus counterpart.
// PARITY GAP: the egui event log is fed from every UI mutation site
// (palette execute, chip toggle, section open/close, anchored promote);
// sibling panels own those files here, so only this panel pushes events.
pub fn panel(ctx: Ctx) -> Element {
    rsx! { DebugPanel { graph: ctx.graph } }
}

#[component]
fn DebugPanel(graph: Signal<Option<GraphData>>) -> Element {
    // rAF-independent perf ticker — lives only while the panel is mounted.
    // History is carried in the GlobalSignal so reopening keeps the window.
    use_future(move || async move {
        let mut buf = PERF.read().samples.clone();
        let start = *T0_MS.read();
        let mut prev = js_sys::Date::now();
        let mut tick = 0u32;
        loop {
            gloo_timers::future::TimeoutFuture::new(SAMPLE_MS).await;
            let now = js_sys::Date::now();
            // Real frame numbers from the render loop's instrumentation;
            // ticker delta only as the no-host fallback.
            let (dts, costs) = crate::render::perf_series();
            let window = ((now - prev) / 16.7).ceil().max(1.0) as usize;
            let mean_tail = |v: &[f32]| -> f32 {
                let tail = &v[v.len().saturating_sub(window)..];
                if tail.is_empty() { 0.0 } else { tail.iter().sum::<f32>() / tail.len() as f32 }
            };
            let frame_ms =
                if dts.is_empty() { (now - prev) as f32 } else { mean_tail(&dts) };
            let cost_ms = mean_tail(&costs);
            prev = now;
            let (max_ke, halted, sim_running, n_nodes, n_edges, host_live) = with_host(|h| {
                (
                    h.pipes.last_max_ke(),
                    h.pipes.is_halted(),
                    h.pipes.sim_running(),
                    h.pipes.n_nodes(),
                    h.pipes.n_edges(),
                    true,
                )
            })
            .unwrap_or((0.0, false, false, 0, 0, false));
            buf.push(Sample { t: (now - start) / 1000.0, frame_ms, cost_ms, max_ke });
            let overflow = buf.len().saturating_sub(SAMPLE_CAP);
            if overflow > 0 {
                buf.drain(..overflow);
            }
            tick += 1;
            if tick % PUBLISH_EVERY == 0 {
                *PERF.write() = Perf {
                    samples: buf.clone(),
                    halted,
                    sim_running,
                    host_live,
                    n_nodes,
                    n_edges,
                };
            }
        }
    });

    let mode = *VIEW.read();
    rsx! {
        div { class: "dbg",
            // Mode toggle --------------------------------------------------
            div { class: "dbg-modes",
                for (m , label , to) in [(ViewMode::Events, "Events", "events"), (ViewMode::Stats, "Stats", "stats")] {
                    button {
                        class: if mode == m { "dbg-mode active" } else { "dbg-mode" },
                        onclick: move |_| {
                            if *VIEW.read() != m {
                                *VIEW.write() = m;
                                let _ = LocalStorage::set(STORE_KEY, m);
                                push_event("debug", format!("mode -> {to}"));
                            }
                        },
                        "{label}"
                    }
                }
            }
            {
                match mode {
                    ViewMode::Events => events_view(),
                    ViewMode::Stats => stats_view(graph),
                }
            }
        }
    }
}

// --- events view --------------------------------------------------------------

fn events_view() -> Element {
    let evs = EVENTS.read();
    let total = evs.len();
    rsx! {
        div { class: "dbg-evhead", "{total} event(s) (cap {EVENT_CAP})" }
        div { class: "dbg-scroll",
            if evs.is_empty() {
                div { class: "dbg-empty", "(no events yet — try the palette, a chip, or a section)" }
            }
            // Newest first.
            for (i , ev) in evs.iter().rev().enumerate() {
                div { key: "{i}", class: "dbg-evrow",
                    span { class: "dbg-time", {format_hms(ev.timestamp_ms)} }
                    span { class: "dbg-src", "{ev.source}" }
                    span { class: "dbg-msg", "{ev.message}" }
                }
            }
        }
        div { class: "dbg-actions",
            button {
                class: "dbg-btn",
                title: "Drop all event log entries",
                onclick: move |_| EVENTS.write().clear(),
                "Clear"
            }
        }
    }
}

fn format_hms(ms: u64) -> String {
    let secs_in_day = (ms / 1000) % 86_400;
    let h = secs_in_day / 3600;
    let m = (secs_in_day % 3600) / 60;
    let s = secs_in_day % 60;
    format!("{h:02}:{m:02}:{s:02}")
}

// --- stats view ----------------------------------------------------------------

fn stats_view(graph: Signal<Option<GraphData>>) -> Element {
    let perf = PERF.read().clone();
    let g = graph.read();

    // Engine status dot (was stats.rs `state.sim_status`).
    // PARITY GAP: SimStatus::Error came from the remote-layout bridge,
    // which isn't wired in this frontend — only running/settled derive
    // from the in-process sim.
    let (sim_dot, sim_label) = if perf.host_live && perf.sim_running && !perf.halted {
        ("dbg-dot green", "running")
    } else {
        ("dbg-dot yellow", "settled")
    };

    // Halted / running badge from the sim (`perf.last_halted` in egui).
    let (run_dot, run_label) = if perf.halted {
        ("dbg-dot yellow", "halted")
    } else {
        ("dbg-dot green", "running")
    };
    // PARITY GAP: `perf.last_layout_id` named whichever layout backend the
    // egui app last dispatched; the only backend wired here is the
    // in-process GPU force layout.
    let backend = if perf.host_live { "gpu-force" } else { "—" };

    let n_nodes = if perf.host_live { perf.n_nodes } else { g.as_ref().map_or(0, |g| g.n_nodes) };
    let n_edges = if perf.host_live { perf.n_edges } else { g.as_ref().map_or(0, |g| g.n_edges) };
    let n_comms = g.as_ref().map_or(0, |g| g.num_communities);
    let dash = |v: u32| if v == 0 { "—".to_string() } else { v.to_string() };
    let (n, m, c) = (dash(n_nodes), dash(n_edges), dash(n_comms));

    let fps_pts: Vec<[f64; 2]> = perf
        .samples
        .iter()
        .map(|s| [s.t, if s.frame_ms > 0.0 { 1000.0 / s.frame_ms as f64 } else { 0.0 }])
        .collect();
    let frame_pts: Vec<[f64; 2]> = perf.samples.iter().map(|s| [s.t, s.frame_ms as f64]).collect();
    let cost_pts: Vec<[f64; 2]> = perf.samples.iter().map(|s| [s.t, s.cost_ms as f64]).collect();
    let ke_pts: Vec<[f64; 2]> = perf.samples.iter().map(|s| [s.t, s.max_ke as f64]).collect();
    let fps_stats = stats3(fps_pts.iter().map(|p| p[1] as f32));
    let frame_stats = stats3(frame_pts.iter().map(|p| p[1] as f32));
    let cost_stats = stats3(cost_pts.iter().map(|p| p[1] as f32));
    let ke_stats = stats3(ke_pts.iter().map(|p| p[1] as f32));

    rsx! {
        div { class: "dbg-dotrow",
            span { class: "{sim_dot}" }
            span { class: "dbg-dotlabel", "{sim_label}" }
        }
        div { class: "dbg-dotrow",
            span { class: "{run_dot}" }
            span { class: "dbg-dotlabel", "{run_label}" }
            span { class: "dbg-backend", "backend: {backend}" }
        }

        div { class: "dbg-statblock",
            div { class: "dbg-stat", "nodes       {n}" }
            div { class: "dbg-stat", "edges       {m}" }
            div { class: "dbg-stat", "communities {c}" }
            div { class: "dbg-stat dim", "samples     {perf.samples.len()}" }
        }

        div { class: "dbg-sep" }

        // FPS ------------------------------------------------------------
        {chart_block("FPS", fps_stats_label(fps_stats), &fps_pts, "rgb(120,220,120)", false)}

        // Frame time ms ----------------------------------------------------
        {chart_block("Frame ms", ms_stats_label(frame_stats), &frame_pts, "rgb(255,220,120)", false)}

        // Render cost: CPU side of RenderHost::frame() (encode + submit +
        // present). PARITY GAP: the egui PerfCollector overlaid per-stage
        // CPU timings (StageId begin/end around each frame phase); this
        // single-pass renderer exposes one frame-cost series — finer GPU
        // stage splits need timestamp queries neither app has.
        {chart_block("Render cost ms", ms_stats_label(cost_stats), &cost_pts, "rgb(120,180,255)", false)}

        div { class: "dbg-sep" }

        // KE history -------------------------------------------------------
        {chart_block(
            "max KE",
            format!("avg {:.2} | p99 {:.2} | max {:.2}", ke_stats.0, ke_stats.1, ke_stats.2),
            &ke_pts,
            "rgb(255,120,200)",
            false,
        )}

        // Cheatsheet (relocated from sections/stats.rs in the egui app).
        div { class: "dbg-sep" }
        div { class: "dbg-grouplabel", "Cheatsheet" }
        div { class: "dbg-statblock",
            for l in [
                "WASD   pan",
                "Q/E    up / down",
                "F+drag focal plane",
                "LMB    attract",
                "RMB    repel",
                "Space  pause sim",
            ]
            {
                div { key: "{l}", class: "dbg-stat", "{l}" }
            }
        }

        // Danger zone (relocated from sections/stats.rs in the egui app).
        div { class: "dbg-sep danger-gap" }
        div { class: "dbg-danger-label", "Danger zone" }
        div { class: "dbg-actions",
            button { class: "dbg-btn danger", onclick: move |_| reset_everything(), "Reset everything" }
        }
    }
}

/// Dioxus analogue of egui's `*state = AppState::default()` — every panel
/// persists under a `jc_*` localStorage key, so drop those and reload to
/// re-initialize the whole frontend from defaults.
fn reset_everything() {
    let raw = LocalStorage::raw();
    let len = raw.length().unwrap_or(0);
    let keys: Vec<String> = (0..len)
        .filter_map(|i| raw.key(i).ok().flatten())
        .filter(|k| k.starts_with("jc_"))
        .collect();
    for k in keys {
        let _ = raw.remove_item(&k);
    }
    // `web-sys` lacks the `Location` feature here (Cargo.toml is owned
    // elsewhere) — reload via the JS global instead.
    let _ = js_sys::eval("location.reload()");
}

// --- chart helpers ---------------------------------------------------------------

/// Compact line chart: fixed window of the last [`CHART_WINDOW_SECS`],
/// y locked to include 0 (the egui plots' `include_y(0.0)`).
fn chart_block(
    title: &str,
    sub: String,
    points: &[[f64; 2]],
    color: &'static str,
    tall: bool,
) -> Element {
    let x_max = points.last().map(|p| p[0]).unwrap_or(0.0);
    let x_min = (x_max - CHART_WINDOW_SECS).max(0.0);
    let visible: Vec<&[f64; 2]> = points.iter().filter(|p| p[0] >= x_min).collect();
    let y_max = visible.iter().map(|p| p[1]).fold(0.0_f64, f64::max).max(1e-9);
    let span = (x_max - x_min).max(1e-9);
    let pts: String = visible
        .iter()
        .map(|p| {
            let x = (p[0] - x_min) / span * 100.0;
            let y = 100.0 - (p[1] / y_max * 100.0).clamp(0.0, 100.0);
            format!("{x:.2},{y:.2} ")
        })
        .collect();
    rsx! {
        div { class: "dbg-grouplabel", "{title}" }
        div { class: "dbg-sub", "{sub}" }
        svg {
            class: if tall { "dbg-chart tall" } else { "dbg-chart" },
            view_box: "0 0 100 100",
            preserve_aspect_ratio: "none",
            polyline {
                points: "{pts}",
                fill: "none",
                stroke: "{color}",
                stroke_width: "1.5",
                vector_effect: "non-scaling-stroke",
            }
        }
        div { class: "dbg-chartmax", {format!("y max {:.2}", if visible.is_empty() { 0.0 } else { y_max })} }
    }
}

/// avg / p99 / max over the buffered window — port of `PerfCollector::stats`.
fn stats3(values: impl Iterator<Item = f32>) -> (f32, f32, f32) {
    let mut buf: Vec<f32> = values.collect();
    if buf.is_empty() {
        return (0.0, 0.0, 0.0);
    }
    let sum: f32 = buf.iter().sum();
    let avg = sum / buf.len() as f32;
    buf.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let max = *buf.last().unwrap();
    let p99_idx = ((buf.len() as f32 * 0.99).floor() as usize).min(buf.len() - 1);
    let p99 = buf[p99_idx];
    (avg, p99, max)
}

fn fps_stats_label((avg, p99, max): (f32, f32, f32)) -> String {
    format!("avg {avg:.1} | p99 {p99:.1} | max {max:.1}")
}

fn ms_stats_label((avg, p99, max): (f32, f32, f32)) -> String {
    format!("avg {avg:.2}ms | p99 {p99:.2}ms | max {max:.2}ms")
}
