//! Shared fixtures + telemetry for layout-engine stability work: the
//! deterministic vault-like seed, a seeded synthetic scale-free graph (so CI
//! does not depend on the real vault snapshot), and per-step displacement
//! statistics. Used by the `graph-layout-stability` dev bin and the
//! `tests/layout_stability.rs` regression suite.

use crate::sim::CsrGraph;

/// splitmix64 — tiny deterministic RNG, stable across platforms/versions
/// (unlike `rand`, which we deliberately avoid here for fixture stability).
pub struct SplitMix64(pub u64);

impl SplitMix64 {
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform f32 in [0, 1).
    pub fn next_f32(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }
}

/// The renderer's seed convention: a uniform random ball of radius
/// `sqrt(n) · 5` — the exact regime the fa2 divergence was observed from.
pub fn ball_seed(n: usize, seed: u64) -> Vec<f32> {
    let mut rng = SplitMix64(seed);
    let radius = (n as f32).sqrt() * 5.0;
    let mut p = Vec::with_capacity(3 * n);
    for _ in 0..n {
        // Rejection-sample the unit ball (deterministic; ~52% acceptance).
        loop {
            let x = 2.0 * rng.next_f32() - 1.0;
            let y = 2.0 * rng.next_f32() - 1.0;
            let z = 2.0 * rng.next_f32() - 1.0;
            if x * x + y * y + z * z <= 1.0 {
                p.push(x * radius);
                p.push(y * radius);
                p.push(z * radius);
                break;
            }
        }
    }
    p
}

/// Seeded preferential-attachment (Barabási–Albert-style) generator: each new
/// node attaches `m` edges to endpoints sampled from the running edge list
/// (∝ degree). Matches the vault's scale-free degree profile (~9.7k nodes /
/// ~48k edges at `n=9724, m=5`) without depending on the vault snapshot.
/// Returns a symmetric CSR with sorted, deduplicated neighbor lists.
pub fn synthetic_scale_free(n: u32, m: usize, seed: u64) -> CsrGraph {
    let mut rng = SplitMix64(seed);
    let n = n.max(2) as usize;
    let m = m.max(1);
    // `targets` holds every edge endpoint twice — sampling it uniformly is
    // sampling nodes ∝ degree (the standard BA trick).
    let mut targets: Vec<u32> = vec![0, 1, 1, 0];
    let mut pairs: Vec<(u32, u32)> = vec![(0, 1)];
    for v in 2..n {
        let mut picked = Vec::with_capacity(m);
        let mut guard = 0;
        while picked.len() < m.min(v) && guard < 64 {
            guard += 1;
            let t = targets[(rng.next_u64() % targets.len() as u64) as usize];
            if t != v as u32 && !picked.contains(&t) {
                picked.push(t);
            }
        }
        for &t in &picked {
            pairs.push((v as u32, t));
            targets.push(v as u32);
            targets.push(t);
        }
    }
    csr_from_pairs(n as u32, &pairs)
}

/// Build a symmetric CSR (both directions, sorted + deduped per node) from
/// undirected edge pairs. Also the loader shape for the vault's flat-pair
/// snapshot (`/tmp/jc-edges.bin`: little-endian u32 src,tgt per edge).
pub fn csr_from_pairs(n_nodes: u32, pairs: &[(u32, u32)]) -> CsrGraph {
    let n = n_nodes as usize;
    let mut adj: Vec<Vec<u32>> = vec![Vec::new(); n];
    for &(a, b) in pairs {
        if a == b || a >= n_nodes || b >= n_nodes {
            continue;
        }
        adj[a as usize].push(b);
        adj[b as usize].push(a);
    }
    let mut offsets = Vec::with_capacity(n + 1);
    let mut neighbors = Vec::new();
    offsets.push(0u32);
    for bucket in &mut adj {
        bucket.sort_unstable();
        bucket.dedup();
        neighbors.extend_from_slice(bucket);
        offsets.push(neighbors.len() as u32);
    }
    CsrGraph {
        n_nodes,
        offsets,
        neighbors,
    }
}

/// Load a flat u32-LE edge-pair file (`src,tgt` per 8 bytes) into a CSR. Node
/// count = max id + 1.
pub fn csr_from_pair_file(path: impl AsRef<std::path::Path>) -> anyhow::Result<CsrGraph> {
    let bytes = std::fs::read(path)?;
    anyhow::ensure!(bytes.len() % 8 == 0, "pair file not a multiple of 8 bytes");
    let mut pairs = Vec::with_capacity(bytes.len() / 8);
    let mut max_id = 0u32;
    for c in bytes.chunks_exact(8) {
        let a = u32::from_le_bytes([c[0], c[1], c[2], c[3]]);
        let b = u32::from_le_bytes([c[4], c[5], c[6], c[7]]);
        max_id = max_id.max(a).max(b);
        pairs.push((a, b));
    }
    Ok(csr_from_pairs(max_id + 1, &pairs))
}

/// One step's stability telemetry.
#[derive(Clone, Copy, Debug)]
pub struct StepStats {
    /// Max |coordinate| over all nodes — the divergence detector.
    pub max_abs: f32,
    /// Median node radius |pos| — tracks the connected bulk, immune to a few
    /// runaway periphery/isolated nodes.
    pub p50_radius: f32,
    /// 99th-percentile node radius.
    pub p99_radius: f32,
    /// Mean per-node displacement magnitude vs. the previous step — the
    /// convergence detector (must decay, not oscillate, after warm-up).
    pub mean_disp: f32,
    /// Median per-node displacement.
    pub p50_disp: f32,
    /// Count of non-finite coordinates (NaN/Inf).
    pub nonfinite: usize,
}

impl StepStats {
    pub fn measure(prev: &[f32], cur: &[f32]) -> Self {
        let n = cur.len() / 3;
        let mut max_abs = 0.0f32;
        let mut sum_disp = 0.0f64;
        let mut nonfinite = 0usize;
        let mut radii = Vec::with_capacity(n);
        let mut disps = Vec::with_capacity(n);
        for i in 0..n {
            let (x, y, z) = (cur[3 * i], cur[3 * i + 1], cur[3 * i + 2]);
            if !(x.is_finite() && y.is_finite() && z.is_finite()) {
                nonfinite += 1;
                continue;
            }
            max_abs = max_abs.max(x.abs()).max(y.abs()).max(z.abs());
            radii.push((x * x + y * y + z * z).sqrt());
            let (dx, dy, dz) = (x - prev[3 * i], y - prev[3 * i + 1], z - prev[3 * i + 2]);
            let d = ((dx * dx + dy * dy + dz * dz) as f64).sqrt();
            disps.push(d as f32);
            sum_disp += d;
        }
        radii.sort_unstable_by(|a, b| a.total_cmp(b));
        disps.sort_unstable_by(|a, b| a.total_cmp(b));
        let pick = |v: &[f32], q: f64| -> f32 {
            if v.is_empty() {
                0.0
            } else {
                v[((v.len() - 1) as f64 * q) as usize]
            }
        };
        StepStats {
            max_abs,
            p50_radius: pick(&radii, 0.5),
            p99_radius: pick(&radii, 0.99),
            mean_disp: if n > 0 { (sum_disp / n as f64) as f32 } else { 0.0 },
            p50_disp: pick(&disps, 0.5),
            nonfinite,
        }
    }
}
