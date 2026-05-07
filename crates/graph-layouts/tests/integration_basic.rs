//! Integration tests for graph-layouts — both placeholder smoke and
//! density regression tests for the force-directed warmup path.

use graph_layouts::warmup_positions;

#[test]
fn integration_placeholder() {
    assert!(true);
}

/// Mean nearest-neighbor distance over a packed `[x0,y0,z0,...]`
/// position buffer. O(n²) — fine for tests at n ≤ a few thousand.
fn mean_nn_distance(positions: &[f32], n: usize) -> f32 {
    assert_eq!(positions.len(), n * 3);
    let mut sum = 0.0_f64;
    for i in 0..n {
        let mut best = f32::INFINITY;
        let pi = (positions[i * 3], positions[i * 3 + 1], positions[i * 3 + 2]);
        for j in 0..n {
            if i == j { continue; }
            let dx = positions[j * 3] - pi.0;
            let dy = positions[j * 3 + 1] - pi.1;
            let dz = positions[j * 3 + 2] - pi.2;
            let d2 = dx * dx + dy * dy + dz * dz;
            if d2 < best { best = d2; }
        }
        sum += best.sqrt() as f64;
    }
    (sum / n as f64) as f32
}

/// Build a synthetic ring-of-rings: `n` nodes in a single ring with
/// every node connected to its 2 nearest neighbours. Forces a layout
/// with non-trivial spring topology so the density test isn't gamed.
fn ring_edges(n: usize) -> Vec<u32> {
    let mut edges = Vec::with_capacity(n * 2 * 2);
    for i in 0..n {
        let j = (i + 1) % n;
        edges.push(i as u32);
        edges.push(j as u32);
    }
    edges
}

/// **Density regression**: after `warmup_positions` runs, the average
/// nearest-neighbour distance must be at least 30% of the configured
/// `spring_len`. If this fails, the layout is collapsing into a tight
/// ball — the very thing the user keeps flagging.
///
/// Threshold picked conservatively: a perfect FR equilibrium would
/// land ratio ≈ 1.0; the multilevel warmup typically lands 0.5–0.8.
/// 0.3 is the tripwire below which the layout is visibly clumped.
#[test]
fn warmup_layout_meets_density_floor_n200() {
    let n = 200;
    let spring_len = 60.0;
    let edges = ring_edges(n);
    let positions = warmup_positions(n, &edges, spring_len, 0xC0FFEE);
    assert_eq!(positions.len(), n * 3);
    let nn = mean_nn_distance(&positions, n);
    let ratio = nn / spring_len;
    assert!(
        ratio > 0.30,
        "warmup layout density too high (mean NN {nn:.2} / spring_len {spring_len:.0} = {ratio:.3}, want > 0.30) — nodes are clumping"
    );
}

#[test]
fn warmup_layout_meets_density_floor_n1000() {
    let n = 1000;
    let spring_len = 200.0;
    let edges = ring_edges(n);
    let positions = warmup_positions(n, &edges, spring_len, 0xBADC0DE);
    assert_eq!(positions.len(), n * 3);
    let nn = mean_nn_distance(&positions, n);
    let ratio = nn / spring_len;
    assert!(
        ratio > 0.30,
        "warmup layout density too high (mean NN {nn:.2} / spring_len {spring_len:.0} = {ratio:.3}, want > 0.30) — nodes are clumping at scale"
    );
}
