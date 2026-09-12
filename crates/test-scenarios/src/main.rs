//! YAML-driven deterministic scenario runner for the molecular force
//! layout.
//!
//! One scenario file is configuration-as-code for a full test bed: the
//! importer package + input, the exact `GpuForceOptions` the sim runs
//! under, and the acceptance gates — precision (identical reruns must be
//! bit-identical), accuracy (bond lengths vs UFF targets, ring angles,
//! planarity), and stochastic volume gates (N seeded runs, distribution
//! bounds). See `scenarios/caffeine-uff.yaml` for the annotated schema.
//!
//! Usage: `cargo run -p test-scenarios -- [scenario.yaml …]`
//! (default: every YAML under `crates/test-scenarios/scenarios/`).
//! Exit code 1 when any gate fails.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use graph_layouts::{GpuForceLayout, GpuForceOptions, Graph, MetadataValue};
use serde::Deserialize;
use vault_data::VaultGraph;

// --- scenario schema -----------------------------------------------------------

#[derive(Debug, Deserialize)]
struct Scenario {
    name: String,
    source: Source,
    sim: Sim,
    #[serde(default)]
    precision: Option<Precision>,
    #[serde(default)]
    accuracy: Option<Accuracy>,
    #[serde(default)]
    stochastic: Option<Stochastic>,
    #[serde(default)]
    robustness: Option<Robustness>,
}

#[derive(Debug, Deserialize)]
struct Source {
    /// Importer package TOML (repo-relative).
    package: PathBuf,
    /// Input file parsed by the package (repo-relative).
    input: PathBuf,
}

#[derive(Debug, Deserialize)]
struct Sim {
    /// Any subset of `GpuForceOptions` (per-field serde defaults fill the
    /// rest — same contract as the app's layout settings JSON).
    #[serde(default)]
    options: serde_yaml::Value,
    /// Total sim steps (rounded up to `steps_per_call`).
    steps: u32,
}

#[derive(Debug, Deserialize)]
struct Precision {
    /// Identical reruns of the deterministic config; final positions must
    /// be bit-identical across all of them.
    repeat_runs: u32,
}

#[derive(Debug, Deserialize)]
struct Accuracy {
    /// Per-bond |d − target| / target against the UFF rest length.
    bond_rel_tol: f32,
    /// Fraction of typed bonds that must satisfy `bond_rel_tol`.
    min_bond_fraction: f32,
    /// No bond may collapse below this world-unit length.
    min_bond_len: f32,
    /// Ring/structure angle expectations (degrees at the middle atom).
    #[serde(default)]
    angles: Vec<AngleExpectation>,
    /// Authored-start runs begin planar (z = 0); the sim has no z-forces
    /// on a planar molecule, so |z| must stay within this bound.
    #[serde(default)]
    planarity_tol: Option<f32>,
    /// Ring closure checks: the interior-angle sum of a simple planar
    /// n-gon is (n−2)·180° regardless of how the bond-only force field
    /// flexes the ring, so a folded or self-intersecting ring fails this
    /// gate even when per-angle tolerances pass.
    #[serde(default)]
    ring_sums: Vec<RingSumExpectation>,
    /// Optional gate on the mean bond relative error (catches systematic
    /// rest-length regressions that per-bond fractions can hide).
    #[serde(default)]
    max_mean_bond_rel_err: Option<f32>,
    /// Optional gate on the worst single bond relative error.
    #[serde(default)]
    max_bond_rel_err: Option<f32>,
}

#[derive(Debug, Deserialize)]
struct AngleExpectation {
    name: String,
    /// Node id refs; resolved by exact id or `:<suffix>` match (SDF atom
    /// indices ride as the package-namespaced id suffix).
    atoms: [String; 3],
    deg: f32,
    tol: f32,
}

#[derive(Debug, Deserialize)]
struct RingSumExpectation {
    name: String,
    /// Ordered ring atom refs (cyclic).
    atoms: Vec<String>,
    /// Expected interior-angle sum, degrees (planar: (n−2)·180).
    deg: f32,
    tol: f32,
}

#[derive(Debug, Deserialize)]
struct Stochastic {
    /// Seeded runs (seed = seed0 + k) that must each recover the accuracy
    /// gates from a perturbed start.
    runs: u32,
    seed0: u64,
    steps: u32,
    /// Gaussian jitter σ (world units) added to every authored coordinate
    /// (x, y, and a fresh z). 0 = full random-ball noise.
    #[serde(default)]
    jitter_sigma: f32,
    gates: StochasticGates,
}

#[derive(Debug, Deserialize)]
struct StochasticGates {
    /// Fraction of runs that must pass the accuracy gates.
    min_pass_fraction: f32,
    /// Per-run bond-recovery bar (fraction within accuracy.bond_rel_tol).
    /// A distribution gate, not the deterministic 100%.
    #[serde(default)]
    min_run_bond_fraction: Option<f32>,
    /// Per-run angle tolerance override (degrees). Jitter recovery leaves
    /// 3D pucker, so the deterministic band is too tight.
    #[serde(default)]
    angle_tol: Option<f32>,
    /// Per-run ring-sum tolerance override (degrees) — 3D pucker lowers
    /// the sum legitimately; folds lower it far more.
    #[serde(default)]
    ring_sum_tol: Option<f32>,
    /// p95 over runs of the per-run max bond relative error.
    p95_bond_rel_err: f32,
}

/// Full-noise robustness probe: random-ball starts that the angle-free
/// force field cannot fold into the molecule (bond-spring entanglement is
/// a genuine local minimum). Gate is "stays finite and bounded", never
/// accuracy — catching NaN/shotgun integrator regressions without
/// demanding physics the engine does not have.
#[derive(Debug, Deserialize)]
struct Robustness {
    runs: u32,
    seed0: u64,
    steps: u32,
    /// Final position magnitude must stay within this bound.
    max_position_norm: f32,
}

// --- graph construction (mirrors app/ui/src/graph_canvas.rs::graph_data_from_vault) ---

/// One runnable molecular graph: the layout-side `Graph` with UFF rest /
/// repulsion metadata attached, plus the evaluation data (bond targets,
/// node elements, authored positions).
struct MolecularCase {
    graph: Graph,
    /// (source id, target id, UFF rest target) for every typed bond.
    bond_targets: Vec<(String, String, f32)>,
    /// Authored 2D positions (for reference in reports).
    n_typed_nodes: usize,
}
/// Where a run's initial positions come from.
enum StartMode {
    /// Authored 2D depiction (z = 0) — the deterministic config.
    Authored,
    /// Authored + seeded Gaussian jitter on every coordinate (x, y, fresh z).
    Jitter { seed: u64, sigma: f32 },
    /// Seeded random 3-D ball — the robustness probe.
    Noise { seed: u64 },
}

/// Deterministic xorshift stream keyed by (seed, node slot). The sim's own
/// SeedMode::Random is a constant-seed LCG, so seed variation must arrive
/// via position3 overrides.
struct Xorshift(u64);

impl Xorshift {
    fn new(seed: u64, slot: usize) -> Self {
        Self(
            seed.wrapping_mul(0x9E37_79B9_7F4A_7C15)
                .wrapping_add(slot as u64 + 1),
        )
    }
    /// Uniform in [-1, 1).
    fn unit(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % 10_000) as f32 / 10_000.0 * 2.0 - 1.0
    }
    /// Approximate Gaussian (sum of 4 uniforms, σ ≈ 1).
    fn gauss(&mut self) -> f32 {
        (self.unit() + self.unit() + self.unit() + self.unit()) * 0.5
    }
}

fn build_case(vault: &VaultGraph, start: &StartMode) -> MolecularCase {
    let mut case_graph = Graph::new();
    let mut typed_nodes = 0usize;
    for (i, (id, node)) in vault.nodes.iter().enumerate() {
        let mut n = graph_layouts::Node::new(id.clone());
        n.position3 = Some(match start {
            StartMode::Authored => [node.x, node.y, 0.0],
            StartMode::Jitter { seed, sigma } => {
                let mut rng = Xorshift::new(*seed, i);
                [
                    node.x + rng.gauss() * sigma,
                    node.y + rng.gauss() * sigma,
                    rng.gauss() * sigma,
                ]
            }
            StartMode::Noise { seed } => {
                let mut rng = Xorshift::new(*seed, i);
                let radius = (vault.nodes.len() as f32).sqrt() * 2.0;
                [rng.unit() * radius, rng.unit() * radius, rng.unit() * radius]
            }
        });
        if let Some(w) = node
            .meta
            .doctype
            .as_deref()
            .and_then(graph_layouts::uff::repulsion_weight)
        {
            typed_nodes += 1;
            n.metadata
                .insert("repulsion".to_string(), MetadataValue::Number(w as f64));
        }
        case_graph.add_node(n);
    }

    let mut bond_targets = Vec::new();
    for edge in &vault.edges {
        let mut e = graph_layouts::Edge::new(
            format!("{}->{}", edge.source, edge.target),
            edge.source.clone(),
            edge.target.clone(),
        );
        let rest = edge
            .kind
            .as_deref()
            .and_then(graph_layouts::uff::bond_order)
            .and_then(|order| {
                let a = vault.nodes.get(&edge.source)?.meta.doctype.as_deref()?;
                let b = vault.nodes.get(&edge.target)?.meta.doctype.as_deref()?;
                graph_layouts::uff::bond_rest_length(a, b, order)
            });
        if let Some(rest) = rest {
            e.metadata
                .insert("rest".to_string(), MetadataValue::Number(rest as f64));
            bond_targets.push((edge.source.clone(), edge.target.clone(), rest));
        }
        case_graph.add_edge(e);
    }
    MolecularCase {
        graph: case_graph,
        bond_targets,
        n_typed_nodes: typed_nodes,
    }
}

// --- sim driver ------------------------------------------------------------------

/// Run `steps` total sim steps (rounded up to `steps_per_call`) and return
/// the final positions keyed by node id. Every call runs on a fresh
/// `GpuForceLayout`, so two calls with the same graph are independent runs.
async fn run_sim(graph: &Graph, options: &GpuForceOptions, steps: u32) -> Result<BTreeMap<String, [f32; 3]>, String> {
    let mut g = graph.clone();
    let mut layout = GpuForceLayout::new(options.clone());
    let per = options.steps_per_call.max(1);
    let calls = steps.div_ceil(per);
    let trace = std::env::var_os("JC_TRACE").is_some();
    for call in 0..calls {
        layout.run(&mut g).await?;
        if trace && call % 10 == 0 {
            let mut lens = Vec::new();
            for e in g.edges.values() {
                let (Some(a), Some(b)) = (g.nodes.get(&e.source), g.nodes.get(&e.target)) else {
                    continue;
                };
                let (Some(pa), Some(pb)) = (a.position3, b.position3) else {
                    continue;
                };
                lens.push(((pa[0] - pb[0]).powi(2) + (pa[1] - pb[1]).powi(2) + (pa[2] - pb[2]).powi(2)).sqrt());
            }
            let mean = lens.iter().sum::<f32>() / lens.len().max(1) as f32;
            let max = lens.iter().cloned().fold(0.0f32, f32::max);
            eprintln!("trace call {call}: mean bond {mean:.3} max {max:.3}");
        }
    }
    let mut out = BTreeMap::new();
    for (id, node) in &g.nodes {
        out.insert(id.clone(), node.position3.unwrap_or([0.0; 3]));
    }
    Ok(out)
}

// --- measurement -------------------------------------------------------------------

#[derive(Debug, Default)]
struct BondStats {
    n: usize,
    within_tol: usize,
    max_rel_err: f32,
    mean_rel_err: f32,
    min_len: f32,
    all_finite: bool,
}

fn bond_stats(positions: &BTreeMap<String, [f32; 3]>, targets: &[(String, String, f32)], tol: f32) -> BondStats {
    let mut s = BondStats {
        min_len: f32::INFINITY,
        all_finite: true,
        ..Default::default()
    };
    let mut sum = 0.0f32;
    for (a, b, target) in targets {
        let (Some(pa), Some(pb)) = (positions.get(a), positions.get(b)) else {
            continue;
        };
        let d = ((pa[0] - pb[0]).powi(2) + (pa[1] - pb[1]).powi(2) + (pa[2] - pb[2]).powi(2)).sqrt();
        if !d.is_finite() {
            s.all_finite = false;
            continue;
        }
        let rel = (d - target).abs() / target;
        s.n += 1;
        if rel <= tol {
            s.within_tol += 1;
        }
        s.max_rel_err = s.max_rel_err.max(rel);
        sum += rel;
        s.min_len = s.min_len.min(d);
    }
    if s.n > 0 {
        s.mean_rel_err = sum / s.n as f32;
    }
    s
}

fn angle_deg(pa: [f32; 3], pb: [f32; 3], pc: [f32; 3]) -> f32 {
    let u = [pa[0] - pb[0], pa[1] - pb[1], pa[2] - pb[2]];
    let v = [pc[0] - pb[0], pc[1] - pb[1], pc[2] - pb[2]];
    let dot = u[0] * v[0] + u[1] * v[1] + u[2] * v[2];
    let nu = (u[0].powi(2) + u[1].powi(2) + u[2].powi(2)).sqrt();
    let nv = (v[0].powi(2) + v[1].powi(2) + v[2].powi(2)).sqrt();
    if nu < 1e-9 || nv < 1e-9 {
        return f32::NAN;
    }
    (dot / (nu * nv)).clamp(-1.0, 1.0).acos().to_degrees()
}

/// Resolve an angle atom ref: exact id, or unique `…:<ref>` suffix match.
fn resolve_atom<'a>(positions: &'a BTreeMap<String, [f32; 3]>, atom_ref: &str) -> Option<&'a [f32; 3]> {
    if let Some(p) = positions.get(atom_ref) {
        return Some(p);
    }
    let suffix = format!(":{atom_ref}");
    let mut hits = positions.keys().filter(|k| k.ends_with(&suffix));
    match (hits.next(), hits.next()) {
        (Some(k), None) => positions.get(k),
        _ => None,
    }
}

struct GateReport {
    lines: Vec<String>,
    failures: Vec<String>,
}

impl GateReport {
    fn new() -> Self {
        Self { lines: Vec::new(), failures: Vec::new() }
    }
    fn info(&mut self, line: String) {
        self.lines.push(line);
    }
    fn check(&mut self, ok: bool, label: String) {
        if ok {
            self.lines.push(format!("  ok   {label}"));
        } else {
            self.lines.push(format!("  FAIL {label}"));
            self.failures.push(label);
        }
    }
}

/// Interior-angle sum of a ring (cyclic atom refs). `None` when any ref
/// is unresolved.
fn ring_sum_deg(positions: &BTreeMap<String, [f32; 3]>, ring: &RingSumExpectation) -> Option<f32> {
    let pts: Vec<[f32; 3]> = ring
        .atoms
        .iter()
        .map(|a| resolve_atom(positions, a).copied())
        .collect::<Option<_>>()?;
    let n = pts.len();
    let mut sum = 0.0;
    for i in 0..n {
        sum += angle_deg(pts[(i + n - 1) % n], pts[i], pts[(i + 1) % n]);
    }
    Some(sum)
}

// --- scenario execution ------------------------------------------------------------

fn load_vault(source: &Source) -> Result<VaultGraph, String> {
    let toml = std::fs::read_to_string(&source.package)
        .map_err(|e| format!("read {}: {e}", source.package.display()))?;
    let package = importer::ValidatedPackage::from_toml(&toml)
        .map_err(|e| format!("parse package {}: {e}", source.package.display()))?;
    let input = std::fs::read_to_string(&source.input)
        .map_err(|e| format!("read {}: {e}", source.input.display()))?;
    let result = package
        .parse_input(&input)
        .map_err(|e| format!("parse {}: {e}", source.input.display()))?;
    Ok(result.graph)
}

fn sim_options(sim: &Sim) -> Result<GpuForceOptions, String> {
    let json = serde_json::to_value(&sim.options).map_err(|e| format!("options map: {e}"))?;
    serde_json::from_value(json).map_err(|e| format!("decode sim.options: {e}"))
}

async fn run_scenario(sc: &Scenario) -> GateReport {
    let mut report = GateReport::new();
    let vault = match load_vault(&sc.source) {
        Ok(v) => v,
        Err(e) => {
            report.check(false, format!("load source: {e}"));
            return report;
        }
    };
    report.info(format!(
        "graph: {} nodes / {} edges",
        vault.nodes.len(),
        vault.edges.len()
    ));
    let options = match sim_options(&sc.sim) {
        Ok(o) => o,
        Err(e) => {
            report.check(false, e);
            return report;
        }
    };

    let case = build_case(&vault, &StartMode::Authored);
    report.info(format!(
        "typed: {} atoms with UFF weights, {} bonds with UFF targets",
        case.n_typed_nodes,
        case.bond_targets.len()
    ));

    // -- precision: identical reruns must be bit-identical ---------------------
    let mut reference: Option<BTreeMap<String, [f32; 3]>> = None;
    let repeats = sc.precision.as_ref().map(|p| p.repeat_runs).unwrap_or(1).max(1);
    for run in 0..repeats {
        match run_sim(&case.graph, &options, sc.sim.steps).await {
            Ok(positions) => {
                if let Some(reference) = &reference {
                    let mut max_drift = 0.0f32;
                    let mut bit_identical = true;
                    for (id, p) in &positions {
                        let r = reference[id];
                        for k in 0..3 {
                            if p[k].to_bits() != r[k].to_bits() {
                                bit_identical = false;
                                max_drift = max_drift.max((p[k] - r[k]).abs());
                            }
                        }
                    }
                    report.check(
                        bit_identical,
                        format!("precision: run {run} bit-identical to run 0 (max drift {max_drift:e})"),
                    );
                } else {
                    reference = Some(positions);
                }
            }
            Err(e) => report.check(false, format!("precision run {run}: {e}")),
        }
    }

    // -- accuracy: bond lengths, angles, planarity, no collapse ---------------
    if let (Some(acc), Some(positions)) = (&sc.accuracy, &reference) {
        let stats = bond_stats(positions, &case.bond_targets, acc.bond_rel_tol);
        report.check(stats.all_finite, "accuracy: all bond lengths finite".to_string());
        let fraction = stats.within_tol as f32 / stats.n.max(1) as f32;
        report.check(
            fraction >= acc.min_bond_fraction,
            format!(
                "accuracy: {}/{} bonds within {:.0}% of UFF target (need ≥ {:.0}%; mean err {:.1}%, max {:.1}%)",
                stats.within_tol,
                stats.n,
                acc.bond_rel_tol * 100.0,
                acc.min_bond_fraction * 100.0,
                stats.mean_rel_err * 100.0,
                stats.max_rel_err * 100.0,
            ),
        );
        if let Some(cap) = acc.max_mean_bond_rel_err {
            report.check(
                stats.mean_rel_err <= cap,
                format!(
                    "accuracy: mean bond err {:.1}% ≤ {:.1}%",
                    stats.mean_rel_err * 100.0,
                    cap * 100.0
                ),
            );
        }
        if let Some(cap) = acc.max_bond_rel_err {
            report.check(
                stats.max_rel_err <= cap,
                format!(
                    "accuracy: worst bond err {:.1}% ≤ {:.1}%",
                    stats.max_rel_err * 100.0,
                    cap * 100.0
                ),
            );
        }
        report.check(
            stats.min_len >= acc.min_bond_len,
            format!(
                "accuracy: shortest bond {:.3} ≥ {:.3} (no collapse)",
                stats.min_len, acc.min_bond_len
            ),
        );
        for angle in &acc.angles {
            let resolved = [
                resolve_atom(positions, &angle.atoms[0]),
                resolve_atom(positions, &angle.atoms[1]),
                resolve_atom(positions, &angle.atoms[2]),
            ];
            let label = format!("angle {} ({:?})", angle.name, angle.atoms);
            match resolved {
                [Some(a), Some(b), Some(c)] => {
                    let deg = angle_deg(*a, *b, *c);
                    let dev = (deg - angle.deg).abs();
                    report.check(
                        dev <= angle.tol,
                        format!("{label}: {deg:.1}° vs {}° ±{}°", angle.deg, angle.tol),
                    );
                }
                _ => report.check(false, format!("{label}: atom ref unresolved")),
            }
        }
        for ring in &acc.ring_sums {
            let label = format!("ring-sum {} ({} atoms)", ring.name, ring.atoms.len());
            match ring_sum_deg(positions, ring) {
                Some(sum) => {
                    let dev = (sum - ring.deg).abs();
                    report.check(
                        dev <= ring.tol,
                        format!("{label}: {sum:.1}° vs {}° ±{}°", ring.deg, ring.tol),
                    );
                }
                None => report.check(false, format!("{label}: atom ref unresolved")),
            }
        }
        if let Some(tol) = acc.planarity_tol {
            let max_z = positions
                .values()
                .map(|p| p[2].abs())
                .fold(0.0f32, f32::max);
            report.check(
                max_z <= tol,
                format!("planarity: max |z| {max_z:.4} ≤ {tol}"),
            );
        }
    }
    // -- stochastic: seeded jitter runs, distribution gates --------------------
    if let Some(sto) = &sc.stochastic {
        let acc = sc.accuracy.as_ref();
        let mut passes = 0u32;
        let mut max_errs: Vec<f32> = Vec::new();
        for k in 0..sto.runs {
            let seed = sto.seed0.wrapping_add(k as u64);
            let jittered = build_case(&vault, &StartMode::Jitter { seed, sigma: sto.jitter_sigma });
            match run_sim(&jittered.graph, &options, sto.steps).await {
                Ok(positions) => {
                    let stats = bond_stats(
                        &positions,
                        &jittered.bond_targets,
                        acc.map(|a| a.bond_rel_tol).unwrap_or(0.5),
                    );
                    max_errs.push(stats.max_rel_err);
                    let bond_fraction = stats.within_tol as f32 / stats.n.max(1) as f32;
                    let (mut worst_angle_dev, mut worst_ring_dev) = (0.0f32, 0.0f32);
                    let angle_tol = sto.gates.angle_tol;
                    let ring_tol = sto.gates.ring_sum_tol;
                    let structure_ok = acc
                        .map(|a| {
                            let angles_ok = a.angles.iter().all(|angle| {
                                match [
                                    resolve_atom(&positions, &angle.atoms[0]),
                                    resolve_atom(&positions, &angle.atoms[1]),
                                    resolve_atom(&positions, &angle.atoms[2]),
                                ] {
                                    [Some(x), Some(y), Some(z)] => {
                                        let dev = (angle_deg(*x, *y, *z) - angle.deg).abs();
                                        worst_angle_dev = worst_angle_dev.max(dev);
                                        dev <= angle_tol.unwrap_or(angle.tol)
                                    }
                                    _ => false,
                                }
                            });
                            let rings_ok = a.ring_sums.iter().all(|ring| {
                                match ring_sum_deg(&positions, ring) {
                                    Some(sum) => {
                                        let dev = (sum - ring.deg).abs();
                                        worst_ring_dev = worst_ring_dev.max(dev);
                                        dev <= ring_tol.unwrap_or(ring.tol)
                                    }
                                    None => false,
                                }
                            });
                            angles_ok && rings_ok
                        })
                        .unwrap_or(true);
                    let run_bond_bar = sto
                        .gates
                        .min_run_bond_fraction
                        .or_else(|| acc.map(|a| a.min_bond_fraction))
                        .unwrap_or(0.0);
                    let passed = stats.all_finite
                        && bond_fraction >= run_bond_bar
                        && stats.min_len >= acc.map(|a| a.min_bond_len).unwrap_or(0.0)
                        && structure_ok;
                    if passed {
                        passes += 1;
                    }
                    report.info(format!(
                        "  stochastic run {k} (seed {seed}): bonds {:.0}% in tol, max err {:.1}%, angle dev {:.0}°, ring dev {:.0}°{}",
                        bond_fraction * 100.0,
                        stats.max_rel_err * 100.0,
                        worst_angle_dev,
                        worst_ring_dev,
                        if passed { "" } else { "  (fail)" },
                    ));
                }
                Err(e) => {
                    report.info(format!("  stochastic run {k} (seed {seed}): sim error {e}"));
                }
            }
        }
        let pass_fraction = passes as f32 / sto.runs.max(1) as f32;
        report.check(
            pass_fraction >= sto.gates.min_pass_fraction,
            format!(
                "stochastic: {passes}/{} runs pass accuracy gates (need ≥ {:.0}%)",
                sto.runs,
                sto.gates.min_pass_fraction * 100.0
            ),
        );
        if !max_errs.is_empty() {
            max_errs.sort_by(|a, b| a.total_cmp(b));
            let p95 = max_errs[((max_errs.len() as f32 * 0.95).ceil() as usize - 1).min(max_errs.len() - 1)];
            report.check(
                p95 <= sto.gates.p95_bond_rel_err,
                format!(
                    "stochastic: p95 max bond rel err {:.1}% ≤ {:.1}%",
                    p95 * 100.0,
                    sto.gates.p95_bond_rel_err * 100.0
                ),
            );
        }
    }

    // -- robustness: full-noise starts must stay finite and bounded --------------
    if let Some(rob) = &sc.robustness {
        for k in 0..rob.runs {
            let seed = rob.seed0.wrapping_add(k as u64);
            let noise = build_case(&vault, &StartMode::Noise { seed });
            match run_sim(&noise.graph, &options, rob.steps).await {
                Ok(positions) => {
                    let max_norm = positions
                        .values()
                        .map(|p| (p[0].powi(2) + p[1].powi(2) + p[2].powi(2)).sqrt())
                        .fold(0.0f32, f32::max);
                    let finite = positions
                        .values()
                        .all(|p| p.iter().all(|c| c.is_finite()));
                    report.check(
                        finite && max_norm <= rob.max_position_norm,
                        format!(
                            "robustness run {k} (seed {seed}): finite={finite} max |pos| {max_norm:.1} ≤ {}",
                            rob.max_position_norm
                        ),
                    );
                }
                Err(e) => report.check(false, format!("robustness run {k} (seed {seed}): sim error {e}")),
            }
        }
    }

    report
}

// --- entry point -----------------------------------------------------------------

fn scenario_paths(args: &[String]) -> Vec<PathBuf> {
    if !args.is_empty() {
        return args.iter().map(PathBuf::from).collect();
    }
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("scenarios");
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("yaml"))
                .collect()
        })
        .unwrap_or_default();
    paths.sort();
    paths
}

fn main() -> ExitCode {
    env_logger::init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let paths = scenario_paths(&args);
    if paths.is_empty() {
        eprintln!("no scenarios found");
        return ExitCode::from(2);
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let mut failures = 0usize;
    for path in paths {
        let raw = match std::fs::read_to_string(&path) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("scenario {}: read error: {e}", path.display());
                failures += 1;
                continue;
            }
        };
        let scenario: Scenario = match serde_yaml::from_str(&raw) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("scenario {}: YAML error: {e}", path.display());
                failures += 1;
                continue;
            }
        };
        println!("scenario {} ({})", scenario.name, path.display());
        let report = runtime.block_on(run_scenario(&scenario));
        for line in &report.lines {
            println!("{line}");
        }
        if report.failures.is_empty() {
            println!("PASS {}\n", scenario.name);
        } else {
            println!("FAIL {} ({} gate(s))\n", scenario.name, report.failures.len());
            failures += 1;
        }
    }
    if failures == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}
