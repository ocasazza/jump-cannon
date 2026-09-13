//! Layout regimes — named parameter bases as data, capability manifests as
//! truth (`docs/layout-ux-spec.md`, implementing `docs/layout-ux.md`).
//!
//! Phase 1 scope (spec §8): the registry loader (serde, `deny_unknown_fields`,
//! options validated against `GpuForceOptions`), the `typed_bond_coverage`
//! predicate, a hand-checked gpu-force capability manifest verified against
//! the options struct by unit test, and the auto-apply/restore seam that
//! makes a UFF-typed graph boot into `molecular-uff` with no `?config=`.
//!
//! Engine-truth anchors that bind this module (spec §7):
//! - E1: typed rests win outright; `spring_len` is the untyped/invalid
//!   fallback (`gpu_force.rs:1350-1351`). No control here may claim to scale
//!   typed rests — a data-owned dimension renders as a capsule, never a knob.
//! - E2: repulsion mixes with per-atom UFF weights via sqrt(wi*wj), so a
//!   repulsion control stays honest on typed atoms (phase-3 intent).
//! - M4: coverage = `typed_edges / edge_count` from `TYPED_FORCE_SUMMARY`
//!   (`(typed_nodes, typed_edges)`); there is no `bonds_typed` field.

use std::collections::BTreeMap;

use dioxus::prelude::*;
use gloo_storage::{LocalStorage, Storage};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::graph_canvas::GraphData;

// The registry ships in the WASM bundle: regimes are deployable data, not
const REGIME_FILES: [(&str, &str); 2] = [
    ("molecular-uff.yaml", include_str!("../../../configs/regimes/molecular-uff.yaml")),
    ("vault-small.yaml", include_str!("../../../configs/regimes/vault-small.yaml")),
];

// --- schema (spec §2) -----------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Execution {
    Live,
    OneShot,
}

/// Closed predicate vocabulary (spec §2 `applicability`), ANDed at
/// evaluation. Phase 1 evaluates `typed_bond_coverage`, `min_nodes`, and
/// `max_nodes`; the remaining fields parse (so a regime declaring them is a
/// schema-valid authoring) but fail closed at resolution with a warning —
/// wiring them is phase-2+ work and a silent match would lie.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct Applicability {
    #[serde(default)]
    pub typed_bond_coverage: Option<CoverageGte>,
    #[serde(default)]
    pub min_nodes: Option<u64>,
    #[serde(default)]
    pub max_nodes: Option<u64>,
    #[serde(default)]
    pub has_authored_positions: Option<bool>,
    #[serde(default)]
    pub source_kind: Option<Vec<String>>,
    #[serde(default)]
    pub engine_kind: Option<Vec<String>>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CoverageGte {
    pub gte: f64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Regime {
    pub id: String,
    pub schema_version: u32,
    pub label: String,
    #[serde(default)]
    pub description: Option<String>,
    pub engine: String,
    pub execution: Execution,
    #[serde(default)]
    pub applicability: Applicability,
    /// Partial `GpuForceOptions` projection: keys MUST be option field names
    /// (validated against the manifest), values deserialize against the
    /// engine struct; `null` marks a data-owned dimension filled at resolve
    /// time from typed graph data (R2: a null with no data source fails
    /// loud, naming the regime).
    #[serde(default)]
    pub options: Option<serde_yml::Mapping>,
    /// Remote-engine regime shape (phase 4); XOR with `options`.
    #[serde(default)]
    pub settings_schema: Option<String>,
    #[serde(default)]
    pub controls: Vec<ControlDecl>,
    #[serde(default)]
    pub presets_hidden: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ControlDecl {
    pub id: String,
    pub kind: ControlKind,
    pub label: String,
    #[serde(default)]
    pub range: Option<(f64, f64)>,
    #[serde(default)]
    pub options: Option<Vec<String>>,
    #[serde(default)]
    pub maps_to: Option<BTreeMap<String, Value>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ControlKind {
    Multiplier,
    Toggle,
    Enum,
    Absolute,
}

impl Applicability {
    /// Count of declared predicates — the R3 specificity sort key and the
    /// "auto:" humanizer's input.
    fn predicate_count(&self) -> usize {
        [
            self.typed_bond_coverage.is_some(),
            self.min_nodes.is_some(),
            self.max_nodes.is_some(),
            self.has_authored_positions.is_some(),
            self.source_kind.is_some(),
            self.engine_kind.is_some(),
        ]
        .into_iter()
        .filter(|d| *d)
        .count()
    }

    fn is_empty(&self) -> bool {
        self.predicate_count() == 0
    }

    /// First matching predicate, humanized (spec §4's capsule reason line).
    /// Only predicates that actually decided the match are named.
    fn humanized_reason(&self, state: &SnapshotState) -> String {
        if let Some(cov) = &self.typed_bond_coverage {
            return format!(
                "{} bonds UFF-typed (coverage {:.2} ≥ {:.2})",
                state.typed_edges, state.typed_bond_coverage, cov.gte
            );
        }
        if let Some(min) = self.min_nodes {
            return format!("{} ≥ {} nodes", state.n_nodes, min);
        }
        if let Some(max) = self.max_nodes {
            return format!("{} ≤ {} nodes", state.n_nodes, max);
        }
        "catch-all".to_string()
    }

    /// Fail closed on predicates phase 1 cannot evaluate yet: a regime that
    /// declares them never matches (and says why in the log) instead of
    /// matching on a guess.
    fn unevaluated(&self) -> Option<&'static str> {
        if self.has_authored_positions.is_some() {
            Some("has_authored_positions")
        } else if self.source_kind.is_some() {
            Some("source_kind")
        } else if self.engine_kind.is_some() {
            Some("engine_kind")
        } else {
            None
        }
    }

    fn matches(&self, state: &SnapshotState) -> bool {
        if let Some(field) = self.unevaluated() {
            tracing::warn!(
                "[regimes] predicate {field} declared but not evaluated in phase 1 — \
                 regime fails closed (spec §2 R3)"
            );
            return false;
        }
        if let Some(cov) = &self.typed_bond_coverage {
            if state.typed_bond_coverage < cov.gte {
                return false;
            }
        }
        if let Some(min) = self.min_nodes {
            if state.n_nodes < min {
                return false;
            }
        }
        if let Some(max) = self.max_nodes {
            if state.n_nodes > max {
                return false;
            }
        }
        true
    }
}

// --- registry (spec §2 rules R1–R5) ----------------------------------------------

/// The registry, loaded once (R3 order). Validation panics here are the
/// "loud boot error naming the file and field" contract; the unit tests
/// keep that from ever shipping.
static REGISTRY: std::sync::LazyLock<Vec<Regime>> =
    std::sync::LazyLock::new(|| load_registry(&REGIME_FILES));

pub(crate) fn registry() -> &'static [Regime] {
    &REGISTRY
}

/// Parse, validate, and order the registry. Load order per R3: predicate
/// count descending (most specific first), then filename ascending. A
/// malformed file is a boot error naming the file and field — these are
/// compile-time-curated data, so the unit tests make any drift a CI failure
/// before it can reach a boot.
pub(crate) fn load_registry(files: &[(&str, &str)]) -> Vec<Regime> {
    let mut regimes: Vec<(String, Regime)> = files
        .iter()
        .map(|(name, yaml)| {
            let regime: Regime = serde_yml::from_str(yaml)
                .unwrap_or_else(|e| panic!("[regimes] {name}: invalid regime YAML: {e}"));
            validate_regime(name, &regime).unwrap_or_else(|e| panic!("[regimes] {name}: {e}"));
            (name.to_string(), regime)
        })
        .collect();
    regimes.sort_by(|(an, a), (bn, b)| {
        b.applicability
            .predicate_count()
            .cmp(&a.applicability.predicate_count())
            .then_with(|| an.cmp(bn))
    });
    regimes.into_iter().map(|(_, r)| r).collect()
}

fn validate_regime(_name: &str, r: &Regime) -> Result<(), String> {
    let id_ok = !r.id.is_empty()
        && r.id
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && r.id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if !id_ok {
        return Err(format!("id {:?} must match ^[a-z0-9][a-z0-9-]*$", r.id));
    }
    if r.schema_version != 1 {
        return Err(format!("schema_version must be 1, got {}", r.schema_version));
    }
    if r.settings_schema.is_some() == r.options.is_some() {
        return Err(
            "exactly one of options (local engines) / settings_schema (broker) is required"
                .to_string(),
        );
    }
    if r.execution == Execution::OneShot {
        // R1: one-shot regimes must not declare live-sim controls.
        const LIVE: [&str; 4] = ["cooling_alpha", "cooling_floor", "energy_threshold", "damping"];
        if let Some(map) = &r.options {
            for key in map.keys() {
                let key = key.as_str().unwrap_or_default();
                if LIVE.contains(&key) {
                    return Err(format!(
                        "execution: one_shot regime must not declare live-sim option {key:?} (R1)"
                    ));
                }
            }
        }
    }
    if let Some(map) = &r.options {
        for (key, value) in map {
            let key = key.as_str().unwrap_or_default();
            if !gpu_force_manifest().iter().any(|d| d.id == key) {
                return Err(format!(
                    "unknown options field {key:?} — not a GpuForceOptions field"
                ));
            }
            if !value.is_null() {
                // Non-null values must deserialize against the engine
                // struct: project the single field and round-trip.
                let mut probe = serde_json::Map::new();
                probe.insert(key.to_string(), yml_to_json(value)?);
                if serde_json::from_value::<graph_layouts::GpuForceOptions>(Value::Object(probe))
                    .is_err()
                {
                    return Err(format!("options field {key:?} does not fit GpuForceOptions"));
                }
            }
        }
    }
    Ok(())
}


// --- snapshot state + resolver (spec §4) ------------------------------------------

/// The five-field resolution state (spec §1). Phase 1 populates the fields
/// the coverage predicate and node bounds need; the rest ride along for
/// phase 2.
#[derive(Clone, Debug)]
pub(crate) struct SnapshotState {
    pub typed_bond_coverage: f64,
    pub typed_nodes: usize,
    pub typed_edges: usize,
    pub n_nodes: u64,
    pub n_edges: usize,
    #[allow(dead_code)]
    pub has_authored: bool,
    #[allow(dead_code)]
    pub source_kind: String,
    #[allow(dead_code)]
    pub engine_kind: &'static str,
}

/// First regime in registry order whose applicability matches (R3: no
/// expression language, first match wins). The registry's catch-all
/// guarantees a result.
pub(crate) fn resolve(state: &SnapshotState) -> &'static Regime {
    registry()
        .iter()
        .find(|r| r.engine == "gpu-force" && r.applicability.matches(state))
        .expect("registry contains a catch-all regime (R4)")
}

/// What the panel renders: the resolved regime plus the measurements that
/// decided the match (UI2/UI5, M2).
#[derive(Clone, Debug, Default)]
pub(crate) struct RegimeResolution {
    pub regime_id: String,
    pub label: String,
    pub reason: String,
    pub typed_nodes: usize,
    pub typed_edges: usize,
    pub n_nodes: usize,
    pub n_edges: usize,
    pub coverage: f64,
    /// The regime quarantines the preset row (fast/balanced/pretty) —
    /// vault-tuned values would clobber UFF geometry.
    pub presets_hidden: bool,
}

pub(crate) static RESOLUTION: GlobalSignal<Option<RegimeResolution>> = Signal::global(|| None);

/// Mean of the resolved (>0) typed UFF rest lengths — the honest anchor for
/// authored-position rescaling and the fill value for a data-owned
/// `spring_len`. ångström-scale by construction.
pub(crate) fn mean_typed_rest(rests: &[f32]) -> Option<f32> {
    let typed: Vec<f32> = rests.iter().copied().filter(|r| *r > 0.0).collect();
    if typed.is_empty() {
        None
    } else {
        Some(typed.iter().sum::<f32>() / typed.len() as f32)
    }
}

fn snapshot_state(graph: &GraphData) -> SnapshotState {
    let summary = *crate::graph_canvas::TYPED_FORCE_SUMMARY.peek();
    let (typed_nodes, typed_edges) = summary.unwrap_or((0, 0));
    let n_edges = graph.n_edges as usize;
    SnapshotState {
        typed_bond_coverage: typed_edges as f64 / n_edges.max(1) as f64,
        typed_nodes,
        typed_edges,
        n_nodes: graph.n_nodes as u64,
        n_edges,
        has_authored: false,
        source_kind: String::new(),
        engine_kind: "local",
    }
}

/// YAML → JSON for the option-value subset regimes may declare (scalars,
/// nulls, sequences of scalars). Structured YAML in `options` is an
/// authoring error, named loudly.
fn yml_to_json(v: &serde_yml::Value) -> Result<Value, String> {
    use serde_yml::Value as Y;
    Ok(match v {
        Y::Null => Value::Null,
        Y::Bool(b) => Value::Bool(*b),
        Y::Number(n) => {
            if let Some(i) = n.as_i64() {
                serde_json::json!(i)
            } else {
                serde_json::json!(n.as_f64().unwrap_or_default())
            }
        }
        Y::String(s) => Value::String(s.clone()),
        Y::Sequence(seq) => Value::Array(
            seq.iter().map(yml_to_json).collect::<Result<Vec<_>, _>>()?,
        ),
        other => {
            return Err(format!(
                "unsupported YAML value in options: {other:?} (scalars and lists only)"
            ))
        }
    })
}

/// Project a regime's options onto a settings JSON object, filling null
/// (data-owned) fields from typed graph data. R2: a null with no data
/// source is an error naming the regime.
fn effective_options(regime: &Regime, graph: &GraphData) -> Result<Value, String> {
    let mut fills = BTreeMap::new();
    if let Some(mean) = graph.scene.edge_rest.as_deref().and_then(mean_typed_rest) {
        fills.insert("spring_len".to_string(), serde_json::json!(mean));
    }
    let mut out = serde_json::Map::new();
    if let Some(map) = &regime.options {
        for (key, value) in map {
            let key = key
                .as_str()
                .ok_or_else(|| format!("regime {}: non-string options key", regime.id))?;
            let json = yml_to_json(value)?;
            let json = match json {
                Value::Null => fills.get(key).cloned().ok_or_else(|| {
                    format!(
                        "regime {id}: options field {key:?} is null (data-owned) but the \
                         loaded graph carries no typed value for it (R2)",
                        id = regime.id
                    )
                })?,
                filled => filled,
            };
            out.insert(key.to_string(), json);
        }
    }
    Ok(Value::Object(out))
}

// --- auto-apply seam ----------------------------------------------------------------
//
// The resolver applies a non-catch-all regime's options on transition into
// it and restores the pre-regime settings on the way out, stashing the
// displaced block in localStorage so a reload round-trips. A user's edits
// while the regime is active are never clobbered: same-regime re-resolution
// keeps the current settings (spec §4 "manual override … keep").

const REGIME_STORE_KEY: &str = "jc_layout_regime_v1";

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct RegimeRecord {
    /// Regime whose options were auto-applied; empty = none.
    #[serde(default)]
    regime_id: String,
    /// The gpu-force settings block displaced by the auto-apply.
    #[serde(default)]
    stash: Option<Value>,
}

fn load_regime_record() -> RegimeRecord {
    LocalStorage::get(REGIME_STORE_KEY).unwrap_or_default()
}

fn save_regime_record(r: &RegimeRecord) {
    let _ = LocalStorage::set(REGIME_STORE_KEY, r);
}

/// Resolve the regime for a freshly loaded graph, publish the resolution
/// the panel renders, and run the auto-apply/restore seam. Called by every
/// `GraphData` producer (server load, github vault, embedded world) —
/// `TYPED_FORCE_SUMMARY` is already written when this runs.
pub(crate) fn on_graph_loaded(graph: &GraphData) {
    let state = snapshot_state(graph);
    let regime = resolve(&state);
    *RESOLUTION.write() = Some(RegimeResolution {
        regime_id: regime.id.clone(),
        label: regime.label.clone(),
        reason: regime.applicability.humanized_reason(&state),
        typed_nodes: state.typed_nodes,
        typed_edges: state.typed_edges,
        n_nodes: state.n_nodes as usize,
        n_edges: state.n_edges,
        coverage: state.typed_bond_coverage,
        presets_hidden: !regime.presets_hidden.is_empty(),
    });

    let mut record = load_regime_record();
    if regime.applicability.is_empty() {
        // Catch-all resolved: leaving an auto-applied regime restores the
        // displaced settings (or drops the block so for_n_nodes re-tunes).
        if !record.regime_id.is_empty() {
            crate::panels::layout::restore_gpu_force_settings(record.stash.take());
            record.regime_id.clear();
            save_regime_record(&record);
        }
        return;
    }
    if record.regime_id == regime.id {
        return; // same regime: keep current (possibly user-tuned) settings
    }
    match effective_options(regime, graph) {
        Ok(options) => {
            let stash = crate::panels::layout::gpu_force_settings_snapshot();
            crate::panels::layout::install_gpu_force_regime_options(options);
            record.regime_id = regime.id.clone();
            record.stash = stash;
            save_regime_record(&record);
        }
        Err(error) => {
            // R2 loud boot error: keep the previous settings rather than
            // apply a regime the data cannot fill.
            tracing::error!("[regimes] not applying {id}: {error}", id = regime.id);
        }
    }
}

// --- gpu-force capability manifest (spec §3) ---------------------------------------
//
// Hand-checked projection of `GpuForceOptions` (phase 5 mechanizes this
// via schemars). One dimension per options field — the parity unit test
// fails the build when the two drift. `not_when` uses the same closed
// predicate vocabulary as regime YAML (M1); a fired `not_when` renders the
// dimension as a collapsed-data capsule on every surface (M2, CK-202).

#[allow(dead_code)] // label/control/owned_by/note surface in phase 3's
                    // manifest-filtered Advanced disclosure; ids gate today.

pub(crate) struct ManifestDim {
    pub id: &'static str,
    pub label: &'static str,
    /// "multiplier" | "toggle" | "enum" | "absolute" ("internal" for
    /// cursor/transient fields no regime or control should carry).
    pub control: &'static str,
    /// Fires when the data owns this dimension outright → capsule (M2).
    pub not_when: Option<&'static str>,
    /// Inverse applicability gate (phase 1: the backend enum's n≥500).
    pub min_nodes: Option<u64>,
    pub owned_by: Option<&'static str>,
    pub note: &'static str,
}

pub(crate) fn gpu_force_manifest() -> &'static [ManifestDim] {
    &[
        ManifestDim { id: "repulsion", label: "Repulsion", control: "absolute", not_when: None, min_nodes: None, owned_by: None,
            note: "mixes with per-atom UFF weights via sqrt(wi*wj) — honest on typed atoms (E2)" },
        ManifestDim { id: "spring_k", label: "Bond stiffness", control: "absolute", not_when: None, min_nodes: None, owned_by: None,
            note: "spring force stiffness" },
        ManifestDim { id: "spring_len", label: "Bond scale", control: "multiplier", not_when: Some("typed_bond_coverage_gte:1.0"), min_nodes: None, owned_by: Some("uff bond table"),
            note: "governs untyped edges only when coverage is partial (E1)" },
        ManifestDim { id: "gravity", label: "Gravity", control: "absolute", not_when: None, min_nodes: None, owned_by: None, note: "" },
        ManifestDim { id: "damping", label: "Damping", control: "absolute", not_when: None, min_nodes: None, owned_by: None, note: "" },
        ManifestDim { id: "dt", label: "Time step", control: "absolute", not_when: None, min_nodes: None, owned_by: None, note: "" },
        ManifestDim { id: "cursor_pos", label: "Cursor", control: "internal", not_when: None, min_nodes: None, owned_by: None,
            note: "render-host cursor pose — never a regime or panel control" },
        ManifestDim { id: "cursor_radius", label: "Cursor radius", control: "internal", not_when: None, min_nodes: None, owned_by: None, note: "" },
        ManifestDim { id: "cursor_strength", label: "Cursor strength", control: "internal", not_when: None, min_nodes: None, owned_by: None, note: "" },
        ManifestDim { id: "steps_per_call", label: "Steps per call", control: "absolute", not_when: None, min_nodes: None, owned_by: None, note: "" },
        ManifestDim { id: "repulsion_radius", label: "Repulsion clip", control: "absolute", not_when: None, min_nodes: None, owned_by: None,
            note: "derived 4x spring_len by apply_engine" },
        ManifestDim { id: "cooling_alpha", label: "Cooling alpha", control: "absolute", not_when: None, min_nodes: None, owned_by: None, note: "" },
        ManifestDim { id: "cooling_floor", label: "Cooling floor", control: "absolute", not_when: None, min_nodes: None, owned_by: None, note: "" },
        ManifestDim { id: "energy_threshold", label: "Energy halt", control: "absolute", not_when: None, min_nodes: None, owned_by: None, note: "" },
        ManifestDim { id: "grid_enabled", label: "Spatial grid", control: "toggle", not_when: None, min_nodes: None, owned_by: None, note: "" },
        ManifestDim { id: "repulsion_mode", label: "Repulsion backend", control: "enum", not_when: None, min_nodes: Some(500), owned_by: None,
            note: "backend choice is a large-graph concern; absent below 500 nodes" },
        ManifestDim { id: "seed_mode", label: "Seed mode", control: "enum", not_when: None, min_nodes: None, owned_by: None,
            note: "none preserves authored positions (E3)" },
        ManifestDim { id: "theta", label: "Barnes-Hut theta", control: "absolute", not_when: None, min_nodes: None, owned_by: None, note: "" },
        ManifestDim { id: "repulsion_samples", label: "NS samples", control: "absolute", not_when: None, min_nodes: Some(500), owned_by: None, note: "" },
    ]
}

/// Evaluate a manifest dimension's `not_when` against live coverage (M1/M2).
pub(crate) fn dim_owned_by_data(dim: &ManifestDim, typed_edges: usize, n_edges: usize) -> bool {
    match dim.not_when {
        Some("typed_bond_coverage_gte:1.0") => n_edges > 0 && typed_edges >= n_edges,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest_ids() -> Vec<String> {
        gpu_force_manifest().iter().map(|d| d.id.to_string()).collect()
    }

    /// Spec §3 parity: the hand-checked manifest's dimension set is exactly
    /// `GpuForceOptions`'s serialized field set — drift in either direction
    /// fails here, so a new options field can never silently lack a
    /// capability declaration.
    #[test]
    fn manifest_fields_match_gpu_force_options_fields() {
        let mut options_fields: Vec<String> = serde_json::to_value(
            graph_layouts::GpuForceOptions::default(),
        )
        .ok()
        .and_then(|v| v.as_object().map(|o| o.keys().cloned().collect()))
        .unwrap_or_default();
        options_fields.sort();
        let mut ids = manifest_ids();
        ids.sort();
        assert_eq!(
            ids,
            options_fields,
            "manifest dimensions must mirror GpuForceOptions fields"
        );
    }

    #[test]
    fn registry_loads_ordered_specific_first() {
        let reg = registry();
        let ids: Vec<&str> = reg.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(
            ids,
            ["molecular-uff", "vault-small"],
            "R3: predicate count desc, filename asc"
        );
        assert!(reg[1].applicability.is_empty(), "R4: catch-all present");
    }

    #[test]
    fn unknown_options_field_fails_validation_loud() {
        let bad = "id: broken\nschema_version: 1\nlabel: Broken\nengine: gpu-force\nexecution: live\napplicability: {}\noptions:\n  no_such_option: 1.0\n";
        let result = std::panic::catch_unwind(|| load_registry(&[("broken.yaml", bad)]));
        assert!(
            result.is_err(),
            "unknown options field must be a loud boot error naming the file"
        );
    }

    #[test]
    fn unknown_applicability_key_is_rejected() {
        let bad = "id: broken\nschema_version: 1\nlabel: Broken\nengine: gpu-force\nexecution: live\napplicability:\n  phase_of_moon: waxing\noptions: {}\n";
        let reg: Result<Regime, _> = serde_yml::from_str(bad);
        assert!(
            reg.is_err(),
            "deny_unknown_fields must reject invented predicates"
        );
    }

    fn state(coverage: f64, n_nodes: u64) -> SnapshotState {
        SnapshotState {
            typed_bond_coverage: coverage,
            typed_nodes: if coverage > 0.0 { 24 } else { 0 },
            typed_edges: (coverage * 25.0) as usize,
            n_nodes,
            n_edges: 25,
            has_authored: true,
            source_kind: "obsidian".into(),
            engine_kind: "local",
        }
    }

    #[test]
    fn resolver_coverage_threshold() {
        assert_eq!(resolve(&state(1.0, 24)).id, "molecular-uff");
        assert_eq!(resolve(&state(0.5, 24)).id, "molecular-uff");
        assert_eq!(resolve(&state(0.49, 24)).id, "vault-small");
        assert_eq!(resolve(&state(0.0, 5000)).id, "vault-small");
    }

    #[test]
    fn resolver_fails_closed_on_unevaluated_predicates() {
        let yaml = "id: guarded\nschema_version: 1\nlabel: Guarded\nengine: gpu-force\nexecution: live\napplicability:\n  source_kind: [obsidian]\noptions: {}\n";
        let reg = load_registry(&[
            ("guarded.yaml", yaml),
            ("vault-small.yaml", REGIME_FILES[1].1),
        ]);
        let hit = reg.iter().find(|r| r.applicability.matches(&state(0.0, 100)));
        assert_eq!(hit.unwrap().id, "vault-small");
    }

    fn molecule_graph(edge_rest: Option<Vec<f32>>) -> GraphData {
        GraphData {
            graph_revision: Some(1),
            n_nodes: 24,
            n_edges: 25,
            num_communities: 1,
            num_wcc: 1,
            ids: Vec::new(),
            id_to_idx: Default::default(),
            scene: crate::render::Scene {
                positions: Vec::new(),
                edges: Vec::new(),
                colors: Vec::new(),
                sizes: Vec::new(),
                edge_rest,
                node_repulsion: None,
            },
        }
    }

    #[test]
    fn molecular_options_fill_spring_len_from_data() {
        let regime = registry().iter().find(|r| r.id == "molecular-uff").unwrap();
        let graph = molecule_graph(Some(vec![1.4; 25]));
        let options = effective_options(regime, &graph).expect("fill succeeds with typed data");
        let spring_len = options.get("spring_len").and_then(|v| v.as_f64()).unwrap();
        assert!(
            (spring_len - 1.4).abs() < 1e-6,
            "null spring_len fills from mean typed rest"
        );
        assert_eq!(options.get("seed_mode").and_then(|v| v.as_str()), Some("none"));
        assert_eq!(options.get("gravity").and_then(|v| v.as_f64()), Some(0.0));
        // Whole block deserializes against the engine struct.
        let parsed: graph_layouts::GpuForceOptions =
            serde_json::from_value(options).expect("fits GpuForceOptions");
        assert_eq!(parsed.seed_mode, graph_layouts::SeedMode::None);

        // R2: null with no typed data fails loud, naming the regime.
        let untyped = molecule_graph(None);
        let err = effective_options(regime, &untyped).expect_err("R2 fail-loud");
        assert!(err.contains("molecular-uff"), "error names the regime: {err}");
        assert!(err.contains("spring_len"), "error names the field: {err}");
    }

    #[test]
    fn mean_rest_ignores_untyped_zeroes() {
        assert_eq!(mean_typed_rest(&[0.0, 2.0, 0.0, 4.0]), Some(3.0));
        assert_eq!(mean_typed_rest(&[0.0, 0.0]), None);
    }

    #[test]
    fn dim_ownership_fires_only_at_full_coverage() {
        let spring_len = gpu_force_manifest()
            .iter()
            .find(|d| d.id == "spring_len")
            .unwrap();
        assert!(
            dim_owned_by_data(spring_len, 25, 25),
            "full coverage → data-owned capsule"
        );
        assert!(
            !dim_owned_by_data(spring_len, 13, 25),
            "partial coverage → live (governs untyped)"
        );
        assert!(!dim_owned_by_data(spring_len, 0, 25), "untyped → live");
    }
}
