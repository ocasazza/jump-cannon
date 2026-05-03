//! Lightweight ring-buffer perf collector for the renderer's hot path.
//!
//! Owned by `App`. The App calls `begin_frame`/`end_frame` once per
//! `update`, and wraps significant chunks of work in
//! `begin_stage`/`end_stage`. Samples land in a fixed-capacity ring
//! buffer the Debug section reads to draw line charts.
//!
//! Cross-target time source: `web_time::Instant` resolves to
//! `std::time::Instant` on native and `performance.now`-backed on WASM.
//! All hot-path bookkeeping is `O(STAGE_COUNT)` array indexing — no
//! allocations per sample (the per-sample `stages` vec is sized to the
//! discriminant count exactly once).

use std::collections::VecDeque;
use web_time::Instant;

/// Pipeline stages we instrument. The discriminant doubles as an index
/// into the `in_flight` start-time array, so keep the layout tight and
/// don't reorder casually — the `ALL` slice + `Self::COUNT` derive from
/// it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum StageId {
    /// `egui::CentralPanel` show — includes the wgpu callback (which
    /// internally records the layout compute dispatch + draws). This is
    /// the closest we can get to "compute step" timing from App::update,
    /// since `compute_step` runs inside egui_wgpu's prepare().
    EguiPaint = 0,
    /// `App::apply_layout_to_gpu` — JSON push to the layout, swap, or
    /// static solve dispatch.
    LayoutDispatch = 1,
    /// `App::apply_style_to_gpu` — recompute color/size buffers + push.
    ApplyStyle = 2,
    /// `App::apply_focus_to_gpu` + `apply_camera_to_gpu` +
    /// `apply_cursor_force` + `tick_post_click_cooldown` — the small
    /// uniform writes lumped together.
    ApplyEffects = 3,
    /// `App::apply_selection` — query evaluation + selected-set push.
    ApplySelection = 4,
    /// `App::refresh_stats` — readback mirror.
    RefreshStats = 5,
    /// Sidebar + command palette + modal egui draws (the "chrome" pass
    /// distinct from the central panel callback).
    UiChrome = 6,
}

impl StageId {
    pub const COUNT: usize = 7;
    pub const ALL: [StageId; Self::COUNT] = [
        StageId::EguiPaint,
        StageId::LayoutDispatch,
        StageId::ApplyStyle,
        StageId::ApplyEffects,
        StageId::ApplySelection,
        StageId::RefreshStats,
        StageId::UiChrome,
    ];

    pub fn idx(self) -> usize {
        self as usize
    }

    pub fn label(self) -> &'static str {
        match self {
            StageId::EguiPaint => "egui central + wgpu cb",
            StageId::LayoutDispatch => "layout dispatch",
            StageId::ApplyStyle => "apply style",
            StageId::ApplyEffects => "apply effects",
            StageId::ApplySelection => "apply selection",
            StageId::RefreshStats => "refresh stats",
            StageId::UiChrome => "ui chrome",
        }
    }

    /// Stable per-stage chart color (RGB, 0..255). Picked from the egui
    /// theme accent palette.
    pub fn color(self) -> [u8; 3] {
        match self {
            StageId::EguiPaint => [255, 80, 80],
            StageId::LayoutDispatch => [80, 200, 120],
            StageId::ApplyStyle => [80, 160, 255],
            StageId::ApplyEffects => [255, 200, 80],
            StageId::ApplySelection => [200, 120, 255],
            StageId::RefreshStats => [120, 220, 220],
            StageId::UiChrome => [200, 200, 200],
        }
    }
}

#[derive(Clone, Debug)]
pub struct PerfSample {
    /// Seconds since collector start.
    pub t: f64,
    /// Total wall time since the previous `end_frame`.
    pub frame_dt_ms: f32,
    /// Per-stage ms, indexed by `StageId::idx`. Always `StageId::COUNT`
    /// long. Stages that didn't run this frame are 0.0.
    pub stages: [f32; StageId::COUNT],
    /// Mirrored from `App::last_observed_max_ke` so the Debug panel can
    /// chart kinetic-energy decay alongside frame timings.
    pub max_ke: f32,
}

pub struct PerfCollector {
    samples: VecDeque<PerfSample>,
    capacity: usize,
    /// Per-stage `Some(start)` while a stage is in-flight, `None`
    /// otherwise. Indexed by `StageId::idx`.
    in_flight: [Option<Instant>; StageId::COUNT],
    /// Accumulated ms per stage for the current frame (set on
    /// `end_stage`). Reset each `begin_frame`.
    stages_acc: [f32; StageId::COUNT],
    frame_start: Option<Instant>,
    last_frame_end: Option<Instant>,
    origin: Instant,
    /// Latest `is_halted` snapshot mirrored by `App::refresh_stats` —
    /// surfaced in the Debug panel's running/halted badge.
    pub last_halted: bool,
    /// Mirrored backend id for the running/halted badge.
    pub last_layout_id: String,
}

impl PerfCollector {
    pub fn new(capacity: usize) -> Self {
        Self {
            samples: VecDeque::with_capacity(capacity),
            capacity,
            in_flight: [None; StageId::COUNT],
            stages_acc: [0.0; StageId::COUNT],
            frame_start: None,
            last_frame_end: None,
            origin: Instant::now(),
            last_halted: false,
            last_layout_id: String::new(),
        }
    }

    pub fn begin_frame(&mut self) {
        self.frame_start = Some(Instant::now());
        self.stages_acc = [0.0; StageId::COUNT];
        self.in_flight = [None; StageId::COUNT];
    }

    pub fn begin_stage(&mut self, id: StageId) {
        self.in_flight[id.idx()] = Some(Instant::now());
    }

    pub fn end_stage(&mut self, id: StageId) {
        if let Some(start) = self.in_flight[id.idx()].take() {
            let dt = start.elapsed().as_secs_f32() * 1000.0;
            self.stages_acc[id.idx()] += dt;
        }
    }

    /// Push a sample into the ring buffer. `max_ke` is the latest KE
    /// readback (caller's responsibility to thread through).
    pub fn end_frame(&mut self, max_ke: f32) {
        let now = Instant::now();
        let frame_dt_ms = match self.last_frame_end {
            Some(prev) => (now - prev).as_secs_f32() * 1000.0,
            None => match self.frame_start {
                Some(s) => (now - s).as_secs_f32() * 1000.0,
                None => 0.0,
            },
        };
        self.last_frame_end = Some(now);
        let t = (now - self.origin).as_secs_f64();
        let sample = PerfSample {
            t,
            frame_dt_ms,
            stages: self.stages_acc,
            max_ke,
        };
        if self.samples.len() == self.capacity {
            self.samples.pop_front();
        }
        self.samples.push_back(sample);
    }

    pub fn samples(&self) -> impl Iterator<Item = &PerfSample> {
        self.samples.iter()
    }

    pub fn len(&self) -> usize {
        self.samples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// FPS = 1000 / frame_dt_ms, plotted as `[t, fps]` pairs.
    pub fn fps_history(&self) -> Vec<[f64; 2]> {
        self.samples
            .iter()
            .map(|s| {
                let fps = if s.frame_dt_ms > 0.0 {
                    1000.0 / s.frame_dt_ms as f64
                } else {
                    0.0
                };
                [s.t, fps]
            })
            .collect()
    }

    pub fn frame_ms_history(&self) -> Vec<[f64; 2]> {
        self.samples
            .iter()
            .map(|s| [s.t, s.frame_dt_ms as f64])
            .collect()
    }

    pub fn stage_ms_history(&self, id: StageId) -> Vec<[f64; 2]> {
        let i = id.idx();
        self.samples
            .iter()
            .map(|s| [s.t, s.stages[i] as f64])
            .collect()
    }

    pub fn ke_history(&self) -> Vec<[f64; 2]> {
        self.samples
            .iter()
            .map(|s| [s.t, s.max_ke as f64])
            .collect()
    }

    /// avg / p99 / max over the buffered window for arbitrary samples.
    pub fn stats(values: impl Iterator<Item = f32>) -> (f32, f32, f32) {
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
}

impl Default for PerfCollector {
    fn default() -> Self {
        // 600 samples ≈ 10 s @ 60fps.
        Self::new(600)
    }
}
