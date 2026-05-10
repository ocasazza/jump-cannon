//! Parameterized regression tests for every static layout. Sweeps a matrix
//! of (algorithm, settings, node-count) and checks invariants:
//!
//! 1. Output length == 3 * n_nodes.
//! 2. All positions finite (no NaN/Inf).
//! 3. Layout-specific bounds (e.g. radius for sphere/circle, extent for
//!    grid/hilbert).
//! 4. Determinism — running twice with the same settings produces the same
//!    bytes (catches accidental nondeterminism from HashMap iteration etc.).
//!
//! Run: `cargo test -p graph-layouts --test regression`.

use graph_layouts::{
    CircleAxis, CircleLayout, CircleSettings, ConcentricLayout, ConcentricMetric,
    ConcentricSettings, Graph, GridLayout, GridSettings, HilbertLayout, HilbertSettings, Node,
    RandomLayout, RandomSettings, SphereLayout, SphereSettings, StaticLayout,
};

fn build_graph(n: usize) -> Graph {
    let mut g = Graph::new();
    for i in 0..n {
        g.add_node(Node::new(format!("{i:08}")));
    }
    for i in 0..n.saturating_sub(1) {
        g.add_edge(graph_layouts::Edge::new(
            format!("e{i:08}"),
            format!("{i:08}"),
            format!("{:08}", i + 1),
        ));
    }
    g
}

fn assert_finite_xyz(out: &[f32], algo: &str, n: usize) {
    assert_eq!(out.len(), 3 * n, "{algo}: wrong output length");
    for (i, v) in out.iter().enumerate() {
        assert!(v.is_finite(), "{algo}: position[{i}] is non-finite ({v})");
    }
}

fn max_radius(out: &[f32]) -> f32 {
    out.chunks(3)
        .map(|p| (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt())
        .fold(0.0f32, f32::max)
}

const SCALES: &[usize] = &[64, 1_024, 16_384];

#[test]
fn random_invariants_and_determinism() {
    for &seed in &[0xC0FFEEu64, 1, 0xDEAD_BEEF] {
        for &radius in &[1.0f32, 200.0, 5_000.0] {
            for &n in SCALES {
                let g = build_graph(n);
                let s = RandomSettings { seed, radius };
                let a = RandomLayout::solve(&s, &g).unwrap();
                let b = RandomLayout::solve(&s, &g).unwrap();
                assert_eq!(a, b, "random not deterministic at seed={seed} r={radius} n={n}");
                assert_finite_xyz(&a, "random", n);
                let r_max = max_radius(&a);
                assert!(
                    r_max <= radius * 1.001,
                    "random: max radius {r_max} > settings radius {radius} at n={n}"
                );
            }
        }
    }
}

#[test]
fn circle_invariants_and_determinism() {
    for &axis in &[CircleAxis::X, CircleAxis::Y, CircleAxis::Z] {
        for &radius in &[1.0f32, 200.0, 5_000.0] {
            for &n in SCALES {
                let g = build_graph(n);
                let s = CircleSettings { radius, axis };
                let a = CircleLayout::solve(&s, &g).unwrap();
                let b = CircleLayout::solve(&s, &g).unwrap();
                assert_eq!(a, b, "circle not deterministic axis={axis:?} r={radius} n={n}");
                assert_finite_xyz(&a, "circle", n);
                // All points should be exactly at `radius` (within f32 epsilon).
                for chunk in a.chunks(3) {
                    let r = (chunk[0] * chunk[0] + chunk[1] * chunk[1] + chunk[2] * chunk[2]).sqrt();
                    assert!(
                        (r - radius).abs() < radius * 1e-3 + 1e-5,
                        "circle: point off ring (r={r}, expected {radius})"
                    );
                }
            }
        }
    }
}

#[test]
fn grid_invariants_and_determinism() {
    for &spacing in &[1.0f32, 50.0, 250.0] {
        for &layers in &[1u32, 4, 16] {
            for &center in &[true, false] {
                for &n in SCALES {
                    let g = build_graph(n);
                    let s = GridSettings { spacing, aspect: 1.0, layers, center };
                    let a = GridLayout::solve(&s, &g).unwrap();
                    let b = GridLayout::solve(&s, &g).unwrap();
                    assert_eq!(a, b, "grid not deterministic spacing={spacing} layers={layers} center={center} n={n}");
                    assert_finite_xyz(&a, "grid", n);
                }
            }
        }
    }
}

#[test]
fn sphere_invariants_and_determinism() {
    for &radius in &[1.0f32, 200.0, 5_000.0] {
        for &jitter in &[0.0f32, 0.1, 0.5] {
            for &n in SCALES {
                let g = build_graph(n);
                let s = SphereSettings { radius, jitter, seed: 0xC0FFEE };
                let a = SphereLayout::solve(&s, &g).unwrap();
                let b = SphereLayout::solve(&s, &g).unwrap();
                assert_eq!(a, b, "sphere not deterministic r={radius} j={jitter} n={n}");
                assert_finite_xyz(&a, "sphere", n);
                let r_max = max_radius(&a);
                let allow = radius * (1.0 + jitter + 1e-3);
                assert!(
                    r_max <= allow,
                    "sphere: max radius {r_max} > allowed {allow} at r={radius} j={jitter}"
                );
            }
        }
    }
}

#[test]
fn hilbert_invariants_and_determinism() {
    for &order in &[1u32, 4, 8] {
        for &extent in &[1.0f32, 1_000.0] {
            for &flatten in &[true, false] {
                for &center in &[true, false] {
                    for &n in SCALES {
                        let g = build_graph(n);
                        let s = HilbertSettings { extent, order, flatten, center };
                        let a = HilbertLayout::solve(&s, &g).unwrap();
                        let b = HilbertLayout::solve(&s, &g).unwrap();
                        assert_eq!(a, b, "hilbert not deterministic order={order} extent={extent} n={n}");
                        assert_finite_xyz(&a, "hilbert", n);
                        let half = extent * 0.5 + 1e-3;
                        for (i, chunk) in a.chunks(3).enumerate() {
                            let bound_x = if center { half } else { extent + 1e-3 };
                            let bound_y = bound_x;
                            let bound_z = if flatten { 1e-3 } else { bound_x };
                            assert!(
                                chunk[0].abs() <= bound_x && chunk[1].abs() <= bound_y && chunk[2].abs() <= bound_z,
                                "hilbert: point {i} ({:?}) outside extent={extent} center={center} flatten={flatten}",
                                chunk
                            );
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn concentric_invariants_and_determinism() {
    for metric in &[
        ConcentricMetric::Degree,
        ConcentricMetric::InDegree,
        ConcentricMetric::OutDegree,
    ] {
        for &min_radius in &[1.0f32, 50.0] {
            for &level_spacing in &[1.0f32, 80.0] {
                for &clockwise in &[true, false] {
                    for &n in SCALES {
                        let g = build_graph(n);
                        let s = ConcentricSettings {
                            metric: *metric,
                            min_radius,
                            level_spacing,
                            clockwise,
                            bucket_count: 0,
                        };
                        let a = ConcentricLayout::solve(&s, &g).unwrap();
                        let b = ConcentricLayout::solve(&s, &g).unwrap();
                        assert_eq!(
                            a, b,
                            "concentric not deterministic metric={metric:?} n={n}"
                        );
                        assert_finite_xyz(&a, "concentric", n);
                    }
                }
            }
        }
    }
}
