//! Timeline panel — Dioxus port of crates/graph-renderer/src/ui/sections/timeline.rs.
//!
//! Buffer + scrub the running simulation's position history. The live sim's
//! node positions are captured into a bounded delta-compressed ring (a port of
//! `crates/graph-renderer/src/timeline.rs::FrameRing`); this panel is the scrub
//! surface over that buffer:
//!
//! * a PLAY / PAUSE toggle (Live ↔ Paused),
//! * STEP-BACK / STEP-FORWARD buttons (one buffered frame at a time, paused),
//! * a scrub SLIDER over the buffered frame range,
//! * a frame-index + buffer-depth + memory readout,
//! * the capture knobs (ring depth, capture stride).
//!
//! The egui app fed the ring from `App::tick_timeline` every rendered frame.
//! There is no per-frame App hook in the Dioxus shell, so this module owns the
//! tick itself: a `spawn_forever` ticker (started on first panel mount, alive
//! for the page) captures `positions_cpu()` from the render host while live and
//! re-pushes the scrubbed frame through `GraphPipelines::set_positions` while
//! paused — the same hold-against-the-running-sim semantics as the egui app.
//!
//! Panel-local state lives in `GlobalSignal`s inside this module (not on
//! `crate::Ctx`); the capture knobs (depth, stride) persist to localStorage
//! under `jc_timeline_v1` — the Dioxus stand-in for their serde round-trip on
//! the egui `TimelineState`. The scrub position is session-only, exactly like
//! the egui `#[serde(skip)]` fields.

use std::cell::{Cell, RefCell};

use dioxus::prelude::*;
use gloo_storage::{LocalStorage, Storage};
use serde::{Deserialize, Serialize};

use crate::Ctx;

// --- frame ring (port of crates/graph-renderer/src/timeline.rs) ---------------

// The ring below is a near-verbatim port of `crates/graph-renderer/src/
// timeline.rs` (keyframe + per-frame delta compression, oldest-first eviction
// with keyframe promotion). Keep it in lockstep with the source of truth —
// its unit tests live there. `#[allow(dead_code)]` mirrors render/camera.rs:
// the full API is kept so diffs against the egui crate stay trivial.

/// One stored frame in the ring. Either a full keyframe or a delta against the
/// immediately-preceding stored frame.
#[derive(Clone, Debug)]
enum Frame {
    /// Full raw positions: `[x0,y0,z0, x1,y1,z1, ...]`, length `3 * n`.
    Key(Vec<f32>),
    /// Per-component delta vs the previous stored frame, length `3 * n`.
    Delta(Vec<f32>),
}

impl Frame {
    fn len(&self) -> usize {
        match self {
            Frame::Key(v) | Frame::Delta(v) => v.len(),
        }
    }
}

/// A bounded ring of position frames with keyframe+delta compression.
/// Indexing is logical: `get(0)` is the oldest retained frame.
#[derive(Clone, Debug)]
#[allow(dead_code)]
struct FrameRing {
    frames: std::collections::VecDeque<Frame>,
    depth: usize,
    keyframe_interval: usize,
    /// Monotonic push count — drives the keyframe cadence so eviction never
    /// shifts the keyframe phase.
    push_count: usize,
    /// Expected component count (`3 * n`); a push with a different length
    /// resets the ring (the graph changed out from under us).
    components: Option<usize>,
}

#[allow(dead_code)]
impl FrameRing {
    fn new(depth: usize, keyframe_interval: usize) -> Self {
        Self {
            frames: std::collections::VecDeque::new(),
            depth: depth.max(1),
            keyframe_interval: keyframe_interval.max(1),
            push_count: 0,
            components: None,
        }
    }

    fn len(&self) -> usize {
        self.frames.len()
    }

    fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    fn depth(&self) -> usize {
        self.depth
    }

    fn components(&self) -> Option<usize> {
        self.components
    }

    /// Approximate retained byte budget: `Σ frame.len() * 4`.
    fn approx_bytes(&self) -> usize {
        self.frames.iter().map(|f| f.len() * 4).sum()
    }

    fn clear(&mut self) {
        self.frames.clear();
        self.components = None;
        self.push_count = 0;
    }

    fn set_depth(&mut self, depth: usize) {
        self.depth = depth.max(1);
        while self.frames.len() > self.depth {
            self.pop_front_preserving();
        }
    }

    /// Push a new position frame: keyframe on the cadence (and always first),
    /// otherwise a delta vs the previous logical frame. Evicts past `depth`.
    fn push(&mut self, positions: &[f32]) {
        if positions.is_empty() {
            return;
        }
        match self.components {
            Some(c) if c != positions.len() => self.clear(),
            _ => {}
        }
        self.components = Some(positions.len());

        let is_keyframe = self.frames.is_empty() || self.push_count % self.keyframe_interval == 0;
        let frame = if is_keyframe {
            Frame::Key(positions.to_vec())
        } else {
            let prev = self
                .reconstruct(self.frames.len() - 1)
                .expect("non-empty ring has a last frame");
            let mut delta = vec![0.0_f32; positions.len()];
            for i in 0..positions.len() {
                delta[i] = positions[i] - prev[i];
            }
            Frame::Delta(delta)
        };

        self.frames.push_back(frame);
        self.push_count = self.push_count.wrapping_add(1);

        while self.frames.len() > self.depth {
            self.pop_front_preserving();
        }
    }

    /// Reconstruct absolute positions at logical index `idx`; cost is bounded
    /// by `keyframe_interval` (nearest keyframe + folded deltas).
    fn get(&self, idx: usize) -> Option<Vec<f32>> {
        self.reconstruct(idx)
    }

    fn latest(&self) -> Option<Vec<f32>> {
        if self.frames.is_empty() {
            None
        } else {
            self.reconstruct(self.frames.len() - 1)
        }
    }

    fn reconstruct(&self, idx: usize) -> Option<Vec<f32>> {
        if idx >= self.frames.len() {
            return None;
        }
        let mut key_at = idx;
        loop {
            match &self.frames[key_at] {
                Frame::Key(_) => break,
                Frame::Delta(_) => {
                    if key_at == 0 {
                        // Should never happen: the oldest retained frame is
                        // always promoted to a keyframe on eviction.
                        return None;
                    }
                    key_at -= 1;
                }
            }
        }
        let Frame::Key(base) = &self.frames[key_at] else {
            return None;
        };
        let mut acc = base.clone();
        for j in (key_at + 1)..=idx {
            if let Frame::Delta(d) = &self.frames[j] {
                for (a, dv) in acc.iter_mut().zip(d.iter()) {
                    *a += *dv;
                }
            }
        }
        Some(acc)
    }

    /// Pop the oldest frame, promoting the new front to a keyframe if its
    /// keyframe base just got evicted — keeps `get(0)` always valid.
    fn pop_front_preserving(&mut self) {
        let promote = if self.frames.len() >= 2 {
            match &self.frames[1] {
                Frame::Delta(_) => self.reconstruct(1),
                Frame::Key(_) => None,
            }
        } else {
            None
        };
        self.frames.pop_front();
        if let Some(abs) = promote {
            if let Some(front) = self.frames.front_mut() {
                *front = Frame::Key(abs);
            }
        }
    }
}

/// Playback state for the scrub UI. Session-only — never persisted (a
/// paused-at-frame-N position is meaningless once the ring is gone).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScrubState {
    /// Following the live sim: new frames append and the canvas shows the
    /// newest; the slider tracks the head.
    Live,
    /// Paused at buffered frame `idx`. Capture stops (the visible history is
    /// frozen for scrubbing) and the canvas shows `idx`.
    Paused { idx: usize },
}

// --- panel state ----------------------------------------------------------------

/// localStorage key for the persisted capture knobs.
const STORE_KEY: &str = "jc_timeline_v1";

/// Persisted capture knobs — the user PARAMETERS that round-tripped via serde
/// on the egui `TimelineState` (depth, stride). Scrub state is session-only.
///
/// PARITY GAP: the egui values also round-tripped through AppState exports /
/// share links / presets; the Dioxus app has no state-export surface yet, so
/// these persist via localStorage only.
#[derive(Clone, Copy, Serialize, Deserialize)]
struct PersistedKnobs {
    depth: usize,
    stride: usize,
}

impl Default for PersistedKnobs {
    fn default() -> Self {
        // Same defaults as the egui `TimelineState`.
        PersistedKnobs { depth: 300, stride: 1 }
    }
}

fn load_knobs() -> PersistedKnobs {
    LocalStorage::get(STORE_KEY).unwrap_or_default()
}

fn save_knobs() {
    let _ = LocalStorage::set(
        STORE_KEY,
        PersistedKnobs { depth: *DEPTH.peek(), stride: *STRIDE.peek() },
    );
}

/// Max retained frames in the ring (memory ↔ scrub history trade).
static DEPTH: GlobalSignal<usize> = Signal::global(|| load_knobs().depth);
/// Capture every `stride`-th ticker frame. `1` = every frame.
static STRIDE: GlobalSignal<usize> = Signal::global(|| load_knobs().stride);
/// Live playback / scrub position.
static SCRUB: GlobalSignal<ScrubState> = Signal::global(|| ScrubState::Live);
/// Mirror of the ring's frame count, written by the ticker so the section can
/// size the scrub slider (the egui `buffered_len` mirror).
static BUFFERED_LEN: GlobalSignal<usize> = Signal::global(|| 0);
/// Mirror of the ring's approximate byte budget, for the readout.
static BUFFERED_BYTES: GlobalSignal<usize> = Signal::global(|| 0);

thread_local! {
    // The ring is big, session-scoped data — it lives outside the signal
    // graph (mirrors the egui split: ring on `App`, mirrors on `AppState`).
    // The keyframe cadence (8) matches the egui App's ring construction.
    static RING: RefCell<FrameRing> = RefCell::new(FrameRing::new(300, 8));
    // Frame counter for the capture stride.
    static CAPTURE_IDX: Cell<u64> = const { Cell::new(0) };
    // Last (depth, stride) pushed into the ring, so a knob change reconciles.
    static PREV_KNOBS: Cell<Option<(usize, usize)>> = const { Cell::new(None) };
    // One-shot: a step/play/slider control fired — push a fresh seek to the
    // GPU even if the index didn't change. Set by the controls, cleared by
    // the ticker (the egui `seek_dirty` flag).
    static SEEK_DIRTY: Cell<bool> = const { Cell::new(false) };
    static TICKER_STARTED: Cell<bool> = const { Cell::new(false) };
}

/// Logical index the scrub UI points at: the paused frame when paused, else
/// the head (newest buffered frame). Untracked — for handlers + the ticker.
fn current_idx() -> usize {
    match *SCRUB.peek() {
        ScrubState::Paused { idx } => idx,
        ScrubState::Live => BUFFERED_LEN.peek().saturating_sub(1),
    }
}

/// Pause at `idx`, clamped to the buffered range, and flag a seek.
fn pause_at(idx: usize) {
    let max = BUFFERED_LEN.peek().saturating_sub(1);
    *SCRUB.write() = ScrubState::Paused { idx: idx.min(max) };
    SEEK_DIRTY.with(|c| c.set(true));
}

/// Resume live playback.
fn resume_live() {
    *SCRUB.write() = ScrubState::Live;
    SEEK_DIRTY.with(|c| c.set(true));
}

// --- ticker (port of App::tick_timeline) ------------------------------------------

/// Start the capture/scrub ticker once, on the root scope so it outlives the
/// panel (capture keeps buffering while the panel is minimized, like the egui
/// App). ~16ms cadence approximates the egui per-rendered-frame tick.
///
/// PARITY GAP: the egui App ticks the timeline from launch; here capture only
/// starts on the panel's first mount (there is no per-frame App hook outside
/// the render module, which is off-limits to this panel) — frames before the
/// panel is first opened are not buffered.
///
/// PARITY GAP: capture cadence is a ~16ms timer, not the renderer's frame
/// clock — `stride` counts ticker ticks, so on non-60Hz displays or under
/// load the buffered time window differs slightly from the egui
/// per-rendered-frame capture.
fn ensure_ticker() {
    if TICKER_STARTED.with(|t| t.replace(true)) {
        return;
    }
    spawn_forever(async move {
        loop {
            tick();
            gloo_timers::future::TimeoutFuture::new(16).await;
        }
    });
}

/// One timeline tick:
///
/// 1. Reconcile the ring's depth with the knobs (a change evicts immediately).
/// 2. While **live**, capture the current CPU positions on the capture stride.
///    While **paused**, do NOT capture — the visible history is frozen so the
///    user can scrub without the incoming stream sliding it out from under
///    them.
/// 3. Mirror the ring's frame count + byte budget into the signals.
/// 4. While **paused**, re-push the selected buffered frame to the GPU every
///    tick: the sim keeps stepping, so `set_positions` both holds the canvas
///    on the scrub frame and re-seeds the layout from it. On resume, push the
///    live head once so the canvas snaps back to "now".
fn tick() {
    let depth = (*DEPTH.peek()).max(1);
    let stride = (*STRIDE.peek()).max(1);

    RING.with(|r| {
        let mut ring = r.borrow_mut();

        if PREV_KNOBS.with(|k| k.get()) != Some((depth, stride)) {
            ring.set_depth(depth);
            PREV_KNOBS.with(|k| k.set(Some((depth, stride))));
        }

        // Equivalent of the egui `loaded_into_gpu` gate: no host (canvas not
        // mounted yet) or no buffers — just keep the mirrors honest.
        let loaded = crate::render::with_host(|h| h.pipes.is_loaded()).unwrap_or(false);
        if !loaded {
            mirror(&ring);
            return;
        }

        let paused = matches!(*SCRUB.peek(), ScrubState::Paused { .. });

        // Capture from the live CPU position mirror (only while live).
        if !paused {
            let n = CAPTURE_IDX.with(|c| {
                let n = c.get().wrapping_add(1);
                c.set(n);
                n
            });
            if n % stride as u64 == 0 {
                if let Some(positions) =
                    crate::render::with_host(|h| h.pipes.positions_cpu().to_vec())
                {
                    if !positions.is_empty() {
                        ring.push(&positions);
                    }
                }
            }
        }

        mirror(&ring);

        let seek_dirty = SEEK_DIRTY.with(|c| c.replace(false));
        if paused {
            let max = ring.len().saturating_sub(1);
            let idx = current_idx().min(max);
            if ring.len() > 0 {
                if let Some(positions) = ring.get(idx) {
                    push_positions_to_gpu(&positions);
                }
            }
            // Paused path always pushes; the flag is irrelevant.
        } else if seek_dirty {
            // Live: just resumed from a paused scrub — snap the canvas back
            // to the newest buffered frame so the visible state matches the
            // still-running sim instead of the frozen scrub frame.
            if let Some(positions) = ring.latest() {
                push_positions_to_gpu(&positions);
            }
        }
    });
}

/// Mirror buffer stats back for the section UI (write-on-change so the panel
/// doesn't re-render on every settled tick).
fn mirror(ring: &FrameRing) {
    let (len, bytes) = (ring.len(), ring.approx_bytes());
    if *BUFFERED_LEN.peek() != len {
        *BUFFERED_LEN.write() = len;
    }
    if *BUFFERED_BYTES.peek() != bytes {
        *BUFFERED_BYTES.write() = bytes;
    }
}

/// Write an absolute position frame into the live GPU positions buffer via
/// `GraphPipelines::set_positions` (the egui `push_positions_to_gpu`).
fn push_positions_to_gpu(positions: &[f32]) {
    crate::render::with_host(|h| {
        // `set_positions` needs `&Device` + `&Queue` + `&mut pipes` from one
        // `&mut RenderHost`, but the device is only reachable through an
        // accessor that borrows the whole host — so split through a raw
        // pointer. SAFETY: `device` and `pipes`/`queue` are disjoint
        // RenderHost fields; `pipes_and_queue` never touches, moves, or drops
        // the device, and the host itself stays pinned in the render module's
        // thread-local for the duration of this closure.
        let device: *const wgpu::Device = h.device();
        let (pipes, queue) = h.pipes_and_queue();
        if let Err(e) = pipes.set_positions(unsafe { &*device }, queue, positions) {
            tracing::warn!("[timeline] scrub set_positions: {e}");
        }
    });
}

// --- panel UI ---------------------------------------------------------------------

pub fn panel(_ctx: Ctx) -> Element {
    ensure_ticker();

    let len = *BUFFERED_LEN.read();

    if len == 0 {
        return rsx! {
            div { class: "timeline-panel",
                div { class: "tl-hint",
                    "No frames buffered yet. Start a layout / simulation; its position \
                     history streams into the ring and you can scrub it here."
                }
                hr {}
                {knobs()}
            }
        };
    }

    let max_idx = len.saturating_sub(1);
    let scrub = *SCRUB.read();
    let paused = matches!(scrub, ScrubState::Paused { .. });

    // While live the slider tracks the head; dragging it pauses and seeks.
    // While paused it drives the paused index.
    let idx = match scrub {
        ScrubState::Paused { idx } => idx,
        ScrubState::Live => max_idx,
    }
    .min(max_idx);

    let shown = idx;
    let status = if paused { "paused" } else { "live" };
    let bytes = *BUFFERED_BYTES.read();
    let depth = *DEPTH.read();

    rsx! {
        div { class: "timeline-panel",
            // ── Transport controls ──────────────────────────────────────
            div { class: "tl-transport",
                // Play/Pause toggle. Live → "⏸ Pause"; Paused → "▶ Play".
                button {
                    class: "btn",
                    title: if paused { "Resume live simulation" } else { "Pause and scrub the buffered history" },
                    onclick: move |_| {
                        if paused {
                            resume_live();
                        } else {
                            // Pause at the current head so the canvas holds
                            // this moment.
                            pause_at(max_idx);
                        }
                    },
                    if paused { "▶ Play" } else { "⏸ Pause" }
                }
                // Step-back / step-forward only make sense while paused;
                // pressing one while live first pauses at the head, then
                // steps.
                button {
                    class: "btn",
                    title: "Step one buffered frame back",
                    onclick: move |_| {
                        let cur = current_idx();
                        pause_at(cur.saturating_sub(1));
                    },
                    "⏮ Step −"
                }
                button {
                    class: "btn",
                    title: "Step one buffered frame forward",
                    onclick: move |_| {
                        let cur = current_idx();
                        if cur + 1 > max_idx {
                            // Stepping past the head returns to live.
                            resume_live();
                        } else {
                            pause_at(cur + 1);
                        }
                    },
                    "Step + ⏭"
                }
            }

            // ── Scrub slider ────────────────────────────────────────────
            div { class: "tl-row",
                span { class: "tl-label", "Frame" }
                input {
                    r#type: "range",
                    min: "0",
                    max: "{max_idx}",
                    step: "1",
                    value: "{idx}",
                    oninput: move |e| {
                        if let Ok(v) = e.value().parse::<usize>() {
                            pause_at(v);
                        }
                    },
                }
                span { class: "tl-val", "{idx}" }
            }

            // ── Readout ─────────────────────────────────────────────────
            div { class: "tl-readout", "frame {shown} / {max_idx}   ({status})" }
            div { class: "tl-readout",
                { format!("buffer {len} / {depth}   ~{}", human_bytes(bytes)) }
            }

            hr {}
            {knobs()}
        }
    }
}

/// Capture knobs: ring depth + capture stride. Both are user parameters and
/// persist (localStorage `jc_timeline_v1`).
fn knobs() -> Element {
    let depth = *DEPTH.read();
    let stride = *STRIDE.read();
    rsx! {
        div { class: "subgroup", "Capture" }
        div { class: "tl-row",
            span { class: "tl-label", "Depth (frames)" }
            input {
                r#type: "range",
                min: "30",
                max: "1000",
                step: "1",
                value: "{depth}",
                oninput: move |e| {
                    if let Ok(v) = e.value().parse::<usize>() {
                        *DEPTH.write() = v.clamp(30, 1000);
                        save_knobs();
                    }
                },
            }
            span { class: "tl-val", "{depth}" }
        }
        div { class: "tl-row",
            span { class: "tl-label", "Stride (every Nth)" }
            input {
                r#type: "range",
                min: "1",
                max: "30",
                step: "1",
                value: "{stride}",
                oninput: move |e| {
                    if let Ok(v) = e.value().parse::<usize>() {
                        *STRIDE.write() = v.clamp(1, 30);
                        save_knobs();
                    }
                },
            }
            span { class: "tl-val", "{stride}" }
        }
        div { class: "tl-hint",
            "Depth = how many frames the ring keeps; stride = capture every Nth \
             live frame. Raise stride (or lower depth) for large graphs — see the \
             memory budget in the timeline module."
        }
    }
}

/// Compact human-readable byte count for the readout.
fn human_bytes(b: usize) -> String {
    const KB: usize = 1024;
    const MB: usize = 1024 * 1024;
    if b >= MB {
        format!("{:.1} MB", b as f64 / MB as f64)
    } else if b >= KB {
        format!("{:.1} KB", b as f64 / KB as f64)
    } else {
        format!("{b} B")
    }
}
