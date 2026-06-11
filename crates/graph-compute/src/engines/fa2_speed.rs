//! ForceAtlas2 adaptive global speed — the host half of the stability fix.
//!
//! Faithful port of the "Auto adjust speed" block of Gephi's reference
//! `ForceAtlas2.java` (the canonical implementation of Jacomy, Venturini,
//! Heymann, Bastian 2014, PLOS ONE 9(6):e98679). Each step the GPU force pass
//! emits per-node `(mass·swing, mass·traction)` pairs
//! (`swing = |F_{t-1} − F_t|`, `traction = |F_{t-1} + F_t|/2`, `mass = deg+1`);
//! the host sums them in fixed node order (deterministic — no GPU reduction)
//! and runs the jitter-tolerance / speed-efficiency state machine to produce
//! the global speed `s(G)` the apply pass scales every displacement by:
//!
//!   factor(n) = s(G) / (1 + sqrt(s(G) · mass·swing(n)))     [per node, GPU]
//!
//! The mechanism exists precisely to prevent the divergence we measured at
//! vault scale (~10k nodes, positions ×37/step → NaN by step 23): when the
//! whole graph swings (forces flip direction step over step), `s(G)` collapses
//! multiplicatively (×0.5/×0.7 paths) and per-node swing damping shrinks the
//! worst offenders' steps; when movement is coherent, speed rises by at most
//! 50%/step (Gephi's `maxRise`).

/// Per-engine adaptive-speed state. Gephi initialises both to 1.0 at layout
/// start (`resetPropertiesValues`/`initAlgo`); engines reset it at `init`.
#[derive(Clone, Copy, Debug)]
pub struct AdaptiveSpeed {
    /// Global speed s(G) — uploaded to the shader's `params.speed`.
    pub speed: f32,
    /// Gephi's `speedEfficiency`: a slow secondary controller tracking how
    /// well `speed` maps onto the swing/traction trade-off.
    pub speed_efficiency: f32,
}

impl Default for AdaptiveSpeed {
    fn default() -> Self {
        Self {
            speed: 1.0,
            speed_efficiency: 1.0,
        }
    }
}

impl AdaptiveSpeed {
    /// Consume one step's per-node `(mass·swing, mass·traction)` stats and
    /// advance the controller, returning the new global speed. Sums run in
    /// f64 (Gephi uses doubles) in slice order, so the result is bit-stable
    /// run-to-run. Non-finite pairs are skipped (a transient NaN must not
    /// poison the controller); a degenerate step (zero swing or traction)
    /// leaves the speed unchanged.
    pub fn update(&mut self, n_nodes: u32, jitter_tolerance: f32, stats: &[f32]) -> f32 {
        let mut total_swinging = 0.0f64;
        let mut total_traction = 0.0f64;
        for pair in stats.chunks_exact(2) {
            let (swg, tra) = (pair[0], pair[1]);
            if swg.is_finite() && tra.is_finite() {
                total_swinging += swg as f64;
                total_traction += tra as f64;
            }
        }
        if !(total_swinging > 0.0) || !(total_traction > 0.0) || n_nodes == 0 {
            return self.speed;
        }
        let n = n_nodes as f64;
        let jitter_tolerance = jitter_tolerance as f64;

        // "The 'right' jitter tolerance for this network. Bigger networks need
        // more tolerance. Denser networks need less tolerance. Totally
        // empiric." — Gephi. This is the size auto-scaling: τ grows with √n
        // and with traction density.
        let estimated_optimal_jt = 0.05 * n.sqrt();
        let min_jt = estimated_optimal_jt.sqrt();
        let max_jt = 10.0f64;
        let mut jt = jitter_tolerance
            * min_jt.max(max_jt.min(estimated_optimal_jt * total_traction / (n * n)));

        let min_speed_efficiency = 0.05f32;

        // Protection against erratic behavior.
        if total_swinging / total_traction > 2.0 {
            if self.speed_efficiency > min_speed_efficiency {
                self.speed_efficiency *= 0.5;
            }
            jt = jt.max(jitter_tolerance);
        }

        let target_speed = jt * self.speed_efficiency as f64 * total_traction / total_swinging;

        // Speed efficiency is adjusted slowly and carefully.
        if total_swinging > jt * total_traction {
            if self.speed_efficiency > min_speed_efficiency {
                self.speed_efficiency *= 0.7;
            }
        } else if self.speed < 1000.0 {
            self.speed_efficiency *= 1.3;
        }

        // The speed shouldn't rise too much too quickly.
        let max_rise = 0.5f64;
        let speed = self.speed as f64;
        self.speed = (speed + (target_speed - speed).min(max_rise * speed)) as f32;
        self.speed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Coherent motion (zero swing ⇒ skipped; tiny swing) lets speed rise, but
    /// never by more than 50% per step.
    #[test]
    fn speed_rise_is_capped_at_50_percent() {
        let mut s = AdaptiveSpeed::default();
        let stats: Vec<f32> = (0..100).flat_map(|_| [0.001f32, 10.0f32]).collect();
        let mut prev = s.speed;
        for _ in 0..20 {
            let next = s.update(100, 1.0, &stats);
            assert!(next <= prev * 1.5 + 1e-3, "rise {prev} -> {next} exceeds 50%");
            assert!(next.is_finite() && next > 0.0);
            prev = next;
        }
        assert!(s.speed > 1.0, "coherent motion should accelerate");
    }

    /// Total swing (forces flip every step, swing = 2·traction + ε ⇒ the >2.0
    /// erratic branch) collapses the speed multiplicatively.
    #[test]
    fn erratic_swing_collapses_speed() {
        let mut s = AdaptiveSpeed::default();
        let stats: Vec<f32> = (0..100).flat_map(|_| [25.0f32, 1.0f32]).collect();
        for _ in 0..30 {
            s.update(100, 1.0, &stats);
        }
        assert!(
            s.speed < 0.05,
            "sustained swinging must throttle the global speed, got {}",
            s.speed
        );
    }

    /// Degenerate stats (all zero / NaN) must not move or poison the state.
    #[test]
    fn degenerate_stats_keep_speed() {
        let mut s = AdaptiveSpeed::default();
        assert_eq!(s.update(10, 1.0, &[0.0; 20]), 1.0);
        assert_eq!(s.update(10, 1.0, &[f32::NAN; 20]), 1.0);
        assert_eq!(s.update(0, 1.0, &[]), 1.0);
        assert!(s.speed_efficiency == 1.0);
    }
}
