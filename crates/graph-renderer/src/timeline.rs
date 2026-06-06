//! Frame-buffer ring for the scrubbable simulation timeline (Phase P3).
//!
//! This is the **buffer** half of "buffer / scrub through the timeline of a
//! simulation": a bounded ring of node-position frames captured from the live
//! sim, plus the seek/reconstruct logic the scrub UI drives. It is deliberately
//! GPU-free and UI-free so it can be unit-tested in isolation — `App` feeds it
//! `positions_cpu()` each frame and reads reconstructed frames back out to push
//! through `GraphPipelines::set_positions`.
//!
//! # Compression: keyframe + per-frame delta
//!
//! Positions are `f32` xyz × n. Storing every frame raw costs `n * 3 * 4` bytes;
//! for n = 50k that is 600 KB/frame, so a few hundred frames would be hundreds
//! of MB. We compress with the classic "dirty-pages frame delta" pattern
//! (cited in `docs/reversible-timeline-plan.md` §1c):
//!
//! * Every `keyframe_interval`-th stored frame is a **keyframe**: the full raw
//!   `Vec<f32>` snapshot.
//! * Every other frame is a **delta**: `current - previous` per component, also
//!   `f32`. A reconstructed frame is `keyframe + Σ deltas` up to that index.
//!
//! Delta storage is the *same* byte size as raw in the worst case (one f32 per
//! component), but two things make it a real win in practice:
//!
//! 1. A settled / slowly-moving sim produces deltas dominated by `0.0` and tiny
//!    magnitudes; callers can additionally quantize (see the memory-budget note
//!    below). The current implementation keeps deltas as exact `f32` so the
//!    reconstruct round-trip is **bit-exact** — quantization is a documented
//!    follow-up knob.
//! 2. Reconstruction never has to walk further back than the nearest keyframe,
//!    bounding seek cost to `keyframe_interval` adds.
//!
//! # Memory budget
//!
//! Per-frame cost is `n * 3 * 4` bytes regardless of keyframe/delta (a delta is
//! one f32 per component). For a ring of `depth` frames over `n` nodes:
//!
//! ```text
//! bytes ≈ depth * n * 12
//! ```
//!
//! | n        | depth | budget   |
//! |----------|-------|----------|
//! | 1k       | 300   | ~3.4 MB  |
//! | 10k      | 300   | ~34 MB   |
//! | 50k      | 300   | ~172 MB  |
//! | 50k      | 120   | ~69 MB   |
//!
//! The default depth (300) keeps n ≤ ~10k comfortably under ~35 MB. For larger
//! graphs the UI lowers depth (or raises the capture stride K) to stay in
//! budget; a future f16/quantized delta encoding would roughly halve or quarter
//! this. The ring **evicts oldest-first** once `depth` is reached.

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
///
/// Indexing is **logical**: `get(0)` is the oldest retained frame and
/// `get(len()-1)` is the newest, regardless of how many frames have been
/// evicted. Reconstruction is delta-folded from the nearest keyframe at-or-
/// before the requested index.
#[derive(Clone, Debug)]
pub struct FrameRing {
    frames: std::collections::VecDeque<Frame>,
    /// Max retained frames before oldest-first eviction.
    depth: usize,
    /// Store a keyframe every `keyframe_interval` pushes. `1` = every frame is
    /// a keyframe (no delta compression). Must be ≥ 1.
    keyframe_interval: usize,
    /// Monotonic count of pushes since construction (NOT clamped to depth).
    /// Drives the keyframe cadence so eviction never shifts the keyframe phase.
    push_count: usize,
    /// Expected component count (`3 * n`) of the frames in this ring. Set on the
    /// first push; a push with a different length resets the ring (the graph
    /// changed out from under us).
    components: Option<usize>,
}

impl FrameRing {
    /// Build an empty ring. `depth` is clamped to ≥ 1; `keyframe_interval` to
    /// ≥ 1.
    pub fn new(depth: usize, keyframe_interval: usize) -> Self {
        Self {
            frames: std::collections::VecDeque::new(),
            depth: depth.max(1),
            keyframe_interval: keyframe_interval.max(1),
            push_count: 0,
            components: None,
        }
    }

    /// Number of frames currently retained.
    pub fn len(&self) -> usize {
        self.frames.len()
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// Configured max retained frames.
    pub fn depth(&self) -> usize {
        self.depth
    }

    /// Component count (`3 * n`) the ring currently holds, if any.
    pub fn components(&self) -> Option<usize> {
        self.components
    }

    /// Approximate retained byte budget: `Σ frame.len() * 4`.
    pub fn approx_bytes(&self) -> usize {
        self.frames.iter().map(|f| f.len() * 4).sum()
    }

    /// Drop all retained frames (e.g. on graph reload). Keeps depth /
    /// keyframe_interval config.
    pub fn clear(&mut self) {
        self.frames.clear();
        self.components = None;
        self.push_count = 0;
    }

    /// Reconfigure depth, evicting oldest frames if the new depth is smaller.
    pub fn set_depth(&mut self, depth: usize) {
        self.depth = depth.max(1);
        while self.frames.len() > self.depth {
            self.pop_front_preserving();
        }
    }

    /// Push a new position frame. Stored as a keyframe on the keyframe cadence
    /// (and always for the very first frame), otherwise as a delta against the
    /// previous logical frame. Evicts oldest-first past `depth`.
    ///
    /// A length change vs the current `components` clears the ring first (the
    /// underlying graph changed).
    pub fn push(&mut self, positions: &[f32]) {
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
            // Delta against the current newest logical frame.
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

    /// Reconstruct the absolute positions at logical index `idx` (0 = oldest
    /// retained, `len()-1` = newest). Returns `None` if `idx` is out of range.
    ///
    /// Walks back to the nearest keyframe at-or-before `idx`, then folds the
    /// intervening deltas forward — so cost is bounded by `keyframe_interval`.
    pub fn get(&self, idx: usize) -> Option<Vec<f32>> {
        self.reconstruct(idx)
    }

    /// Reconstruct the newest retained frame.
    pub fn latest(&self) -> Option<Vec<f32>> {
        if self.frames.is_empty() {
            None
        } else {
            self.reconstruct(self.frames.len() - 1)
        }
    }

    // -- internals ---------------------------------------------------------

    fn reconstruct(&self, idx: usize) -> Option<Vec<f32>> {
        if idx >= self.frames.len() {
            return None;
        }
        // Find the nearest keyframe at-or-before idx.
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

    /// Pop the oldest frame. If the *new* oldest frame is a delta (its
    /// keyframe just got evicted), promote it to a keyframe by reconstructing
    /// its absolute value first — otherwise reconstruction of every later
    /// frame would lose its base. This is what keeps `get(0)` always valid.
    fn pop_front_preserving(&mut self) {
        // Reconstruct what will become the new oldest frame BEFORE we drop the
        // current oldest, so its keyframe base is still present.
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

/// Playback state for the scrub UI. Lives on `AppState` as `#[serde(skip)]`
/// session state — never persisted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScrubState {
    /// Following the live sim: new frames append to the ring and the canvas
    /// shows the newest frame. The slider tracks the head.
    Live,
    /// Paused at a buffered frame `idx` (logical index into the ring). The live
    /// stream keeps filling the ring, but the canvas shows `idx`.
    Paused { idx: usize },
}

impl Default for ScrubState {
    fn default() -> Self {
        ScrubState::Live
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(base: f32, n: usize) -> Vec<f32> {
        (0..n * 3).map(|i| base + i as f32).collect()
    }

    #[test]
    fn push_get_len_basic() {
        let mut ring = FrameRing::new(8, 4);
        assert!(ring.is_empty());
        for k in 0..5 {
            ring.push(&frame(k as f32 * 100.0, 2));
        }
        assert_eq!(ring.len(), 5);
        for k in 0..5 {
            assert_eq!(ring.get(k).unwrap(), frame(k as f32 * 100.0, 2));
        }
        assert_eq!(ring.latest().unwrap(), frame(400.0, 2));
        assert!(ring.get(5).is_none());
    }

    #[test]
    fn delta_reconstruct_round_trip_is_bit_exact() {
        // Non-keyframe frames must reconstruct to the exact f32 pushed.
        let mut ring = FrameRing::new(64, 5);
        let mut expected = Vec::new();
        for k in 0..40 {
            let f: Vec<f32> = (0..9)
                .map(|i| (k as f32 * 1.37) + (i as f32 * 0.013) - 7.0)
                .collect();
            ring.push(&f);
            expected.push(f);
        }
        for (k, want) in expected.iter().enumerate() {
            let got = ring.get(k).unwrap();
            assert_eq!(&got, want, "frame {k} did not round-trip bit-exactly");
        }
    }

    #[test]
    fn eviction_is_oldest_first_and_keeps_get_valid() {
        // depth 4, keyframe every 3 → forces deltas to outlive their keyframe.
        let mut ring = FrameRing::new(4, 3);
        for k in 0..10 {
            ring.push(&frame(k as f32 * 10.0, 1));
        }
        assert_eq!(ring.len(), 4);
        // Logical 0..3 now map to pushes 6,7,8,9.
        let expected_bases = [60.0, 70.0, 80.0, 90.0];
        for (i, base) in expected_bases.iter().enumerate() {
            assert_eq!(
                ring.get(i).unwrap(),
                frame(*base, 1),
                "post-eviction frame {i} wrong"
            );
        }
        assert_eq!(ring.latest().unwrap(), frame(90.0, 1));
    }

    #[test]
    fn oldest_frame_is_always_reconstructable_after_keyframe_eviction() {
        // Keyframe interval > depth means at most one keyframe is ever held;
        // when it evicts, the new front must be promoted to a keyframe.
        let mut ring = FrameRing::new(3, 100);
        for k in 0..6 {
            ring.push(&frame(k as f32, 2));
        }
        assert_eq!(ring.len(), 3);
        // Every retained frame must reconstruct (no orphaned deltas).
        for i in 0..ring.len() {
            assert!(ring.get(i).is_some(), "frame {i} orphaned after eviction");
        }
        assert_eq!(ring.get(0).unwrap(), frame(3.0, 2));
        assert_eq!(ring.get(2).unwrap(), frame(5.0, 2));
    }

    #[test]
    fn length_change_resets_ring() {
        let mut ring = FrameRing::new(8, 2);
        ring.push(&frame(0.0, 3));
        ring.push(&frame(10.0, 3));
        assert_eq!(ring.components(), Some(9));
        // Different node count → ring resets, starts fresh as a keyframe.
        ring.push(&frame(0.0, 5));
        assert_eq!(ring.len(), 1);
        assert_eq!(ring.components(), Some(15));
        assert_eq!(ring.get(0).unwrap(), frame(0.0, 5));
    }

    #[test]
    fn set_depth_shrinks_and_evicts() {
        let mut ring = FrameRing::new(10, 4);
        for k in 0..10 {
            ring.push(&frame(k as f32, 1));
        }
        assert_eq!(ring.len(), 10);
        ring.set_depth(3);
        assert_eq!(ring.len(), 3);
        assert_eq!(ring.get(0).unwrap(), frame(7.0, 1));
        assert_eq!(ring.get(2).unwrap(), frame(9.0, 1));
    }

    #[test]
    fn approx_bytes_tracks_frames() {
        let mut ring = FrameRing::new(100, 4);
        assert_eq!(ring.approx_bytes(), 0);
        for k in 0..10 {
            ring.push(&frame(k as f32, 4)); // 12 components → 48 bytes each
        }
        assert_eq!(ring.approx_bytes(), 10 * 12 * 4);
    }

    #[test]
    fn empty_push_is_noop() {
        let mut ring = FrameRing::new(4, 2);
        ring.push(&[]);
        assert!(ring.is_empty());
    }
}
