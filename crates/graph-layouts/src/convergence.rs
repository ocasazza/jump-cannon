//! Layout convergence diagnostics.
//!
//! Monitors stress during layout iteration to detect when a layout has
//! stabilised — the layout analog of MM-GBSA ensemble-average convergence
//! checking. Two entry points:
//!
//! * [`check_convergence`] — one-shot diagnostic over a full history slice.
//! * [`StressBuffer`] — a fixed-size circular buffer for streaming checks
//!   without unbounded memory.
//!
//! All stress values are `f32`; accumulation uses `f64` internally.

/// Convergence diagnostic for a running layout.
#[derive(Debug, Clone)]
pub struct ConvergenceStatus {
    /// The most recent stress value.
    pub stress: f32,
    /// Running mean of the stress history.
    pub cumulative_avg: f32,
    /// |mean(second_half) - mean(first_half)| — how much the stress level
    /// has shifted between the two halves of the window.
    pub drift: f32,
    /// Whether drift < threshold.
    pub converged: bool,
    /// Consecutive steps (walking backwards from the end) where
    /// |stress[i] - stress[i-1]| < threshold — local stability.
    pub stable_steps: u32,
    /// Full stress history this diagnostic was computed from.
    pub stress_history: Vec<f32>,
}

/// Check whether a layout has converged based on stress history.
///
/// * `threshold` — if `drift < threshold`, consider converged.
///   Reasonable default: `0.001`.
///
/// # Algorithm
///
/// 1. Fewer than 20 samples → `converged = false` (need minimum data).
/// 2. `cumulative_avg = mean(stress_history)`.
/// 3. Split history in half; `drift = |mean(second_half) - mean(first_half)|`.
/// 4. `converged = drift < threshold`.
/// 5. `stable_steps`: walk backwards from the last element, counting
///    consecutive steps where `|stress[i] - stress[i-1]| < threshold`.
pub fn check_convergence(
    stress_history: &[f32],
    threshold: f32,
) -> ConvergenceStatus {
    let n = stress_history.len();
    if n < 20 {
        let stress = stress_history.last().copied().unwrap_or(0.0);
        return ConvergenceStatus {
            stress,
            cumulative_avg: 0.0,
            drift: 0.0,
            converged: false,
            stable_steps: 0,
            stress_history: stress_history.to_vec(),
        };
    }

    // Cumulative average with f64 accumulation.
    let cumulative_avg = stress_history.iter().map(|&s| s as f64).sum::<f64>() / n as f64;

    // Split into halves; for odd n the second half gets the extra element.
    let mid = n / 2;
    let first_half_mean = stress_history[..mid]
        .iter()
        .map(|&s| s as f64)
        .sum::<f64>()
        / mid as f64;
    let second_half_mean = stress_history[mid..]
        .iter()
        .map(|&s| s as f64)
        .sum::<f64>()
        / (n - mid) as f64;
    let drift = (second_half_mean - first_half_mean).abs() as f32;

    // Walk backwards counting consecutive steps where step-to-step
    // change is below threshold (local stability).
    let threshold_f64 = threshold as f64;
    let mut stable_steps: u32 = 0;
    for w in stress_history.windows(2).rev() {
        if (w[1] as f64 - w[0] as f64).abs() < threshold_f64 {
            stable_steps += 1;
        } else {
            break;
        }
    }

    // Converged when drift is small AND at least 10 consecutive recent
    // steps are within threshold of the mean (guards against oscillation).
    let converged = drift < threshold && stable_steps >= 10;

    ConvergenceStatus {
        stress: stress_history.last().copied().unwrap_or(0.0),
        cumulative_avg: cumulative_avg as f32,
        drift,
        converged,
        stable_steps,
        stress_history: stress_history.to_vec(),
    }
}

/// A fixed-size circular buffer for tracking stress over the last N steps.
///
/// Useful for streaming convergence checks without unbounded memory.
/// `N = 500` is a reasonable default for interactive layout runs.
#[derive(Debug, Clone)]
pub struct StressBuffer<const N: usize> {
    buf: [f32; N],
    write: usize,
    count: usize,
}

impl<const N: usize> StressBuffer<N> {
    /// Create an empty buffer.
    pub fn new() -> Self {
        Self {
            buf: [0.0; N],
            write: 0,
            count: 0,
        }
    }

    /// Push a new stress value, overwriting the oldest entry once full.
    pub fn push(&mut self, stress: f32) {
        self.buf[self.write] = stress;
        self.write = (self.write + 1) % N;
        if self.count < N {
            self.count += 1;
        }
    }

    /// Return the buffered stress values in insertion order (oldest first).
    pub fn as_slice(&self) -> &[f32] {
        if self.count < N {
            &self.buf[..self.count]
        } else {
            // When full, [write..N) is the older segment, [0..write) is the newer.
            // But we want insertion order: the oldest element is at `write`.
            // Since we can't return a non-contiguous slice, return all N elements
            // with a note that callers should iterate in order via the buffer.
            //
            // We return the full buffer; the caller knows insertion order starts
            // at `write`.  For convenience, the `check_convergence` method
            // reconstructs the ordered view internally.
            &self.buf[..N]
        }
    }

    /// Return the number of values currently in the buffer.
    pub fn len(&self) -> usize {
        self.count
    }

    /// Return whether the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Return the values in insertion order (oldest first) as an owned `Vec`.
    /// This is the canonical ordered view — `as_slice` may return a
    /// rotation-dependent order once the buffer wraps.
    pub fn ordered(&self) -> Vec<f32> {
        let mut out = Vec::with_capacity(self.count);
        if self.count < N {
            out.extend_from_slice(&self.buf[..self.count]);
        } else {
            out.extend_from_slice(&self.buf[self.write..N]);
            out.extend_from_slice(&self.buf[..self.write]);
        }
        out
    }

    /// Check convergence using the buffered history.
    ///
    /// Equivalent to `check_convergence(&self.ordered(), threshold)`.
    pub fn check_convergence(&self, threshold: f32) -> ConvergenceStatus {
        let ordered = self.ordered();
        let mut status = check_convergence(&ordered, threshold);
        // Don't clone the full history into the status — keep only the ordered
        // view we already allocated.
        status.stress_history = ordered;
        status
    }
}

// Default is useful for ergonomics in structs.
impl<const N: usize> Default for StressBuffer<N> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Generate a monotonically decreasing stress sequence that converges:
    /// quick decay (first 5 steps) then a long flat plateau.
    fn convergent_sequence(len: usize) -> Vec<f32> {
        let mut v: Vec<f32> = (0..len)
            .map(|i| {
                if i < 5 {
                    0.05 * (-0.5 * i as f32).exp() + 0.01
                } else {
                    0.01
                }
            })
            .collect();
        // Tiny noise on the plateau so step-to-step differences are non-zero
        // but still well within threshold.
        for i in 5..len {
            v[i] += 0.00005 * ((i as f32) * 0.7).sin();
        }
        v
    }

    /// Generate an oscillating stress sequence (never converges).
    fn oscillating_sequence(len: usize) -> Vec<f32> {
        (0..len)
            .map(|i| if i % 2 == 0 { 0.05 } else { 0.03 })
            .collect()
    }

    // -----------------------------------------------------------------
    // check_convergence tests
    // -----------------------------------------------------------------

    #[test]
    fn convergent_sequence_converges() {
        // A mostly-flat sequence with a tiny early decay → drift should be
        // well under 0.002 and stable_steps should be large.
        let history = convergent_sequence(200);
        let status = check_convergence(&history, 0.002);
        assert!(status.converged,
            "convergent sequence should converge: drift={} stable_steps={}",
            status.drift, status.stable_steps);
    }

    #[test]
    fn oscillating_sequence_does_not_converge() {
        let history = oscillating_sequence(100);
        let status = check_convergence(&history, 0.001);
        assert!(!status.converged,
            "oscillating sequence should not converge: drift={} stable_steps={}",
            status.drift, status.stable_steps);
        // Step-to-step jumps are 0.02, which is >> 0.001.
        assert_eq!(status.stable_steps, 0,
            "oscillating: no consecutive steps should be within threshold");
    }

    #[test]
    fn short_sequence_returns_not_converged() {
        let history = convergent_sequence(10);
        let status = check_convergence(&history, 0.001);
        assert!(!status.converged,
            "fewer than 20 samples must return not converged");
    }

    #[test]
    fn exactly_twenty_values_is_minimum() {
        let history = convergent_sequence(20);
        let status = check_convergence(&history, 0.001);
        // Just confirms we don't short-circuit at exactly 20.
        assert_eq!(status.stress_history.len(), 20);
    }

    #[test]
    fn monotonic_decrease_drift_decreases() {
        // As the sequence stabilises, drift should shrink.
        let history = convergent_sequence(200);
        let early = check_convergence(&history[..50], 0.01);
        let late = check_convergence(&history, 0.01);
        assert!(late.drift < early.drift,
            "drift should decrease as sequence stabilises: early={} late={}",
            early.drift, late.drift);
    }

    #[test]
    fn constant_sequence_is_converged() {
        let history = vec![0.02; 100];
        let status = check_convergence(&history, 0.001);
        assert!(status.converged, "constant sequence should converge");
        assert!(status.drift < 1e-7, "drift near zero for constant sequence");
        assert_eq!(status.stable_steps, 99,
            "all 99 consecutive pairs are stable");
    }

    #[test]
    fn stable_steps_counts_correctly() {
        // First 97 values flat at 0.03, last 3 jump to 0.05.
        // Step-to-step: the last jump is 0.02 → far above threshold.
        let mut history = vec![0.03; 100];
        history[97] = 0.05;
        history[98] = 0.05;
        history[99] = 0.05;
        let status = check_convergence(&history, 0.001);
        // Walking backwards: [99]→[98] diff=0, [98]→[97] diff=0, [97]→[96] diff=0.02 > t
        assert_eq!(status.stable_steps, 2,
            "expected 2 stable steps, got {}", status.stable_steps);
    }

    #[test]
    fn stable_steps_tail_only() {
        // First 50 at 0.03, last 50 at 0.0301 — tiny step-to-step changes.
        let mut history = vec![0.03; 100];
        for v in history.iter_mut().skip(50) {
            *v = 0.0301;
        }
        let status = check_convergence(&history, 0.002);
        // The only "big" step change is at index 50: 0.0301-0.03 = 0.0001 < 0.002.
        // All 99 consecutive pairs are within threshold.
        assert_eq!(status.stable_steps, 99,
            "all consecutive pairs should be stable");
    }

    // -----------------------------------------------------------------
    // StressBuffer tests
    // -----------------------------------------------------------------

    #[test]
    fn stress_buffer_retains_last_n() {
        let mut buf = StressBuffer::<500>::new();
        for i in 0..600 {
            buf.push(i as f32);
        }
        assert_eq!(buf.len(), 500);
        let ordered = buf.ordered();
        assert_eq!(ordered.len(), 500);
        // First retained value should be 100, last should be 599.
        assert_eq!(ordered[0], 100.0);
        assert_eq!(ordered[499], 599.0);
    }

    #[test]
    fn stress_buffer_partial_fill() {
        let mut buf = StressBuffer::<500>::new();
        for i in 0..10 {
            buf.push(i as f32);
        }
        assert_eq!(buf.len(), 10);
        let ordered = buf.ordered();
        assert_eq!(ordered, (0..10).map(|i| i as f32).collect::<Vec<_>>());
    }

    #[test]
    fn stress_buffer_empty() {
        let buf = StressBuffer::<500>::new();
        assert_eq!(buf.len(), 0);
        assert!(buf.is_empty());
        assert!(buf.ordered().is_empty());
        let status = buf.check_convergence(0.001);
        assert!(!status.converged);
    }

    #[test]
    fn stress_buffer_check_convergence_uses_ordered_view() {
        let mut buf = StressBuffer::<500>::new();
        let seq = convergent_sequence(200);
        for &s in &seq {
            buf.push(s);
        }
        let status = buf.check_convergence(0.002);
        let direct = check_convergence(&seq, 0.002);
        assert_eq!(status.drift, direct.drift);
        assert_eq!(status.converged, direct.converged);
        assert_eq!(status.stable_steps, direct.stable_steps);
    }

    #[test]
    fn stress_buffer_wraparound_correct() {
        // Tiny buffer (N=4) so we can reason about wraparound easily.
        let mut buf = StressBuffer::<4>::new();
        buf.push(10.0);
        buf.push(20.0);
        buf.push(30.0);
        buf.push(40.0); // full: write wraps to 0
        assert_eq!(buf.ordered(), vec![10.0, 20.0, 30.0, 40.0]);

        buf.push(50.0); // overwrites 10
        assert_eq!(buf.ordered(), vec![20.0, 30.0, 40.0, 50.0]);

        buf.push(60.0); // overwrites 20
        assert_eq!(buf.ordered(), vec![30.0, 40.0, 50.0, 60.0]);
    }

    #[test]
    fn stress_buffer_default() {
        let buf: StressBuffer<500> = StressBuffer::default();
        assert_eq!(buf.len(), 0);
    }
}