//! Property-based fuzz tests for every static layout. Each test generates
//! random settings + random graph sizes within plausible ranges, runs the
//! solver, and asserts invariants. Failures are auto-shrunk to a minimal
//! repro.
//!
//! Volume:
//!   `cargo test -p graph-layouts --test fuzz`     # default 256 cases per test
//!   `PROPTEST_CASES=10000 cargo test ...`         # higher volume
//!   `just fuzz`                                   # 10000 cases, release build
//!
//! When proptest finds a counterexample it persists to
//! `crates/graph-layouts/proptest-regressions/` — commit those files so the
//! shrunk repro stays in CI forever.

use graph_layouts::{
    CircleAxis, CircleLayout, CircleSettings, CiseLayout, CiseSettings, ConcentricLayout,
    ConcentricMetric, ConcentricSettings, CoseBilkentLayout, CoseBilkentSettings, DagreLayout,
    DagreRanker, DagreSettings, FcoseLayout, FcoseQuality, FcoseSettings, Graph, GridLayout,
    GridSettings, HilbertLayout, HilbertSettings, KlayLayout, KlaySettings, Node, RandomLayout,
    RandomSettings, RankDirection, SphereLayout, SphereSettings, StaticLayout,
};
use proptest::prelude::*;

// ---- Graph generators ------------------------------------------------------

/// Path graph of `n` nodes. Cheap and exercises the edge pass for layouts
/// that read it. Capped low to keep fuzz iterations fast.
fn path_graph(n: usize) -> Graph {
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

fn small_n() -> impl Strategy<Value = usize> {
    // Skew toward small graphs: most cases are 1..=128, a tail goes up to 1024
    // so we still hit larger-N edge cases occasionally.
    prop_oneof![
        4 => 1usize..=128,
        1 => 128usize..=1024,
    ]
}

/// Small-N strategy for the O(n²)-per-iteration force layouts (fcose,
/// cose_bilkent) and the layered solvers (dagre, klay) whose crossing
/// minimization is super-linear. Keeps `cargo test` fast.
fn tiny_n() -> impl Strategy<Value = usize> {
    1usize..=48
}

// ---- Invariant helpers -----------------------------------------------------

fn assert_finite(out: &[f32], n: usize, algo: &str) -> Result<(), String> {
    if out.len() != 3 * n {
        return Err(format!("{algo}: len={} expected {}", out.len(), 3 * n));
    }
    for (i, v) in out.iter().enumerate() {
        if !v.is_finite() {
            return Err(format!("{algo}: pos[{i}]={v} non-finite"));
        }
    }
    Ok(())
}

fn max_radius_xy(out: &[f32]) -> f32 {
    out.chunks(3).map(|p| (p[0] * p[0] + p[1] * p[1]).sqrt()).fold(0.0f32, f32::max)
}

fn max_radius_xyz(out: &[f32]) -> f32 {
    out.chunks(3)
        .map(|p| (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt())
        .fold(0.0f32, f32::max)
}

fn assert_deterministic<S, F>(s: &S, g: &Graph, solve: F, algo: &str) -> Result<Vec<f32>, String>
where
    F: Fn(&S, &Graph) -> Result<Vec<f32>, String>,
{
    let a = solve(s, g).map_err(|e| format!("{algo}: solve A: {e}"))?;
    let b = solve(s, g).map_err(|e| format!("{algo}: solve B: {e}"))?;
    if a != b {
        return Err(format!("{algo}: nondeterministic"));
    }
    Ok(a)
}

// ---- Strategies ------------------------------------------------------------

fn random_settings() -> impl Strategy<Value = RandomSettings> {
    (any::<u64>(), 0.0f32..=10_000.0).prop_map(|(seed, radius)| RandomSettings { seed, radius })
}

fn circle_axis() -> impl Strategy<Value = CircleAxis> {
    prop_oneof![Just(CircleAxis::X), Just(CircleAxis::Y), Just(CircleAxis::Z)]
}

fn circle_settings() -> impl Strategy<Value = CircleSettings> {
    (0.0f32..=10_000.0, circle_axis()).prop_map(|(radius, axis)| CircleSettings { radius, axis })
}

fn grid_settings() -> impl Strategy<Value = GridSettings> {
    (0.1f32..=500.0, 0.25f32..=4.0, 1u32..=32, any::<bool>())
        .prop_map(|(spacing, aspect, layers, center)| GridSettings { spacing, aspect, layers, center })
}

fn sphere_settings() -> impl Strategy<Value = SphereSettings> {
    (0.0f32..=10_000.0, 0.0f32..=1.0, any::<u64>())
        .prop_map(|(radius, jitter, seed)| SphereSettings { radius, jitter, seed })
}

fn hilbert_settings() -> impl Strategy<Value = HilbertSettings> {
    (1.0f32..=10_000.0, 1u32..=10, any::<bool>(), any::<bool>())
        .prop_map(|(extent, order, flatten, center)| HilbertSettings { extent, order, flatten, center })
}

fn concentric_metric() -> impl Strategy<Value = ConcentricMetric> {
    prop_oneof![
        Just(ConcentricMetric::Degree),
        Just(ConcentricMetric::InDegree),
        Just(ConcentricMetric::OutDegree),
    ]
}

fn concentric_settings() -> impl Strategy<Value = ConcentricSettings> {
    (concentric_metric(), 0.1f32..=2_000.0, 0.1f32..=500.0, any::<bool>(), 0u32..=64).prop_map(
        |(metric, min_radius, level_spacing, clockwise, bucket_count)| ConcentricSettings {
            metric,
            min_radius,
            level_spacing,
            clockwise,
            bucket_count,
        },
    )
}

fn fcose_quality() -> impl Strategy<Value = FcoseQuality> {
    prop_oneof![
        Just(FcoseQuality::Draft),
        Just(FcoseQuality::Default),
        Just(FcoseQuality::Proof),
    ]
}

fn fcose_settings() -> impl Strategy<Value = FcoseSettings> {
    (100.0f64..=10_000.0, 10.0f64..=300.0, 0.0f64..=100.0, fcose_quality(), any::<u64>()).prop_map(
        |(node_repulsion, ideal_edge_length, node_overlap, quality, seed)| FcoseSettings {
            node_repulsion,
            ideal_edge_length,
            node_overlap,
            quality,
            seed,
        },
    )
}

fn cose_bilkent_settings() -> impl Strategy<Value = CoseBilkentSettings> {
    (100.0f64..=10_000.0, 10.0f64..=300.0, 0u32..=120, any::<u64>()).prop_map(
        |(node_repulsion, ideal_edge_length, iterations, seed)| CoseBilkentSettings {
            node_repulsion,
            ideal_edge_length,
            iterations,
            seed,
        },
    )
}

fn cise_settings() -> impl Strategy<Value = CiseSettings> {
    (1.0f64..=200.0).prop_map(|circle_spacing| CiseSettings {
        clusters: Vec::new(),
        circle_spacing,
    })
}

fn rank_direction() -> impl Strategy<Value = RankDirection> {
    prop_oneof![
        Just(RankDirection::TB),
        Just(RankDirection::BT),
        Just(RankDirection::LR),
        Just(RankDirection::RL),
    ]
}

fn dagre_ranker() -> impl Strategy<Value = DagreRanker> {
    prop_oneof![
        Just(DagreRanker::NetworkSimplex),
        Just(DagreRanker::TightTree),
        Just(DagreRanker::LongestPath),
    ]
}

fn dagre_settings() -> impl Strategy<Value = DagreSettings> {
    (rank_direction(), dagre_ranker(), 10.0f64..=300.0, 10.0f64..=300.0, any::<bool>()).prop_map(
        |(rank_direction, ranker, rank_separation, node_separation, acyclic)| DagreSettings {
            rank_direction,
            ranker,
            rank_separation,
            node_separation,
            acyclic,
        },
    )
}

fn klay_settings() -> impl Strategy<Value = KlaySettings> {
    (10.0f64..=300.0, 10.0f64..=300.0)
        .prop_map(|(layer_spacing, node_spacing)| KlaySettings { layer_spacing, node_spacing })
}

// ---- Property tests --------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig {
        // Compromise: 256 default cases is too low for fuzz-style coverage but
        // fast enough to keep `cargo test` quick. `PROPTEST_CASES=N` overrides.
        cases: 256,
        max_shrink_iters: 8192,
        ..ProptestConfig::default()
    })]

    #[test]
    fn fuzz_random(s in random_settings(), n in small_n()) {
        let g = path_graph(n);
        let out = assert_deterministic(&s, &g, RandomLayout::solve, "random").map_err(TestCaseError::fail)?;
        assert_finite(&out, n, "random").map_err(TestCaseError::fail)?;
        let r = max_radius_xyz(&out);
        prop_assert!(
            r <= s.radius * 1.001 + 1e-3,
            "random: max_radius {} > settings radius {}", r, s.radius
        );
    }

    #[test]
    fn fuzz_circle(s in circle_settings(), n in small_n()) {
        let g = path_graph(n);
        let out = assert_deterministic(&s, &g, CircleLayout::solve, "circle").map_err(TestCaseError::fail)?;
        assert_finite(&out, n, "circle").map_err(TestCaseError::fail)?;
        if s.radius > 0.0 {
            for chunk in out.chunks(3) {
                let r = (chunk[0] * chunk[0] + chunk[1] * chunk[1] + chunk[2] * chunk[2]).sqrt();
                prop_assert!(
                    (r - s.radius).abs() <= s.radius * 1e-3 + 1e-4,
                    "circle: r={} off ring (expected {})", r, s.radius
                );
            }
        }
    }

    #[test]
    fn fuzz_grid(s in grid_settings(), n in small_n()) {
        let g = path_graph(n);
        let out = assert_deterministic(&s, &g, GridLayout::solve, "grid").map_err(TestCaseError::fail)?;
        assert_finite(&out, n, "grid").map_err(TestCaseError::fail)?;
    }

    #[test]
    fn fuzz_sphere(s in sphere_settings(), n in small_n()) {
        let g = path_graph(n);
        let out = assert_deterministic(&s, &g, SphereLayout::solve, "sphere").map_err(TestCaseError::fail)?;
        assert_finite(&out, n, "sphere").map_err(TestCaseError::fail)?;
        let r = max_radius_xyz(&out);
        // Jitter perturbs along the radial direction, so the bound is
        // radius * (1 + jitter).
        let allow = s.radius * (1.0 + s.jitter) + 1e-3;
        prop_assert!(r <= allow, "sphere: r={r} > allow={allow}");
    }

    #[test]
    fn fuzz_hilbert(s in hilbert_settings(), n in small_n()) {
        let g = path_graph(n);
        let out = assert_deterministic(&s, &g, HilbertLayout::solve, "hilbert").map_err(TestCaseError::fail)?;
        assert_finite(&out, n, "hilbert").map_err(TestCaseError::fail)?;
        // Each axis is bounded by extent (origin) or half-extent (centered).
        let bound_xy = if s.center { s.extent * 0.5 } else { s.extent } + 1e-3;
        let bound_z = if s.flatten { 1e-3 } else { bound_xy };
        for (i, chunk) in out.chunks(3).enumerate() {
            prop_assert!(
                chunk[0].abs() <= bound_xy
                    && chunk[1].abs() <= bound_xy
                    && chunk[2].abs() <= bound_z,
                "hilbert: pos[{}] {:?} outside extent={} center={} flatten={}",
                i, chunk, s.extent, s.center, s.flatten
            );
        }
    }

    #[test]
    fn fuzz_concentric(s in concentric_settings(), n in small_n()) {
        let g = path_graph(n);
        let out = assert_deterministic(&s, &g, ConcentricLayout::solve, "concentric").map_err(TestCaseError::fail)?;
        assert_finite(&out, n, "concentric").map_err(TestCaseError::fail)?;
        // All points sit on the xy plane (z = 0) per the impl.
        for (i, chunk) in out.chunks(3).enumerate() {
            prop_assert!(chunk[2].abs() < 1e-4, "concentric: pos[{}].z = {} ≠ 0", i, chunk[2]);
        }
        // Radii are non-negative.
        let r_xy = max_radius_xy(&out);
        prop_assert!(r_xy >= 0.0);
    }

    // fcose / cose_bilkent now seed from a fixed `seed` (StdRng), so they are
    // deterministic: assert full determinism + finiteness. Small N keeps the
    // O(n²) force loops fast.
    #[test]
    fn fuzz_fcose(s in fcose_settings(), n in tiny_n()) {
        let g = path_graph(n);
        let out = assert_deterministic(&s, &g, FcoseLayout::solve, "fcose").map_err(TestCaseError::fail)?;
        assert_finite(&out, n, "fcose").map_err(TestCaseError::fail)?;
    }

    #[test]
    fn fuzz_cose_bilkent(s in cose_bilkent_settings(), n in tiny_n()) {
        let g = path_graph(n);
        let out = assert_deterministic(&s, &g, CoseBilkentLayout::solve, "cose_bilkent").map_err(TestCaseError::fail)?;
        assert_finite(&out, n, "cose_bilkent").map_err(TestCaseError::fail)?;
    }

    // cise / dagre / klay are deterministic: full determinism + finiteness,
    // and the 2D solvers keep z = 0.
    #[test]
    fn fuzz_cise(s in cise_settings(), n in small_n()) {
        let g = path_graph(n);
        let out = assert_deterministic(&s, &g, CiseLayout::solve, "cise").map_err(TestCaseError::fail)?;
        assert_finite(&out, n, "cise").map_err(TestCaseError::fail)?;
        for (i, chunk) in out.chunks(3).enumerate() {
            prop_assert!(chunk[2].abs() < 1e-4, "cise: pos[{}].z = {} ≠ 0", i, chunk[2]);
        }
    }

    #[test]
    fn fuzz_dagre(s in dagre_settings(), n in tiny_n()) {
        let g = path_graph(n);
        let out = assert_deterministic(&s, &g, DagreLayout::solve, "dagre").map_err(TestCaseError::fail)?;
        assert_finite(&out, n, "dagre").map_err(TestCaseError::fail)?;
        for (i, chunk) in out.chunks(3).enumerate() {
            prop_assert!(chunk[2].abs() < 1e-4, "dagre: pos[{}].z = {} ≠ 0", i, chunk[2]);
        }
    }

    #[test]
    fn fuzz_klay(s in klay_settings(), n in tiny_n()) {
        let g = path_graph(n);
        let out = assert_deterministic(&s, &g, KlayLayout::solve, "klay").map_err(TestCaseError::fail)?;
        assert_finite(&out, n, "klay").map_err(TestCaseError::fail)?;
        for (i, chunk) in out.chunks(3).enumerate() {
            prop_assert!(chunk[2].abs() < 1e-4, "klay: pos[{}].z = {} ≠ 0", i, chunk[2]);
        }
    }
}
