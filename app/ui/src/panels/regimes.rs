//! Layout regimes — named parameter bases as data, capability manifests as
//! truth (`docs/layout-ux-spec.md`, implementing `docs/layout-ux.md`).
//!
//! Phases 1–2 (spec §8): the registry loader (serde, `deny_unknown_fields`,
//! options validated against `GpuForceOptions`), the `typed_bond_coverage`
//! resolver, a hand-checked gpu-force capability manifest verified against
//! the options struct by unit test, and `jc_layout_v2` persistence — a
//! pinned regime id plus dimensionless overrides against the resolved base,
//! migrated once from the legacy `jc_layout_v1` absolutes.
//!
//! The effective gpu-force settings block is *derived*: regime base
//! (n-tuned defaults ⊕ regime options, data-owned nulls filled from typed
//! data) ⊕ overrides. Nothing else writes that block, so a registry YAML
//! edit propagates to every user who did not override that control.
//!
//! Engine-truth anchors that bind this module (spec §7):
//! - E1: typed rests win outright; `spring_len` is the untyped/invalid
//!   fallback (`gpu_force.rs:1350-1351`). No control here may claim to scale
//!   typed rests — a data-owned dimension renders as a capsule, never a knob,
//!   and an override on one is parked rather than applied.
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
// runtime-fetched configuration, and the nix appSrc includes app/configs.
const REGIME_FILES: [(&str, &str); 7] = [
    ("balanced.yaml", include_str!("../../../configs/regimes/balanced.yaml")),
    ("fast.yaml", include_str!("../../../configs/regimes/fast.yaml")),
    ("fcose-quality.yaml", include_str!("../../../configs/regimes/fcose-quality.yaml")),
    ("molecular-uff.yaml", include_str!("../../../configs/regimes/molecular-uff.yaml")),
    ("pretty.yaml", include_str!("../../../configs/regimes/pretty.yaml")),
    ("vault-large.yaml", include_str!("../../../configs/regimes/vault-large.yaml")),
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
/// evaluation. Phases 1–2 evaluate `typed_bond_coverage`, `min_nodes`,
/// `max_nodes`, and `engine_kind`; the remaining fields parse (so a regime
/// declaring them is schema-valid authoring) but fail closed at resolution
/// with a warning — a silent match on an unevaluated predicate would lie.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct Applicability {
    pub typed_bond_coverage: Option<CoverageGte>,
    pub min_nodes: Option<u64>,
    pub max_nodes: Option<u64>,
    pub has_authored_positions: Option<bool>,
    pub source_kind: Option<Vec<String>>,
    pub engine_kind: Option<Vec<String>>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CoverageGte {
    pub gte: f64,
}

fn default_true() -> bool {
    true
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Regime {
    pub id: String,
    pub schema_version: u32,
    pub label: String,
    #[serde(default)]
    #[allow(dead_code)] // picker tooltip copy (phase 3)
    pub description: Option<String>,
    pub engine: String,
    pub execution: Execution,
    /// Automatic-resolution eligibility. `false` = picker-only: the regime
    /// is a deliberate user choice (the migrated vault presets), never
    /// inferred from a graph property. NOT a predicate — it takes no part in
    /// the R3 specificity sort. (Spec §9 changelog 2026-09-13: without it,
    /// `fast`/`balanced`/`pretty` — one predicate each — would outrank the
    /// zero-predicate vault catch-all and hijack auto-resolution.)
    #[serde(default = "default_true")]
    pub auto: bool,
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
    /// Declared intent controls (phase 3 renders these; the schema accepts
    /// them now so a regime authored today validates unchanged).
    #[serde(default)]
    #[allow(dead_code)]
    pub controls: Vec<ControlDecl>,
    /// Regime ids quarantined from this regime's picker — vault-tuned
    /// presets must not be offered on ångström geometry.
    #[serde(default)]
    pub presets_hidden: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)] // consumed by the phase-3 intent renderer
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
    fn humanized_reason(&self, state: &SnapshotState) -> String {
        if let Some(cov) = &self.typed_bond_coverage {
            return format!(
                "{} bonds UFF-typed (coverage {:.2} ≥ {:.2})",
                state.typed_edges, state.typed_bond_coverage, cov.gte
            );
        }
        if let Some(min) = self.min_nodes {
            return format!("{} ≥ {min} nodes", state.n_nodes);
        }
        if let Some(max) = self.max_nodes {
            return format!("{} ≤ {max} nodes", state.n_nodes);
        }
        if self.engine_kind.is_some() {
            return format!("{} engine", state.engine_kind);
        }
        "catch-all".to_string()
    }

    /// Fail closed on predicates the resolver cannot evaluate yet: a regime
    /// that declares them never matches (and says why in the log) instead of
    /// matching on a guess.
    fn unevaluated(&self) -> Option<&'static str> {
        if self.has_authored_positions.is_some() {
            Some("has_authored_positions")
        } else if self.source_kind.is_some() {
            Some("source_kind")
        } else {
            None
        }
    }

    fn matches(&self, state: &SnapshotState) -> bool {
        if let Some(field) = self.unevaluated() {
            tracing::warn!(
                "[regimes] predicate {field} declared but not evaluated yet — \
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
        if let Some(kinds) = &self.engine_kind {
            if !kinds.iter().any(|k| k == state.engine_kind) {
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
            validate_regime(&regime).unwrap_or_else(|e| panic!("[regimes] {name}: {e}"));
            (name.to_string(), regime)
        })
        .collect();
    regimes.sort_by(|(an, a), (bn, b)| {
        b.applicability
            .predicate_count()
            .cmp(&a.applicability.predicate_count())
            .then_with(|| an.cmp(bn))
    });
    // R4: resolution must never fail, so an auto-eligible catch-all has to
    // exist. Checked at load so a registry edit cannot remove the floor.
    assert!(
        regimes
            .iter()
            .any(|(_, r)| r.auto && r.applicability.is_empty()),
        "[regimes] registry needs an auto-eligible catch-all regime (R4)"
    );
    // R5 governance: the registry is bounded — a new graph class resolves to
    // the nearest existing regime until three catalog sources justify one.
    assert!(
        regimes.len() <= 8,
        "[regimes] registry is capped at ~8 entries (R5); got {}",
        regimes.len()
    );
    regimes.into_iter().map(|(_, r)| r).collect()
}

fn validate_regime(r: &Regime) -> Result<(), String> {
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
            if manifest_dim(&r.engine, key).is_none() {
                return Err(format!(
                    "unknown options field {key:?} — engine {:?} declares no such dimension",
                    r.engine
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

pub(crate) fn regime_by_id(id: &str) -> Option<&'static Regime> {
    registry().iter().find(|r| r.id == id)
}

// --- snapshot state + resolver (spec §4) ------------------------------------------

/// The five-field resolution state (spec §1). Phases 1–2 populate the
/// fields the coverage, node-bound, and engine-kind predicates need; the
/// rest ride along for later phases.
#[derive(Clone, Debug)]
pub(crate) struct SnapshotState {
    pub typed_bond_coverage: f64,
    #[allow(dead_code)]
    pub typed_nodes: usize,
    pub typed_edges: usize,
    pub n_nodes: u64,
    #[allow(dead_code)]
    pub n_edges: usize,
    #[allow(dead_code)]
    pub has_authored: bool,
    #[allow(dead_code)]
    pub source_kind: String,
    pub engine_kind: &'static str,
}

/// First auto-eligible regime in registry order whose applicability matches
/// (R3: no expression language, first match wins). The registry's catch-all
/// guarantees a result.
/// First auto-eligible regime for `engine` whose applicability matches. The
/// gpu-force catch-all guarantees a result for the local physics engine; an
/// engine with no regime at all falls back to it, which is honest — the
/// panel then reports the gpu-force regime it actually resolved, and a
/// regime-less engine renders its own settings form instead.
pub(crate) fn resolve_for(engine: &str, state: &SnapshotState) -> &'static Regime {
    registry()
        .iter()
        .find(|r| r.auto && r.engine == engine && r.applicability.matches(state))
        .or_else(|| {
            registry()
                .iter()
                .find(|r| r.auto && r.engine == "gpu-force" && r.applicability.matches(state))
        })
        .expect("registry contains a catch-all regime (R4)")
}

/// Whether any regime in the registry targets `engine` — i.e. whether the
/// panel should render a regime surface for it at all.
pub(crate) fn engine_has_regime(engine: &str) -> bool {
    registry().iter().any(|r| r.engine == engine)
}

/// Regimes the picker may offer for this state: applicable (auto-eligible or
/// not) minus the ones the active regime quarantines (`presets_hidden`).
pub(crate) fn selectable_regimes(
    state: &SnapshotState,
    active: &Regime,
) -> Vec<&'static Regime> {
    registry()
        .iter()
        .filter(|r| r.engine == active.engine)
        .filter(|r| r.id == active.id || r.applicability.matches(state))
        .filter(|r| !active.presets_hidden.iter().any(|hidden| *hidden == r.id))
        .collect()
}

/// What the panel renders: the resolved regime plus the measurements that
/// decided the match (UI2/UI5, M2).
#[derive(Clone, Debug, Default)]
pub(crate) struct RegimeResolution {
    pub regime_id: String,
    /// Engine this regime targets — the panel renders a regime surface only
    /// for the engine actually running.
    pub engine: String,
    /// `live` (continuous sim) or `one_shot` (Solve-driven solver): decides
    /// whether live-sim rows exist at all (UI6/R1).
    pub one_shot: bool,
    pub label: String,
    pub reason: String,
    /// The user pinned this regime in the picker (no auto-resolution).
    pub pinned: bool,
    pub typed_nodes: usize,
    pub typed_edges: usize,
    pub n_nodes: usize,
    pub n_edges: usize,
    #[allow(dead_code)] // typed_edges/n_edges carry this to the panel copy
    pub coverage: f64,
    /// Regime ids the picker offers, with display labels.
    pub choices: Vec<(String, String)>,
    /// Overrides retained but not applied because their dimension is
    /// data-owned under the current graph state (spec §1 "parked").
    pub parked: usize,
    /// Names of the parked controls, for the `why ▸` disclosure.
    pub parked_ids: Vec<String>,
    /// Overrides currently in effect.
    pub applied_overrides: usize,
    /// The active regime's declared intent controls, ready to render.
    pub intents: Vec<IntentView>,
    /// Regime ids this regime quarantines (`why ▸` copy).
    pub presets_hidden: Vec<String>,
}

/// One declared intent control, resolved against the live state: a
/// dimensionless multiplier (or toggle) over the regime base, never an
/// absolute engine constant.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct IntentView {
    pub id: String,
    pub label: String,
    pub toggle: bool,
    /// Declared option labels for an `enum` control (empty otherwise) and
    /// the index currently selected.
    pub choices: Vec<String>,
    pub selected: usize,
    /// Multiplier range (`kind: multiplier`); neutral is 1.0.
    pub range: (f64, f64),
    /// Current multiplier, or 1.0 with no override.
    pub multiplier: f64,
    /// Current toggle position (`kind: toggle`).
    pub on: bool,
    /// Humanized list of the option fields this intent scales — the
    /// control's tooltip, so no knob hides what it moves.
    pub affects: String,
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

/// Everything the base/override math needs from the loaded graph. Kept as a
/// small copyable record so an override edit can recompute the effective
/// settings without re-reading the scene buffers.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct GraphInput {
    pub n_nodes: usize,
    pub n_edges: usize,
    pub typed_nodes: usize,
    pub typed_edges: usize,
    pub mean_typed_rest: Option<f32>,
}

static INPUT: GlobalSignal<GraphInput> = Signal::global(GraphInput::default);

fn snapshot_state(input: &GraphInput) -> SnapshotState {
    SnapshotState {
        typed_bond_coverage: input.typed_edges as f64 / input.n_edges.max(1) as f64,
        typed_nodes: input.typed_nodes,
        typed_edges: input.typed_edges,
        n_nodes: input.n_nodes as u64,
        n_edges: input.n_edges,
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

// --- base + overrides (spec §5) ---------------------------------------------------

/// The regime's parameter base: the engine's n-tuned defaults, overlaid with
/// the regime's declared options, with data-owned (`null`) fields filled
/// from typed graph data. R2: a null with no data source is an error naming
/// the regime and field.
pub(crate) fn base_options(
    regime: &Regime,
    input: &GraphInput,
) -> Result<serde_json::Map<String, Value>, String> {
    // The engine's own defaults are the floor: gpu-force is n-tuned
    // (`for_n_nodes` scales spring_len/repulsion/halt with the node count —
    // no YAML value can express that), every other engine takes its
    // registry defaults. Installing one engine's block onto another would
    // hand the solver settings it cannot even deserialize.
    let defaults = if regime.engine == "gpu-force" {
        serde_json::to_value(graph_layouts::GpuForceOptions::for_n_nodes(input.n_nodes))
            .map_err(|e| format!("regime {}: n-tuned defaults: {e}", regime.id))?
    } else {
        crate::panels::layout::engine_default_settings(&regime.engine)
    };
    let mut base = defaults.as_object().cloned().ok_or_else(|| {
        format!(
            "regime {}: engine {:?} has no default settings object",
            regime.id, regime.engine
        )
    })?;
    let Some(map) = &regime.options else {
        return Ok(base);
    };
    for (key, value) in map {
        let key = key
            .as_str()
            .ok_or_else(|| format!("regime {}: non-string options key", regime.id))?;
        let json = match yml_to_json(value)? {
            Value::Null => data_fill(key, input).ok_or_else(|| {
                format!(
                    "regime {id}: options field {key:?} is null (data-owned) but the \
                     loaded graph carries no typed value for it (R2)",
                    id = regime.id
                )
            })?,
            filled => filled,
        };
        base.insert(key.to_string(), json);
    }
    Ok(base)
}

/// Values the loaded graph's typed data can supply for a data-owned field.
fn data_fill(field: &str, input: &GraphInput) -> Option<Value> {
    match field {
        "spring_len" => input.mean_typed_rest.map(|mean| serde_json::json!(mean)),
        _ => None,
    }
}

/// A user-set control value, stored against the regime base (spec P1).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Override {
    /// Dimensionless factor on the resolved base: a registry YAML edit
    /// propagates to everyone who did not override that control, and no
    /// absolute value crosses a regime boundary.
    Multiplier { value: f64 },
    /// Values a base cannot scale: enums, toggles, and numeric bases of
    /// exactly zero (scaling zero is not an override).
    Absolute { value: Value },
}

/// How a user-entered absolute value is stored against `base`: as a
/// multiplier when the base is a nonzero number, absolute otherwise.
pub(crate) fn override_for(base: Option<&Value>, entered: &Value) -> Override {
    match (base.and_then(Value::as_f64), entered.as_f64()) {
        (Some(b), Some(v)) if b.abs() > f64::EPSILON => Override::Multiplier { value: v / b },
        _ => Override::Absolute {
            value: entered.clone(),
        },
    }
}

/// Persisted layout state (spec P1). `regime_id` is a *pin*: the user's
/// picker choice, honored while that regime still matches the loaded graph.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct LayoutStateV2 {
    pub regime_id: Option<String>,
    /// Control id (a gpu-force option field through phase 2) → override.
    pub overrides: BTreeMap<String, Override>,
}

const STATE_KEY_V2: &str = "jc_layout_v2";
/// Legacy absolute settings bag (`PanelState`). Left in place for one
/// release after migration, per spec P2.
const STATE_KEY_V1: &str = "jc_layout_v1";

fn load_v2() -> Option<LayoutStateV2> {
    LocalStorage::get(STATE_KEY_V2).ok()
}

fn save_v2(state: &LayoutStateV2) {
    let _ = LocalStorage::set(STATE_KEY_V2, state);
}

/// Legacy `jc_layout_v1` absolutes → overrides against a base (spec P2).
/// A legacy value equal to the base is not an override; unknown legacy
/// fields are dropped silently.
pub(crate) fn migrate_overrides(
    legacy_gpu_force: &Value,
    base: &serde_json::Map<String, Value>,
) -> BTreeMap<String, Override> {
    let mut overrides = BTreeMap::new();
    let Some(legacy) = legacy_gpu_force.as_object() else {
        return overrides;
    };
    for (field, value) in legacy {
        if manifest_dim("gpu-force", field).is_none() {
            continue; // unknown legacy field: dropped
        }
        if manifest_dim("gpu-force", field).is_some_and(|d| d.control == "internal") {
            continue; // cursor pose is render-host state, never an override
        }
        let base_value = base.get(field);
        if base_value == Some(value) {
            continue; // identical to the base: nothing to carry
        }
        overrides.insert(field.clone(), override_for(base_value, value));
    }
    overrides
}

/// Effective settings for the active regime: base ⊕ overrides. Overrides on
/// a dimension the data owns under the current state are *parked* — retained
/// in storage, surfaced in the panel, never applied (M2/E1).
pub(crate) fn effective_options(
    regime: &Regime,
    overrides: &BTreeMap<String, Override>,
    input: &GraphInput,
) -> Result<Effective, String> {
    let mut base = base_options(regime, input)?;
    let mut parked: Vec<String> = Vec::new();
    let mut applied = 0usize;
    for (key, value) in overrides {
        // An override key is either an engine option field (raw control in
        // the Advanced disclosure) or one of the active regime's declared
        // intents, which expands onto option fields through `maps_to`.
        let targets: Vec<(String, f64)> = match manifest_dim(&regime.engine, key) {
            Some(_) => vec![(key.clone(), 1.0)],
            None => match regime.controls.iter().find(|c| c.id == *key) {
                Some(control) => intent_targets(&regime.engine, control),
                None => {
                    // An intent another regime declares (the user set it
                    // there) is *parked* under this one: retained, surfaced,
                    // not applied. A control no regime declares is stale —
                    // dropped silently.
                    if registry()
                        .iter()
                        .any(|r| r.controls.iter().any(|c| c.id == *key))
                    {
                        parked.push(key.clone());
                    }
                    continue;
                }
            },
        };
        if targets.is_empty() {
            continue;
        }
        // M2/E1: an intent whose every target is data-owned has nothing
        // honest to do — park it whole rather than move a subset silently.
        let live: Vec<(String, f64)> = targets
            .into_iter()
            .filter(|(field, _)| {
                !manifest_dim(&regime.engine, field).is_some_and(|dim| {
                    dim_owned_by_data(&dim, input.typed_edges, input.n_edges)
                })
            })
            .collect();
        if live.is_empty() {
            parked.push(key.clone());
            continue;
        }
        let mut touched = false;
        for (field, exponent) in live {
            match value {
                Override::Multiplier { value } => {
                    let factor = if (exponent - 1.0).abs() < f64::EPSILON {
                        *value
                    } else {
                        value.powf(exponent)
                    };
                    if let Some(b) = base.get(&field).and_then(Value::as_f64) {
                        let scaled = b * factor;
                        let is_int = base
                            .get(&field)
                            .is_some_and(|v| v.is_i64() || v.is_u64());
                        base.insert(
                            field.clone(),
                            if is_int {
                                serde_json::json!(scaled.round().max(0.0) as i64)
                            } else {
                                serde_json::json!(scaled)
                            },
                        );
                        touched = true;
                    }
                }
                Override::Absolute { value } => {
                    base.insert(field.clone(), choice_value(key, regime, &field, value));
                    touched = true;
                }
            }
        }
        if touched {
            applied += 1;
        }
    }
    clamp_engine_ranges(&mut base);
    Ok(Effective {
        options: Value::Object(base),
        parked,
        applied,
    })
}

/// The derived gpu-force settings plus what the override layer did with the
/// user's stored intent: which controls were applied, and which are parked
/// because the loaded graph's data owns their dimension.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Effective {
    pub options: Value,
    pub parked: Vec<String>,
    pub applied: usize,
}

/// The active regime's declared controls, resolved against stored
/// overrides: each is a dimensionless multiplier (or a toggle) over the
/// regime base. Controls whose every target dimension the data owns are
/// omitted — a knob that cannot move anything must not exist (M2/M3).
pub(crate) fn intent_views(
    regime: &Regime,
    overrides: &BTreeMap<String, Override>,
    input: &GraphInput,
    base: &serde_json::Map<String, Value>,
) -> Vec<IntentView> {
    regime
        .controls
        .iter()
        .filter_map(|control| {
            let targets = intent_targets(&regime.engine, control);
            if targets.is_empty() {
                return None;
            }
            let live: Vec<&(String, f64)> = targets
                .iter()
                .filter(|(field, _)| {
                    !manifest_dim(&regime.engine, field).is_some_and(|dim| {
                        dim_owned_by_data(&dim, input.typed_edges, input.n_edges)
                    })
                })
                .collect();
            if live.is_empty() {
                return None;
            }
            let stored = overrides.get(&control.id);
            let toggle = control.kind == ControlKind::Toggle;
            let choices = if control.kind == ControlKind::Enum {
                control.options.clone().unwrap_or_default()
            } else {
                Vec::new()
            };
            // Which option is live: an explicit override, else whatever the
            // base already carries (so the control shows the running value).
            let selected = match stored {
                Some(Override::Absolute { value }) => match value {
                    Value::Number(n) => n.as_u64().unwrap_or(0) as usize,
                    Value::String(_) => control
                        .maps_to
                        .as_ref()
                        .and_then(|m| m.values().find_map(|v| v.as_array()))
                        .and_then(|arr| arr.iter().position(|v| v == value))
                        .unwrap_or(0),
                    _ => 0,
                },
                _ => control
                    .maps_to
                    .as_ref()
                    .and_then(|m| {
                        m.iter().find_map(|(field, values)| {
                            let arr = values.as_array()?;
                            arr.iter().position(|v| Some(v) == base.get(field))
                        })
                    })
                    .unwrap_or(0),
            };
            let on = if toggle {
                match stored {
                    Some(Override::Absolute { value }) => value.as_bool().unwrap_or(false),
                    // No override: read the toggle's current position off the
                    // base, so the checkbox reflects what the sim is running.
                    _ => toggle_is_on(control, base),
                }
            } else {
                false
            };
            let multiplier = match stored {
                Some(Override::Multiplier { value }) => *value,
                _ => 1.0,
            };
            Some(IntentView {
                id: control.id.clone(),
                label: control.label.clone(),
                toggle,
                choices,
                selected,
                range: control.range.unwrap_or((0.5, 2.0)),
                multiplier,
                on,
                affects: live
                    .iter()
                    .map(|(field, exponent)| {
                        if (*exponent - 1.0).abs() < f64::EPSILON {
                            field.clone()
                        } else {
                            format!("{field}^{exponent}")
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(", "),
            })
        })
        .collect()
}

/// Is a toggle intent currently in its "on" position, judged by the base
/// value of the first field it maps?
fn toggle_is_on(control: &ControlDecl, base: &serde_json::Map<String, Value>) -> bool {
    let Some(maps_to) = &control.maps_to else {
        return false;
    };
    maps_to.iter().any(|(field, choices)| {
        let Some(choices) = choices.as_array() else {
            return false;
        };
        base.get(field) == choices.get(1)
    })
}

/// `(option field, exponent)` pairs an intent scales. A control whose id is
/// itself an option field maps 1:1 (the Repulsion intent is exactly the
/// engine's repulsion strength — E2 makes that honest on typed atoms);
/// otherwise the regime declares the mapping in `maps_to`, where a numeric
/// value is the exponent applied to the multiplier (1.0 scales with it,
/// -1.0 inversely, 0.5 as its square root).
fn intent_targets(engine: &str, control: &ControlDecl) -> Vec<(String, f64)> {
    if manifest_dim(engine, &control.id).is_some() {
        return vec![(control.id.clone(), 1.0)];
    }
    let Some(maps_to) = &control.maps_to else {
        return Vec::new();
    };
    maps_to
        .iter()
        .filter(|(field, _)| manifest_dim(engine, field).is_some())
        .filter_map(|(field, weight)| match weight {
            // Multiplier intent: the number is the exponent applied to the
            // multiplier for this field.
            Value::Number(_) => weight.as_f64().map(|w| (field.clone(), w)),
            // Toggle intent: `[off, on]` option values, selected by
            // `toggle_value` — no exponent takes part.
            Value::Array(_) => Some((field.clone(), 1.0)),
            _ => None,
        })
        .collect()
}

/// Resolve a stored choice to the option value the regime declares. A
/// toggle declares two values (`maps_to: { field: [off, on] }`) and stores a
/// bool; an enum declares one value per option and stores the option index.
/// A raw field override (no declared choices) stores its value verbatim.
fn choice_value(control_id: &str, regime: &Regime, field: &str, entered: &Value) -> Value {
    let declared = regime
        .controls
        .iter()
        .find(|c| c.id == control_id)
        .and_then(|c| c.maps_to.as_ref()?.get(field)?.as_array().cloned());
    let Some(choices) = declared else {
        return entered.clone();
    };
    let index = match entered {
        Value::Bool(on) => Some(usize::from(*on)),
        Value::Number(n) => n.as_u64().map(|i| i as usize),
        // An explicit value the regime declares is kept as-is.
        Value::String(_) if choices.contains(entered) => return entered.clone(),
        _ => None,
    };
    index
        .and_then(|i| choices.get(i).cloned())
        .unwrap_or_else(|| entered.clone())
}

/// Keep an intent from driving an option outside the range the engine can
/// use. The shader clamps `cooling_alpha` at its use site; `damping` above
/// 1.0 would inject energy every step instead of removing it.
fn clamp_engine_ranges(base: &mut serde_json::Map<String, Value>) {
    const CLAMPS: [(&str, f64, f64); 4] = [
        ("damping", 0.0, 0.999),
        ("cooling_alpha", 0.5, 1.0),
        ("cooling_floor", 0.0, 1.0),
        ("dt", 1.0e-4, 1.0),
    ];
    for (field, lo, hi) in CLAMPS {
        if let Some(value) = base.get(field).and_then(Value::as_f64) {
            let clamped = value.clamp(lo, hi);
            if (clamped - value).abs() > f64::EPSILON {
                base.insert(field.to_string(), serde_json::json!(clamped));
            }
        }
    }
}

// --- resolution + install ----------------------------------------------------------

/// Resolve the regime for a freshly loaded graph, migrate legacy settings on
/// first run, publish the resolution the panel renders, and install the
/// effective settings. Called by every `GraphData` producer (server load,
/// github vault, embedded world) — `TYPED_FORCE_SUMMARY` is already written
/// when this runs.
pub(crate) fn on_graph_loaded(graph: &GraphData) {
    let summary = *crate::graph_canvas::TYPED_FORCE_SUMMARY.peek();
    let (typed_nodes, typed_edges) = summary.unwrap_or((0, 0));
    let input = GraphInput {
        n_nodes: graph.n_nodes as usize,
        n_edges: graph.n_edges as usize,
        typed_nodes,
        typed_edges,
        mean_typed_rest: graph.scene.edge_rest.as_deref().and_then(mean_typed_rest),
    };
    *INPUT.write() = input;
    migrate_if_needed(&input);
    reapply();
}

/// One-shot `jc_layout_v1` → `jc_layout_v2` migration (spec P2). Legacy
/// absolutes divide by the `vault-large` base for the same field, so their
/// historic vault meaning is preserved as dimensionless overrides, and the
/// pin records the regime they were authored against. The v1 key is left in
/// place for one release.
fn migrate_if_needed(input: &GraphInput) {
    if load_v2().is_some() {
        return;
    }
    let legacy: Option<Value> = LocalStorage::get::<Value>(STATE_KEY_V1)
        .ok()
        .and_then(|v| v.get("settings")?.get("gpu-force").cloned());
    let Some(legacy) = legacy else {
        save_v2(&LayoutStateV2::default());
        return;
    };
    let Some(vault_large) = regime_by_id("vault-large") else {
        save_v2(&LayoutStateV2::default());
        return;
    };
    let Ok(base) = base_options(vault_large, input) else {
        save_v2(&LayoutStateV2::default());
        return;
    };
    let overrides = migrate_overrides(&legacy, &base);
    let migrated = LayoutStateV2 {
        regime_id: (!overrides.is_empty()).then(|| vault_large.id.clone()),
        overrides,
    };
    tracing::info!(
        "[regimes] migrated {} legacy gpu-force value(s) to jc_layout_v2",
        migrated.overrides.len()
    );
    save_v2(&migrated);
}

/// Re-resolve and re-install after the running engine changed.
pub(crate) fn on_engine_changed() {
    reapply();
}

/// Pin (or unpin, with `None`) the regime the picker selected, then
/// recompute. An unpinned panel follows automatic resolution.
pub(crate) fn pin(regime_id: Option<String>) {
    let mut state = load_v2().unwrap_or_default();
    state.regime_id = regime_id;
    save_v2(&state);
    reapply();
}

/// Record one control's user-entered absolute value as an override against
/// the active regime base, then recompute.
pub(crate) fn set_option_override(field: &str, entered: Value) {
    let input = *INPUT.peek();
    let mut state = load_v2().unwrap_or_default();
    let engine = crate::panels::layout::active_engine_id();
    let active = active_regime(&state, &snapshot_state(&input), &engine);
    let base = base_options(active, &input).unwrap_or_default();
    state
        .overrides
        .insert(field.to_string(), override_for(base.get(field), &entered));
    save_v2(&state);
    reapply();
}

/// Record an intent control's multiplier (1.0 = neutral, i.e. no override).
pub(crate) fn set_intent_multiplier(control_id: &str, multiplier: f64) {
    let mut state = load_v2().unwrap_or_default();
    if (multiplier - 1.0).abs() < 1.0e-6 {
        state.overrides.remove(control_id);
    } else {
        state.overrides.insert(
            control_id.to_string(),
            Override::Multiplier { value: multiplier },
        );
    }
    save_v2(&state);
    reapply();
}

/// Record an enum intent's selected option index. The regime's `maps_to`
/// arrays turn the index into the value(s) the engine accepts.
pub(crate) fn set_intent_choice(control_id: &str, index: usize) {
    let mut state = load_v2().unwrap_or_default();
    state.overrides.insert(
        control_id.to_string(),
        Override::Absolute {
            value: serde_json::json!(index),
        },
    );
    save_v2(&state);
    reapply();
}

/// Record a toggle intent's position. Stored as an absolute: the regime's
/// `maps_to` names the two option values it selects between.
pub(crate) fn set_intent_toggle(control_id: &str, on: bool) {
    let mut state = load_v2().unwrap_or_default();
    state.overrides.insert(
        control_id.to_string(),
        Override::Absolute {
            value: serde_json::json!(on),
        },
    );
    save_v2(&state);
    reapply();
}

/// Drop every override and follow automatic resolution again (the panel's
/// reset-to-defaults action for gpu-force).
pub(crate) fn reset() {
    save_v2(&LayoutStateV2::default());
    reapply();
}

/// The regime in effect: the pin when it exists and still matches the loaded
/// graph (spec §4 "if manual_override active and regime still loads: keep"),
/// otherwise automatic resolution.
fn active_regime(
    state: &LayoutStateV2,
    snapshot: &SnapshotState,
    engine: &str,
) -> &'static Regime {
    state
        .regime_id
        .as_deref()
        .and_then(regime_by_id)
        .filter(|r| r.engine == engine && r.applicability.matches(snapshot))
        .unwrap_or_else(|| resolve_for(engine, snapshot))
}

/// Recompute the resolution + effective settings from persisted state and
/// the current graph, and install them. The single writer of the gpu-force
/// settings block.
fn reapply() {
    let input = *INPUT.peek();
    let state = load_v2().unwrap_or_default();
    let snapshot = snapshot_state(&input);
    let engine = crate::panels::layout::active_engine_id();
    if !engine_has_regime(&engine) {
        // A regime-less engine (a static solver with no authored regime,
        // or a remote bridge) keeps its own settings form: publishing a
        // regime for it would be a claim nothing backs.
        *RESOLUTION.write() = None;
        return;
    }
    let regime = active_regime(&state, &snapshot, &engine);
    let pinned = state
        .regime_id
        .as_deref()
        .is_some_and(|id| id == regime.id.as_str());

    match effective_options(regime, &state.overrides, &input) {
        Ok(effective) => {
            let base = base_options(regime, &input).unwrap_or_default();
            *RESOLUTION.write() = Some(RegimeResolution {
                regime_id: regime.id.clone(),
                engine: regime.engine.clone(),
                one_shot: regime.execution == Execution::OneShot,
                label: regime.label.clone(),
                reason: if pinned {
                    "pinned".to_string()
                } else {
                    regime.applicability.humanized_reason(&snapshot)
                },
                pinned,
                typed_nodes: input.typed_nodes,
                typed_edges: input.typed_edges,
                n_nodes: input.n_nodes,
                n_edges: input.n_edges,
                coverage: snapshot.typed_bond_coverage,
                choices: selectable_regimes(&snapshot, regime)
                    .into_iter()
                    .map(|r| (r.id.clone(), r.label.clone()))
                    .collect(),
                parked: effective.parked.len(),
                parked_ids: effective.parked.clone(),
                applied_overrides: effective.applied,
                intents: intent_views(regime, &state.overrides, &input, &base),
                presets_hidden: regime.presets_hidden.clone(),
            });
            crate::panels::layout::install_engine_settings(&regime.engine, effective.options);
        }
        Err(error) => {
            // R2 loud error: keep the previous settings rather than install
            // a base the data cannot fill.
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

#[allow(dead_code)] // label/owned_by/note surface in the Advanced disclosure
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ManifestDim {
    pub id: String,
    pub label: String,
    /// "multiplier" | "toggle" | "enum" | "absolute" ("internal" for
    /// cursor/transient fields no regime or control should carry).
    pub control: &'static str,
    /// Fires when the data owns this dimension outright → capsule (M2).
    pub not_when: Option<&'static str>,
    /// Inverse applicability gate (the backend enum's n≥500).
    pub min_nodes: Option<u64>,
    pub owned_by: Option<String>,
    pub note: String,
}

/// The curated gpu-force table: the one engine whose dimensions carry real
/// applicability predicates (E1's data-owned `spring_len`, the backend
/// enum's node floor). Verified field-for-field against `GpuForceOptions`
/// by unit test.
static GPU_FORCE_MANIFEST: std::sync::LazyLock<Vec<ManifestDim>> =
    std::sync::LazyLock::new(|| {
        vec![
        ManifestDim { id: "repulsion".into(), label: "Repulsion".into(), control: "absolute", not_when: None, min_nodes: None, owned_by: None,
            note: "mixes with per-atom UFF weights via sqrt(wi*wj) — honest on typed atoms (E2)".into() },
        ManifestDim { id: "spring_k".into(), label: "Bond stiffness".into(), control: "absolute", not_when: None, min_nodes: None, owned_by: None,
            note: "spring force stiffness".into() },
        ManifestDim { id: "spring_len".into(), label: "Bond scale".into(), control: "multiplier", not_when: Some("typed_bond_coverage_gte:1.0"), min_nodes: None, owned_by: Some("uff bond table".into()),
            note: "governs untyped edges only when coverage is partial (E1)".into() },
        ManifestDim { id: "gravity".into(), label: "Gravity".into(), control: "absolute", not_when: None, min_nodes: None, owned_by: None, note: "".into() },
        ManifestDim { id: "damping".into(), label: "Damping".into(), control: "absolute", not_when: None, min_nodes: None, owned_by: None, note: "".into() },
        ManifestDim { id: "dt".into(), label: "Time step".into(), control: "absolute", not_when: None, min_nodes: None, owned_by: None, note: "".into() },
        ManifestDim { id: "cursor_pos".into(), label: "Cursor".into(), control: "internal", not_when: None, min_nodes: None, owned_by: None,
            note: "render-host cursor pose — never a regime or panel control".into() },
        ManifestDim { id: "cursor_radius".into(), label: "Cursor radius".into(), control: "internal", not_when: None, min_nodes: None, owned_by: None, note: "".into() },
        ManifestDim { id: "cursor_strength".into(), label: "Cursor strength".into(), control: "internal", not_when: None, min_nodes: None, owned_by: None, note: "".into() },
        ManifestDim { id: "steps_per_call".into(), label: "Steps per call".into(), control: "absolute", not_when: None, min_nodes: None, owned_by: None, note: "".into() },
        ManifestDim { id: "repulsion_radius".into(), label: "Repulsion clip".into(), control: "absolute", not_when: None, min_nodes: None, owned_by: None,
            note: "derived 4x spring_len by apply_engine".into() },
        ManifestDim { id: "cooling_alpha".into(), label: "Cooling alpha".into(), control: "absolute", not_when: None, min_nodes: None, owned_by: None, note: "".into() },
        ManifestDim { id: "cooling_floor".into(), label: "Cooling floor".into(), control: "absolute", not_when: None, min_nodes: None, owned_by: None, note: "".into() },
        ManifestDim { id: "energy_threshold".into(), label: "Energy halt".into(), control: "absolute", not_when: None, min_nodes: None, owned_by: None, note: "".into() },
        ManifestDim { id: "grid_enabled".into(), label: "Spatial grid".into(), control: "toggle", not_when: None, min_nodes: None, owned_by: None, note: "".into() },
        ManifestDim { id: "repulsion_mode".into(), label: "Repulsion backend".into(), control: "enum", not_when: None, min_nodes: Some(500), owned_by: None,
            note: "backend choice is a large-graph concern; absent below 500 nodes".into() },
        ManifestDim { id: "seed_mode".into(), label: "Seed mode".into(), control: "enum", not_when: None, min_nodes: None, owned_by: None,
            note: "none preserves authored positions (E3)".into() },
        ManifestDim { id: "theta".into(), label: "Barnes-Hut theta".into(), control: "absolute", not_when: None, min_nodes: None, owned_by: None, note: "".into() },
        ManifestDim { id: "repulsion_samples".into(), label: "NS samples".into(), control: "absolute", not_when: None, min_nodes: Some(500), owned_by: None, note: "".into() },
    ]
    });

pub(crate) fn gpu_force_manifest() -> &'static [ManifestDim] {
    &GPU_FORCE_MANIFEST
}

/// The capability manifest for one local engine. `gpu-force` carries the
/// curated table above (its dimensions have real applicability predicates);
/// every other local engine is *projected* from its own default settings —
/// one dimension per serialized field, control kind inferred from the value
/// — the same rule the worker applies in `manifest_from_settings`. A
/// projection makes no applicability claims, because nothing is known.
pub(crate) fn engine_manifest(engine: &str) -> Vec<ManifestDim> {
    if engine == "gpu-force" {
        return gpu_force_manifest().to_vec();
    }
    let defaults = crate::panels::layout::engine_default_settings(engine);
    let Some(fields) = defaults.as_object() else {
        return Vec::new();
    };
    fields
        .iter()
        .map(|(id, value)| ManifestDim {
            id: id.clone(),
            label: humanize_field(id),
            control: match value {
                Value::Bool(_) => "toggle",
                Value::String(_) => "enum",
                Value::Number(_) => "absolute",
                // Lists and tagged structures are honoured but are not one
                // knob; a client must edit them structurally.
                _ => "internal",
            },
            not_when: None,
            min_nodes: None,
            owned_by: None,
            note: String::new(),
        })
        .collect()
}

/// `ideal_edge_length` → `Ideal edge length`: a label for a field the panel
/// learned about by projection, without inventing copy.
fn humanize_field(id: &str) -> String {
    let spaced = id.replace('_', " ");
    let mut chars = spaced.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => spaced,
    }
}

pub(crate) fn manifest_dim(engine: &str, id: &str) -> Option<ManifestDim> {
    engine_manifest(engine).into_iter().find(|d| d.id == id)
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
        let mut ids: Vec<String> = gpu_force_manifest().iter().map(|d| d.id.to_string()).collect();
        ids.sort();
        assert_eq!(
            ids, options_fields,
            "manifest dimensions must mirror GpuForceOptions fields"
        );
    }

    #[test]
    fn registry_loads_ordered_specific_first() {
        let ids: Vec<&str> = registry().iter().map(|r| r.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                // one predicate each, filename-ascending…
                "balanced",
                "fast",
                "molecular-uff",
                "pretty",
                "vault-large",
                // …then the catch-alls
                "fcose-quality",
                "vault-small",
            ],
            "R3: predicate count desc, filename asc"
        );
        let gpu_catch_all = registry()
            .iter()
            .find(|r| r.applicability.is_empty() && r.engine == "gpu-force" && r.auto);
        assert_eq!(gpu_catch_all.map(|r| r.id.as_str()), Some("vault-small"), "R4");
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

    /// Registry YAML by filename — never by index, which shifts whenever a
    /// regime is added.
    fn regime_file(name: &str) -> &'static str {
        REGIME_FILES
            .iter()
            .find(|(file, _)| *file == name)
            .map(|(_, yaml)| *yaml)
            .unwrap_or_else(|| panic!("no registry file {name}"))
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
    fn resolver_coverage_threshold_and_node_bounds() {
        assert_eq!(resolve_for("gpu-force", &state(1.0, 24)).id, "molecular-uff");
        assert_eq!(resolve_for("gpu-force", &state(0.5, 24)).id, "molecular-uff");
        assert_eq!(resolve_for("gpu-force", &state(0.49, 24)).id, "vault-small");
        assert_eq!(resolve_for("gpu-force", &state(0.0, 999)).id, "vault-small");
        assert_eq!(resolve_for("gpu-force", &state(0.0, 1000)).id, "vault-large");
    }

    /// Presets are picker-only: a one-predicate `engine_kind` regime must
    /// not outrank the zero-predicate vault catch-all in auto-resolution.
    #[test]
    fn presets_never_win_auto_resolution_but_are_selectable() {
        let untyped = state(0.0, 200);
        assert_eq!(resolve_for("gpu-force", &untyped).id, "vault-small");
        let active = resolve_for("gpu-force", &untyped);
        let offered: Vec<&str> = selectable_regimes(&untyped, active)
            .iter()
            .map(|r| r.id.as_str())
            .collect();
        for preset in ["fast", "balanced", "pretty"] {
            assert!(offered.contains(&preset), "picker offers {preset}: {offered:?}");
        }
        assert!(
            !offered.contains(&"molecular-uff"),
            "molecular regime is not applicable to an untyped graph: {offered:?}"
        );
    }

    /// `presets_hidden` quarantines the vault-tuned presets from ångström
    /// geometry — on every surface, including the picker.
    #[test]
    fn molecular_regime_hides_vault_presets() {
        let molecule = state(1.0, 24);
        let active = resolve_for("gpu-force", &molecule);
        assert_eq!(active.id, "molecular-uff");
        let offered: Vec<&str> = selectable_regimes(&molecule, active)
            .iter()
            .map(|r| r.id.as_str())
            .collect();
        assert_eq!(offered, ["molecular-uff", "vault-small"], "presets quarantined");
    }

    #[test]
    fn resolver_fails_closed_on_unevaluated_predicates() {
        let yaml = "id: guarded\nschema_version: 1\nlabel: Guarded\nengine: gpu-force\nexecution: live\napplicability:\n  source_kind: [obsidian]\noptions: {}\n";
        let reg = load_registry(&[
            ("guarded.yaml", yaml),
            ("vault-small.yaml", regime_file("vault-small.yaml")),
        ]);
        let hit = reg
            .iter()
            .find(|r| r.auto && r.applicability.matches(&state(0.0, 100)));
        assert_eq!(hit.unwrap().id, "vault-small");
    }

    fn molecule_input(mean_rest: Option<f32>) -> GraphInput {
        GraphInput {
            n_nodes: 24,
            n_edges: 25,
            typed_nodes: 24,
            typed_edges: 25,
            mean_typed_rest: mean_rest,
        }
    }

    #[test]
    fn molecular_base_fills_spring_len_from_data() {
        let regime = regime_by_id("molecular-uff").unwrap();
        let base = base_options(regime, &molecule_input(Some(1.4))).expect("typed fill");
        assert!(
            (base["spring_len"].as_f64().unwrap() - 1.4).abs() < 1e-6,
            "null spring_len fills from the mean typed rest"
        );
        assert_eq!(base["seed_mode"], serde_json::json!("none"));
        assert_eq!(base["gravity"], serde_json::json!(0.0));
        let parsed: graph_layouts::GpuForceOptions =
            serde_json::from_value(Value::Object(base)).expect("fits GpuForceOptions");
        assert_eq!(parsed.seed_mode, graph_layouts::SeedMode::None);

        let err = base_options(regime, &molecule_input(None)).expect_err("R2 fail-loud");
        assert!(err.contains("molecular-uff") && err.contains("spring_len"), "{err}");
    }

    /// The vault base stays n-tuned: no YAML value can express
    /// `for_n_nodes`, so the regime overlays only what is not a function of
    /// the node count.
    #[test]
    fn vault_base_is_n_tuned() {
        let regime = regime_by_id("vault-large").unwrap();
        let small = base_options(regime, &GraphInput { n_nodes: 1_000, n_edges: 2_000, ..Default::default() }).unwrap();
        let large = base_options(regime, &GraphInput { n_nodes: 100_000, n_edges: 200_000, ..Default::default() }).unwrap();
        assert!(
            large["spring_len"].as_f64().unwrap() > small["spring_len"].as_f64().unwrap(),
            "spring_len scales with n"
        );
        assert_eq!(large["repulsion_mode"], serde_json::json!("barnes_hut"));
    }

    #[test]
    fn override_storage_prefers_multipliers() {
        // Nonzero numeric base → dimensionless multiplier.
        assert_eq!(
            override_for(Some(&serde_json::json!(50.0)), &serde_json::json!(75.0)),
            Override::Multiplier { value: 1.5 }
        );
        // Zero base cannot be scaled.
        assert_eq!(
            override_for(Some(&serde_json::json!(0.0)), &serde_json::json!(0.3)),
            Override::Absolute { value: serde_json::json!(0.3) }
        );
        // Enums and toggles are absolute by nature.
        assert_eq!(
            override_for(Some(&serde_json::json!("grid")), &serde_json::json!("bh")),
            Override::Absolute { value: serde_json::json!("bh") }
        );
    }

    #[test]
    fn effective_applies_multipliers_against_the_regime_base() {
        let regime = regime_by_id("vault-small").unwrap();
        let input = GraphInput { n_nodes: 500, n_edges: 900, ..Default::default() };
        let base = base_options(regime, &input).unwrap();
        let mut overrides = BTreeMap::new();
        overrides.insert("spring_len".to_string(), Override::Multiplier { value: 2.0 });
        overrides.insert(
            "repulsion_mode".to_string(),
            Override::Absolute { value: serde_json::json!("ns") },
        );
        overrides.insert("bogus_field".to_string(), Override::Multiplier { value: 9.0 });
        let effective = effective_options(regime, &overrides, &input).unwrap();
        assert_eq!(
            effective.options["spring_len"].as_f64().unwrap(),
            base["spring_len"].as_f64().unwrap() * 2.0
        );
        assert_eq!(effective.options["repulsion_mode"], serde_json::json!("ns"));
        assert!(effective.parked.is_empty());
        assert_eq!(effective.applied, 2, "unknown control dropped, not applied");
    }

    /// E1/M2: an override on a data-owned dimension is retained but never
    /// applied — the engine ignores `spring_len` for typed edges, so
    /// applying it would move a number that changes nothing.
    #[test]
    fn overrides_on_data_owned_dimensions_are_parked() {
        let regime = regime_by_id("molecular-uff").unwrap();
        let input = molecule_input(Some(1.33));
        let mut overrides = BTreeMap::new();
        overrides.insert("spring_len".to_string(), Override::Multiplier { value: 40.0 });
        let effective = effective_options(regime, &overrides, &input).unwrap();
        assert_eq!(effective.parked, vec!["spring_len".to_string()]);
        assert_eq!(effective.applied, 0);
        assert!(
            (effective.options["spring_len"].as_f64().unwrap() - 1.33).abs() < 1e-6,
            "parked override must not scale the data-owned rest"
        );
    }

    /// Spec P2: legacy absolutes become multipliers against the vault base,
    /// values equal to the base are not overrides, and unknown legacy fields
    /// are dropped silently.
    #[test]
    fn v1_migration_divides_legacy_absolutes_by_the_vault_base() {
        let regime = regime_by_id("vault-large").unwrap();
        let input = GraphInput { n_nodes: 10_000, n_edges: 40_000, ..Default::default() };
        let base = base_options(regime, &input).unwrap();
        let base_spring = base["spring_len"].as_f64().unwrap();
        let legacy = serde_json::json!({
            "spring_len": base_spring * 1.25,
            "damping": base["damping"].as_f64().unwrap(),
            "seed_mode": "none",
            "cursor_radius": 12.0,
            "long_forgotten_knob": 3.0,
        });
        let overrides = migrate_overrides(&legacy, &base);
        match overrides.get("spring_len") {
            Some(Override::Multiplier { value }) => {
                assert!((value - 1.25).abs() < 1e-6, "legacy absolute ÷ base: {value}")
            }
            other => panic!("expected a multiplier, got {other:?}"),
        }
        assert_eq!(
            overrides.get("seed_mode"),
            Some(&Override::Absolute { value: serde_json::json!("none") })
        );
        assert!(!overrides.contains_key("damping"), "base-equal value is not an override");
        assert!(!overrides.contains_key("cursor_radius"), "cursor pose is host state");
        assert!(!overrides.contains_key("long_forgotten_knob"), "unknown field dropped");
    }

    /// A pin survives only while its regime still matches the loaded graph
    /// (spec §4) — otherwise a vault pin would put 53-ångström springs on a
    /// molecule.
    #[test]
    fn pin_is_dropped_when_its_regime_stops_matching() {
        let pinned = LayoutStateV2 {
            regime_id: Some("vault-large".to_string()),
            overrides: BTreeMap::new(),
        };
        let big_vault = state(0.0, 5_000);
        assert_eq!(active_regime(&pinned, &big_vault, "gpu-force").id, "vault-large");
        let molecule = state(1.0, 24);
        assert_eq!(
            active_regime(&pinned, &molecule, "gpu-force").id,
            "molecular-uff",
            "a pin that no longer applies falls back to resolution"
        );
    }

    /// Intents are dimensionless multipliers over the base, expanded
    /// through the regime's declared `maps_to` exponents. Settle raises the
    /// halt threshold while easing cooling — one knob, two honest targets.
    #[test]
    fn intent_multiplier_expands_through_maps_to() {
        let regime = regime_by_id("vault-small").unwrap();
        let input = GraphInput { n_nodes: 400, n_edges: 900, ..Default::default() };
        let base = base_options(regime, &input).unwrap();
        let mut overrides = BTreeMap::new();
        overrides.insert("settle".to_string(), Override::Multiplier { value: 2.0 });
        overrides.insert("spread".to_string(), Override::Multiplier { value: 0.5 });
        let effective = effective_options(regime, &overrides, &input).unwrap();
        assert_eq!(
            effective.options["energy_threshold"].as_f64().unwrap(),
            base["energy_threshold"].as_f64().unwrap() * 2.0,
            "exponent 1.0 scales with the multiplier"
        );
        assert_eq!(
            effective.options["spring_len"].as_f64().unwrap(),
            base["spring_len"].as_f64().unwrap() * 0.5,
            "spread maps onto spring_len"
        );
        let cooling = effective.options["cooling_alpha"].as_f64().unwrap();
        assert!(
            cooling < base["cooling_alpha"].as_f64().unwrap(),
            "exponent -1.0 moves cooling the other way: {cooling}"
        );
        assert_eq!(effective.applied, 2);
    }

    /// An intent may not drive an option outside the range the engine can
    /// use: damping above 1.0 would add energy every step.
    #[test]
    fn intents_are_clamped_to_engine_ranges() {
        let yaml = "id: extreme\nschema_version: 1\nlabel: Extreme\nengine: gpu-force\nexecution: live\napplicability: {}\noptions: {}\ncontrols:\n  - { id: wild, kind: multiplier, label: Wild, range: [0.5, 8.0], maps_to: { damping: 1.0, cooling_alpha: 1.0 } }\n";
        let regime = &load_registry(&[("extreme.yaml", yaml)])[0];
        let input = GraphInput { n_nodes: 100, n_edges: 100, ..Default::default() };
        let mut overrides = BTreeMap::new();
        overrides.insert("wild".to_string(), Override::Multiplier { value: 8.0 });
        let effective = effective_options(regime, &overrides, &input).unwrap();
        assert_eq!(effective.options["damping"].as_f64().unwrap(), 0.999);
        assert_eq!(effective.options["cooling_alpha"].as_f64().unwrap(), 1.0);
    }

    /// Toggle intents select between the two option values the regime
    /// declares; with no override the view reflects what the sim is running.
    #[test]
    fn toggle_intent_selects_declared_option_values() {
        let regime = regime_by_id("molecular-uff").unwrap();
        let input = molecule_input(Some(1.33));
        let base = base_options(regime, &input).unwrap();
        let views = intent_views(regime, &BTreeMap::new(), &input, &base);
        let keep = views.iter().find(|v| v.id == "keep_authored").expect("toggle declared");
        assert!(keep.on, "molecular base runs seed_mode: none");

        let mut overrides = BTreeMap::new();
        overrides.insert(
            "keep_authored".to_string(),
            Override::Absolute { value: serde_json::json!(false) },
        );
        let effective = effective_options(regime, &overrides, &input).unwrap();
        assert_eq!(effective.options["seed_mode"], serde_json::json!("random"));
    }

    /// M2/M3: a control that could only move a data-owned dimension is not
    /// constructed — on typed geometry the Spread intent does not exist,
    /// and a stored Spread override is parked rather than applied.
    #[test]
    fn data_owned_intents_are_not_constructed() {
        let molecular = regime_by_id("molecular-uff").unwrap();
        let input = molecule_input(Some(1.33));
        let base = base_options(molecular, &input).unwrap();
        let molecular_views = intent_views(molecular, &BTreeMap::new(), &input, &base);
        let ids: Vec<&str> = molecular_views.iter().map(|v| v.id.as_str()).collect();
        assert_eq!(ids, ["repulsion", "keep_authored"], "no geometry-scale knob (E1)");

        let vault = regime_by_id("vault-small").unwrap();
        let vault_input = GraphInput { n_nodes: 300, n_edges: 800, ..Default::default() };
        let vault_base = base_options(vault, &vault_input).unwrap();
        let vault_views = intent_views(vault, &BTreeMap::new(), &vault_input, &vault_base);
        let vault_ids: Vec<&str> = vault_views.iter().map(|v| v.id.as_str()).collect();
        assert_eq!(vault_ids, ["repulsion", "spread", "stiffness", "settle"]);

        // The same stored Spread override: live on the vault graph, parked
        // on the molecule.
        let mut overrides = BTreeMap::new();
        overrides.insert("spread".to_string(), Override::Multiplier { value: 1.5 });
        assert_eq!(
            effective_options(vault, &overrides, &vault_input).unwrap().applied,
            1
        );
        let molecular_effective = effective_options(molecular, &overrides, &input).unwrap();
        assert_eq!(molecular_effective.parked, vec!["spread".to_string()]);
        assert_eq!(molecular_effective.applied, 0);
    }

    #[test]
    fn mean_rest_ignores_untyped_zeroes() {
        assert_eq!(mean_typed_rest(&[0.0, 2.0, 0.0, 4.0]), Some(3.0));
        assert_eq!(mean_typed_rest(&[0.0, 0.0]), None);
    }

    /// Regimes are per-engine: a one-shot solver regime resolves for its own
    /// engine and does not shadow the gpu-force resolution, and vice versa.
    #[test]
    fn resolution_is_scoped_to_the_running_engine() {
        let vault = state(0.0, 400);
        assert_eq!(resolve_for("gpu-force", &vault).id, "vault-small");
        assert_eq!(resolve_for("fcose", &vault).id, "fcose-quality");
        // An engine no regime targets falls back to the gpu-force catch-all,
        // and the panel checks `engine_has_regime` before rendering claims.
        assert!(engine_has_regime("fcose"));
        assert!(!engine_has_regime("dagre"));
    }

    /// R1/UI6: a one-shot regime declares no live-sim option, so the panel
    /// has no cooling/damping/halt rows to render for it.
    #[test]
    fn one_shot_regime_declares_no_live_sim_options() {
        let regime = regime_by_id("fcose-quality").expect("registered");
        assert_eq!(regime.execution, Execution::OneShot);
        let options = regime.options.as_ref().expect("local engine regime");
        for field in ["cooling_alpha", "cooling_floor", "energy_threshold", "damping"] {
            assert!(
                !options.keys().any(|k| k.as_str() == Some(field)),
                "one-shot regime must not declare {field} (R1)"
            );
        }
        let bad = "id: bad-one-shot\nschema_version: 1\nlabel: Bad\nengine: fcose\nexecution: one_shot\napplicability: {}\noptions:\n  damping: 0.5\n";
        assert!(
            std::panic::catch_unwind(|| load_registry(&[("bad.yaml", bad)])).is_err(),
            "a one-shot regime declaring a live-sim option is a loud boot error"
        );
    }

    /// An engine other than gpu-force gets its manifest by projecting its own
    /// default settings, so its declared dimensions are exactly the fields it
    /// accepts — with control kinds inferred, and no invented applicability.
    #[test]
    fn non_gpu_engines_project_their_own_settings_into_a_manifest() {
        let dims = engine_manifest("fcose");
        let ids: Vec<&str> = dims.iter().map(|d| d.id.as_str()).collect();
        for field in ["node_repulsion", "ideal_edge_length", "quality", "seed"] {
            assert!(ids.contains(&field), "projected manifest must cover {field}: {ids:?}");
        }
        let quality = dims.iter().find(|d| d.id == "quality").unwrap();
        assert_eq!(quality.control, "enum", "string-valued field → enum");
        assert_eq!(quality.label, "Quality", "label humanized from the field name");
        assert!(
            dims.iter().all(|d| d.not_when.is_none() && d.min_nodes.is_none()),
            "a projection claims no applicability: nothing is known"
        );
    }

    /// The one-shot regime's quality enum maps the selected option onto the
    /// engine's own field (spec §2 `maps_to`), so the control never invents
    /// an iteration count the engine derives itself.
    #[test]
    fn quality_choice_maps_onto_the_engine_field() {
        let regime = regime_by_id("fcose-quality").unwrap();
        let input = GraphInput { n_nodes: 200, n_edges: 400, ..Default::default() };
        let mut overrides = BTreeMap::new();
        // An enum control stores the selected option INDEX; the regime's
        // declared array turns it into the value the engine accepts.
        overrides.insert(
            "quality".to_string(),
            Override::Absolute { value: serde_json::json!(2) },
        );
        let effective = effective_options(regime, &overrides, &input).unwrap();
        assert_eq!(effective.options["quality"], serde_json::json!("proof"));
        assert_eq!(effective.applied, 1);
        // An explicit declared value is kept verbatim, so a share link or a
        // hand-edited store still resolves.
        overrides.insert(
            "quality".to_string(),
            Override::Absolute { value: serde_json::json!("draft") },
        );
        let explicit = effective_options(regime, &overrides, &input).unwrap();
        assert_eq!(explicit.options["quality"], serde_json::json!("draft"));
    }

    /// The settings a one-shot regime installs must be settings the engine
    /// can actually run: the panel's Solve path deserializes this block into
    /// the engine's own settings struct and solves with it.
    #[test]
    fn one_shot_regime_settings_run_on_the_engine() {
        let regime = regime_by_id("fcose-quality").unwrap();
        let input = GraphInput { n_nodes: 8, n_edges: 8, ..Default::default() };
        let effective = effective_options(regime, &BTreeMap::new(), &input).unwrap();
        let settings: graph_layouts::FcoseSettings =
            serde_json::from_value(effective.options.clone())
                .expect("regime-installed settings deserialize into FcoseSettings");
        let _ = settings;
        // And the solver accepts the very same JSON through its dyn entry
        // point, which is what `run_static_solve` calls.
        use graph_layouts::{BoxedStatic, DynStaticLayout, Edge, FcoseLayout, Graph, Node};
        let mut graph = Graph::default();
        for i in 0..8u32 {
            graph
                .nodes
                .insert(i.to_string(), Node::default());
        }
        for i in 0..7u32 {
            graph.edges.insert(
                format!("e{i}"),
                Edge {
                    source: i.to_string(),
                    target: (i + 1).to_string(),
                    ..Default::default()
                },
            );
        }
        let layout: Box<dyn DynStaticLayout> = Box::new(BoxedStatic::<FcoseLayout>::new());
        let positions = layout
            .solve_dyn(&effective.options, &graph)
            .expect("the engine solves with the regime's settings");
        assert_eq!(positions.len(), 8 * 3, "one xyz per node");
        assert!(
            positions.iter().all(|v| v.is_finite()),
            "solver returned non-finite coordinates"
        );
    }

    #[test]
    fn dim_ownership_fires_only_at_full_coverage() {
        let spring_len = manifest_dim("gpu-force", "spring_len").unwrap();
        assert!(dim_owned_by_data(&spring_len, 25, 25), "full coverage → capsule");
        assert!(!dim_owned_by_data(&spring_len, 13, 25), "partial → governs untyped");
        assert!(!dim_owned_by_data(&spring_len, 0, 25), "untyped → live");
    }
}
