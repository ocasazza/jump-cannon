//! Layout panel — Dioxus port of crates/graph-renderer/src/ui/sections/layout.rs
//! (+ ui/layout/registry.rs, ui/layout/algorithms/*, ui/sections/seed.rs — the
//! seed UI is embedded at the bottom of the Layout section, same as egui).
//!
//! One engine gallery split by backend, switched from the Settings panel
//! header (This Device / Compute Cluster — the Layout surface is a Settings
//! tab, so the segmented switch rides the panel's header-actions slot):
//!   * **This Device** — every non-bridge layout (the static solvers + the
//!     local GPU physics sim) as rich cards. Selecting a card sets the
//!     active id directly.
//!   * **Compute Cluster** — a live worker status card (○/◐/● health dot,
//!     reconnect, refresh) above the engines advertised by the
//!     graph-compute worker via `GET /compute/engines`. Each engine card
//!     routes through one of the two "bridge" layouts and patches that
//!     bridge's settings so the versioned `PUT /compute/layout` selects the
//!     worker once; WebSocket open/reconnect carries only graph + selection
//!     identity and never mutates shared worker state.
//!
//! Bridge routing (worker engine id → bridge + settings patch):
//!   * `geometric`     → active `"geometric"`, LensConfig.use_gpu=false
//!   * `geometric-gpu` → active `"geometric"`, LensConfig.use_gpu=true
//!   * everything else → active `"remote-fa2"`, RemoteFa2Settings.layout_id=<id>
//!
//! The engine "registry" is the match tables below (`local_descriptors` /
//! `build_static` / `build_physics` / `default_settings`) — the algorithms
//! themselves live in graph_layouts, the same crate the egui registry built
//! its factories from, so the position math is byte-identical.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use dioxus::prelude::*;
use gloo_storage::{LocalStorage, Storage};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use graph_layouts::geometric::{ClassLens, CoordinationLens, EdgeLengthLens, LensConfig, MassLens};
use graph_layouts::{
    BoxedPhysics, BoxedStatic, CircleAxis, CircleLayout, CircleSettings, CiseLayout, CiseSettings,
    ConcentricLayout, ConcentricMetric, ConcentricSettings, CoseBilkentLayout, CoseBilkentSettings,
    DagreLayout, DagreRanker, DagreSettings, DynPhysicsLayout, DynStaticLayout, FcoseLayout,
    FcoseQuality, FcoseSettings, GpuForceLayout, GpuForceOptions, Graph, GridLayout, GridSettings,
    HilbertLayout, HilbertSettings, KlayLayout, KlaySettings, LayoutDescriptor, LayoutKind,
    LayoutRequirements, PhysicsLayout, RandomLayout, RandomSettings, RankDirection, RepulsionMode,
    SpectralLayout, SpectralSettings, SphereLayout, SphereSettings, StaticLayout,
};

use crate::api::{get_json, put_json, put_raw_json};
use crate::render;
use crate::Ctx;

const STORE_KEY: &str = "jc_layout_v1";

/// The two registry ids that are "bridges" to the remote worker rather
/// than real local layouts. Excluded from the Local group and surfaced
/// (with the worker's own engine names) under Remote.
const BRIDGE_GEOMETRIC: &str = "geometric";
const BRIDGE_REMOTE_FA2: &str = "remote-fa2";

// --- panel-local state ---------------------------------------------------------

/// Mirror of the egui `SeedStrategy` (ui/state.rs). `BuiltIn(i)` indexes
/// `tvix_wasm::seed_demos()` (via [`builtin_demo_indices`]).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
enum SeedStrategy {
    #[default]
    None,
    BuiltIn(usize),
    Custom,
}

/// Persisted panel state — `LayoutState` (active + JSON-keyed settings bag)
/// plus the seed picker, stored under one localStorage key.
#[derive(Clone, Serialize, Deserialize)]
struct PanelState {
    active: String,
    settings: BTreeMap<String, Value>,
    #[serde(default)]
    seed_strategy: SeedStrategy,
    #[serde(default)]
    seed_custom: String,
}

impl Default for PanelState {
    fn default() -> Self {
        Self {
            active: "gpu-force".to_string(),
            settings: BTreeMap::new(),
            seed_strategy: SeedStrategy::None,
            seed_custom: String::new(),
        }
    }
}

fn load_state() -> PanelState {
    LocalStorage::get(STORE_KEY).unwrap_or_default()
}

fn persist(s: &PanelState) {
    // Attribute the auto-snapshot — egui sections/layout.rs stamps
    // `snapshot_source = Some("Layout")`.
    crate::appstate::note_source("Layout");
    let _ = LocalStorage::set(STORE_KEY, s);
}

static STATE: GlobalSignal<PanelState> = Signal::global(load_state);
/// Latest `/compute/engines` result. `None` until the first fetch; the inner
/// `Result` distinguishes a server error from a successful "no worker" view.
static COMPUTE: GlobalSignal<Option<Result<ComputeEngines, String>>> = Signal::global(|| None);
static HEALTH: GlobalSignal<Option<ComputeHealth>> = Signal::global(|| None);
/// What the last apply pushed — the swap/short-circuit detector (mirrors
/// `prev_layout_key` / `prev_active_layout_id` / `prev_seed_mode` on the
/// egui App). `generation` ties it to one render-host build: a canvas
/// remount reboots the host into the default gpu-force layout, so a stale
/// key must not suppress the re-apply.
#[derive(Clone, PartialEq)]
struct AppliedKey {
    generation: u64,
    active: String,
    json: Value,
}

static PREV_APPLIED: GlobalSignal<Option<AppliedKey>> = Signal::global(|| None);
static SOLVE_MSG: GlobalSignal<String> = Signal::global(String::new);
static SEED_STATUS: GlobalSignal<Option<String>> = Signal::global(|| None);
static SEED_ERROR: GlobalSignal<Option<String>> = Signal::global(|| None);
/// The `(engine_id, settings)` that were last actually *solved* by a static
/// engine. Unlike [`PREV_APPLIED`] (which the poll loop refreshes on every
/// settings push), this only advances when a real solve runs — so the panel
/// can tell when a static engine has un-applied slider edits pending.
static LAST_SOLVED: GlobalSignal<Option<(String, Value)>> = Signal::global(|| None);
static CONTROL_REQUEST_TOKEN: GlobalSignal<u64> = Signal::global(|| 0);
static CONTROL_BUSY: GlobalSignal<bool> = Signal::global(|| false);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RemoteLease {
    graph_revision: u64,
    selection_generation: u64,
}

static REMOTE_LEASE_VIEW: GlobalSignal<Option<RemoteLease>> = Signal::global(|| None);

// --- /compute wire types (FROZEN CONTRACT — see graph-api/src/server.rs) --------

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
struct ComputeEngines {
    #[serde(default)]
    connected: bool,
    #[serde(default)]
    #[allow(dead_code)] // broker-side selection; the picker derives its own
    active: String,
    #[serde(default)]
    engines: Vec<EngineInfo>,
    #[serde(default)]
    selection_generation: u64,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
struct EngineInfo {
    id: String,
    display_name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    kind: String,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
struct ComputeHealth {
    connected: bool,
    url: String,
    #[serde(default)]
    worker_ok: bool,
    #[serde(default)]
    requested_layout_id: String,
    #[serde(default)]
    effective_layout_id: String,
    #[serde(default)]
    fallback_reason: Option<String>,
    #[serde(default)]
    worker_status_error: Option<String>,
    #[serde(default)]
    selection_generation: u64,
    #[serde(default)]
    graph_revision: u64,
}

#[derive(Debug, PartialEq, Eq)]
struct HealthView {
    class: &'static str,
    text: String,
}

fn worker_health_view(health: Option<&ComputeHealth>) -> HealthView {
    let Some(health) = health else {
        return HealthView {
            class: "lay-health",
            text: "compute worker — (no /compute/health yet)".into(),
        };
    };
    if !health.connected {
        return HealthView {
            class: "lay-health bad",
            text: format!("compute worker ○ disconnected — {}", health.url),
        };
    }
    if let Some(error) = health.worker_status_error.as_deref() {
        return HealthView {
            class: "lay-health bad",
            text: format!("compute worker ◐ connected, status unavailable — {error}"),
        };
    }
    if !health.worker_ok {
        return HealthView {
            class: "lay-health bad",
            text: "compute worker ◐ connected, not ready".into(),
        };
    }
    HealthView {
        class: "lay-health ok",
        text: format!("compute worker ● ready — {}", health.url),
    }
}

fn remote_solver_status(
    health: Option<&ComputeHealth>,
    lease: Option<RemoteLease>,
    control_busy: bool,
) -> String {
    if control_busy {
        return "selecting engine…".into();
    }
    let Some(lease) = lease else {
        return "not attached".into();
    };
    let Some(health) = health else {
        return format!(
            "attached at selection g{} · awaiting worker status",
            lease.selection_generation
        );
    };
    if health.graph_revision != lease.graph_revision
        || health.selection_generation != lease.selection_generation
    {
        return format!(
            "refreshing status for graph r{} · selection g{}",
            lease.graph_revision, lease.selection_generation
        );
    }
    if !health.connected {
        return "disconnected".into();
    }
    if let Some(error) = health.worker_status_error.as_deref() {
        return format!("worker status unavailable: {error}");
    }
    if !health.worker_ok {
        return "worker not ready".into();
    }

    let requested = health.requested_layout_id.trim();
    let effective = health.effective_layout_id.trim();
    if effective.is_empty() {
        return format!(
            "requested {} · no effective engine reported · selection g{}",
            if requested.is_empty() { "unknown" } else { requested },
            lease.selection_generation
        );
    }
    if !requested.is_empty() && requested != effective {
        let reason = health
            .fallback_reason
            .as_deref()
            .filter(|reason| !reason.trim().is_empty())
            .unwrap_or("worker fallback");
        return format!(
            "requested {requested} → effective {effective} · fallback: {reason} · selection g{}",
            lease.selection_generation
        );
    }
    if let Some(reason) = health
        .fallback_reason
        .as_deref()
        .filter(|reason| !reason.trim().is_empty())
    {
        return format!(
            "effective {effective} · {reason} · selection g{}",
            lease.selection_generation
        );
    }
    format!(
        "effective {effective} · selection g{}",
        lease.selection_generation
    )
}

async fn fetch_compute() {
    let r = get_json::<ComputeEngines>("/compute/engines").await;
    let should_sync = r.as_ref().map(|v| v.connected).unwrap_or(false) && active_is_remote();
    if let Ok(view) = &r {
        KNOWN_SELECTION_GENERATION.with(|g| g.set(view.selection_generation));
    }
    *COMPUTE.write() = Some(r);
    match get_json::<ComputeHealth>("/compute/health").await {
        Ok(h) => store_health(Some(h)),
        Err(_) => store_health(None),
    }
    if should_sync && expected_stream_lease().is_none() {
        request_remote_selection();
    }
}

#[derive(Clone, Debug, Serialize)]
struct ComputeLayoutPutReq {
    layout_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    lens: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    expected_generation: Option<u64>,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct ComputeLayoutPutResp {
    ok: bool,
    #[serde(default)]
    changed: bool,
    #[serde(default)]
    graph_revision: u64,
    #[serde(default)]
    selection_generation: u64,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct InitialPlacementResp {
    ok: bool,
    #[serde(default)]
    n_nodes: u32,
    #[serde(default)]
    graph_revision: u64,
    #[serde(default)]
    selection_generation: u64,
    #[serde(default)]
    error: Option<String>,
}

fn validate_placement_response(
    response: &InitialPlacementResp,
    expected: RemoteLease,
    n_nodes: usize,
) -> Result<(), String> {
    if !response.ok {
        return Err(response.error.clone().unwrap_or_else(|| "initial placement rejected".into()));
    }
    if response.n_nodes as usize != n_nodes {
        return Err(format!("worker placed {} nodes; expected {n_nodes}", response.n_nodes));
    }
    if response.graph_revision != expected.graph_revision {
        return Err(format!(
            "placement graph revision {} != active revision {}",
            response.graph_revision, expected.graph_revision
        ));
    }
    if response.selection_generation != expected.selection_generation {
        return Err(format!(
            "placement changed selection generation {} -> {}",
            expected.selection_generation, response.selection_generation
        ));
    }
    Ok(())
}

fn positions_le_bytes(positions: &[f32], n_nodes: usize) -> Result<Vec<u8>, String> {
    let expected_components = n_nodes
        .checked_mul(3)
        .ok_or_else(|| "node count is too large for a placement".to_string())?;
    if positions.len() != expected_components {
        return Err(format!(
            "placement has {} components; expected {expected_components} for {n_nodes} nodes",
            positions.len()
        ));
    }
    let mut bytes = Vec::with_capacity(positions.len() * 4);
    for (i, value) in positions.iter().copied().enumerate() {
        if !value.is_finite() {
            return Err(format!("position component {i} is not finite"));
        }
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    Ok(bytes)
}

// --- engine registry -------------------------------------------------------------

/// Same order the egui `LayoutRegistry::seed_default` registers, minus the
/// two bridges (they live under Remote). Computed once — the descriptor set
/// is static for the lifetime of the app.
fn local_descriptors() -> &'static [LayoutDescriptor] {
    use std::sync::LazyLock;
    static DESCRIPTORS: LazyLock<Vec<LayoutDescriptor>> = LazyLock::new(|| {
        vec![
            <GpuForceLayout as PhysicsLayout>::descriptor(),
            <RandomLayout as StaticLayout>::descriptor(),
            <CircleLayout as StaticLayout>::descriptor(),
            <GridLayout as StaticLayout>::descriptor(),
            <SphereLayout as StaticLayout>::descriptor(),
            <ConcentricLayout as StaticLayout>::descriptor(),
            <HilbertLayout as StaticLayout>::descriptor(),
            <SpectralLayout as StaticLayout>::descriptor(),
            <FcoseLayout as StaticLayout>::descriptor(),
            <CoseBilkentLayout as StaticLayout>::descriptor(),
            <CiseLayout as StaticLayout>::descriptor(),
            <DagreLayout as StaticLayout>::descriptor(),
            <KlayLayout as StaticLayout>::descriptor(),
        ]
    });
    &DESCRIPTORS
}

fn descriptor_for(id: &str) -> Option<LayoutDescriptor> {
    match id {
        BRIDGE_REMOTE_FA2 => Some(<RemoteFa2Layout as PhysicsLayout>::descriptor()),
        BRIDGE_GEOMETRIC => Some(<RemoteGeometricLayout as PhysicsLayout>::descriptor()),
        other => local_descriptors().iter().find(|d| d.id == other).cloned(),
    }
}

fn default_settings(id: &str) -> Value {
    let v = match id {
        "gpu-force" => serde_json::to_value(GpuForceOptions::default()),
        "random" => serde_json::to_value(RandomSettings::default()),
        "circle" => serde_json::to_value(CircleSettings::default()),
        "grid" => serde_json::to_value(GridSettings::default()),
        "sphere" => serde_json::to_value(SphereSettings::default()),
        "concentric" => serde_json::to_value(ConcentricSettings::default()),
        "hilbert" => serde_json::to_value(HilbertSettings::default()),
        "spectral" => serde_json::to_value(SpectralSettings::default()),
        "fcose" => serde_json::to_value(FcoseSettings::default()),
        "cose_bilkent" => serde_json::to_value(CoseBilkentSettings::default()),
        "cise" => serde_json::to_value(CiseSettings::default()),
        "dagre" => serde_json::to_value(DagreSettings::default()),
        "klay" => serde_json::to_value(KlaySettings::default()),
        BRIDGE_REMOTE_FA2 => serde_json::to_value(RemoteFa2Settings::default()),
        BRIDGE_GEOMETRIC => serde_json::to_value(default_lens()),
        _ => Ok(Value::Null),
    };
    v.unwrap_or(Value::Null)
}

fn build_static(id: &str) -> Option<Box<dyn DynStaticLayout>> {
    Some(match id {
        "random" => Box::new(BoxedStatic::<RandomLayout>::new()) as Box<dyn DynStaticLayout>,
        "circle" => Box::new(BoxedStatic::<CircleLayout>::new()),
        "grid" => Box::new(BoxedStatic::<GridLayout>::new()),
        "sphere" => Box::new(BoxedStatic::<SphereLayout>::new()),
        "concentric" => Box::new(BoxedStatic::<ConcentricLayout>::new()),
        "hilbert" => Box::new(BoxedStatic::<HilbertLayout>::new()),
        "spectral" => Box::new(BoxedStatic::<SpectralLayout>::new()),
        "fcose" => Box::new(BoxedStatic::<FcoseLayout>::new()),
        "cose_bilkent" => Box::new(BoxedStatic::<CoseBilkentLayout>::new()),
        "cise" => Box::new(BoxedStatic::<CiseLayout>::new()),
        "dagre" => Box::new(BoxedStatic::<DagreLayout>::new()),
        "klay" => Box::new(BoxedStatic::<KlayLayout>::new()),
        _ => return None,
    })
}

fn build_physics(id: &str, json: &Value) -> Option<Box<dyn DynPhysicsLayout>> {
    Some(match id {
        "gpu-force" => {
            let opts: GpuForceOptions = serde_json::from_value(json.clone()).unwrap_or_default();
            Box::new(BoxedPhysics::new(GpuForceLayout::new(opts))) as Box<dyn DynPhysicsLayout>
        }
        BRIDGE_REMOTE_FA2 => {
            let s: RemoteFa2Settings = serde_json::from_value(json.clone()).unwrap_or_default();
            Box::new(BoxedPhysics::new(RemoteFa2Layout::create(s)))
        }
        BRIDGE_GEOMETRIC => {
            let s: LensConfig =
                serde_json::from_value(json.clone()).unwrap_or_else(|_| default_lens());
            Box::new(BoxedPhysics::new(RemoteGeometricLayout::create(s)))
        }
        _ => return None,
    })
}

// --- settings access ---------------------------------------------------------------

fn settings_or_default(id: &str) -> Value {
    STATE
        .read()
        .settings
        .get(id)
        .cloned()
        .unwrap_or_else(|| default_settings(id))
}

fn typed_settings<T: Serialize + DeserializeOwned + Default>(id: &str) -> T {
    // Cache the last deserialized value per engine id. The cache key is the
    // JSON pointer (which is stable as long as the settings map entry isn't
    // replaced), so we only pay the serde cost when settings actually change.
    thread_local! {
        static CACHE: std::cell::RefCell<std::collections::HashMap<String, (String, String)>> =
            std::cell::RefCell::new(std::collections::HashMap::new());
    }
    let raw = settings_or_default(id);
    let raw_str = raw.to_string();
    CACHE.with(|c| {
        let mut cache = c.borrow_mut();
        if let Some((cached_raw, cached_serialized)) = cache.get(id) {
            if *cached_raw == raw_str {
                // Same JSON — reuse the cached deserialized value.
                return serde_json::from_str(cached_serialized).unwrap_or_default();
            }
        }
        // New or changed — deserialize and cache.
        let val: T = serde_json::from_value(raw).unwrap_or_default();
        if let Ok(serialized) = serde_json::to_string(&val) {
            cache.insert(id.to_string(), (raw_str, serialized));
        }
        val
    })
}

/// Write one engine's settings block, persist, and push to the GPU.
fn put_settings(id: &str, v: Value) {
    let mut st = STATE.read().clone();
    st.settings.insert(id.to_string(), v);
    persist(&st);
    *STATE.write() = st;
    if active_is_remote() && STATE.peek().active == id {
        request_remote_selection();
    } else {
        apply_engine(false);
    }
}

fn save_settings<T: Serialize>(id: &str, s: &T) {
    if let Ok(v) = serde_json::to_value(s) {
        put_settings(id, v);
    }
}

/// Decode-mutate-store helper — the Dioxus stand-in for the egui pattern of
/// round-tripping the JSON block through the typed settings struct.
fn edit<T: Serialize + DeserializeOwned + Default>(id: &str, f: impl FnOnce(&mut T)) {
    let mut s: T = typed_settings(id);
    f(&mut s);
    save_settings(id, &s);
}

fn set_active(id: &str) {
    let mut st = STATE.read().clone();
    st.active = id.to_string();
    persist(&st);
    *STATE.write() = st;
    apply_engine(false);
}

/// Route a worker engine id to the appropriate bridge and patch that
/// bridge's persisted settings. The control-plane PUT below is the sole
/// selection mutation; WebSocket open/reconnect is read-only.
fn select_remote_engine(engine_id: &str) {
    let mut st = STATE.read().clone();
    match engine_id {
        BRIDGE_GEOMETRIC | "geometric-gpu" => {
            let want_gpu = engine_id == "geometric-gpu";
            st.active = BRIDGE_GEOMETRIC.to_string();
            let json = st
                .settings
                .entry(BRIDGE_GEOMETRIC.to_string())
                .or_insert_with(|| default_settings(BRIDGE_GEOMETRIC));
            let mut cfg: LensConfig =
                serde_json::from_value(json.clone()).unwrap_or_else(|_| default_lens());
            cfg.use_gpu = want_gpu;
            // use_multilevel is user-controlled via the geometric UI checkbox
            // (default false); selecting the engine preserves that choice.
            if let Ok(v) = serde_json::to_value(&cfg) {
                *json = v;
            }
        }
        other => {
            st.active = BRIDGE_REMOTE_FA2.to_string();
            let json = st
                .settings
                .entry(BRIDGE_REMOTE_FA2.to_string())
                .or_insert_with(|| default_settings(BRIDGE_REMOTE_FA2));
            let mut cfg: RemoteFa2Settings =
                serde_json::from_value(json.clone()).unwrap_or_default();
            cfg.layout_id = other.to_string();
            if let Ok(v) = serde_json::to_value(&cfg) {
                *json = v;
            }
        }
    }
    persist(&st);
    *STATE.write() = st;
    request_remote_selection();
}

fn desired_remote_selection() -> Option<ComputeLayoutPutReq> {
    let st = STATE.peek().clone();
    match st.active.as_str() {
        BRIDGE_GEOMETRIC => {
            let lens: LensConfig = st
                .settings
                .get(BRIDGE_GEOMETRIC)
                .cloned()
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_else(default_lens);
            Some(ComputeLayoutPutReq {
                layout_id: if lens.use_gpu { "geometric-gpu" } else { "geometric" }.into(),
                params: None,
                lens: serde_json::to_value(lens).ok(),
                expected_generation: None,
            })
        }
        BRIDGE_REMOTE_FA2 => {
            let settings: RemoteFa2Settings = st
                .settings
                .get(BRIDGE_REMOTE_FA2)
                .cloned()
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_default();
            Some(ComputeLayoutPutReq {
                layout_id: settings.layout_id,
                params: None,
                lens: None,
                expected_generation: None,
            })
        }
        _ => None,
    }
}

/// Serialize one desired remote selection through the versioned singleton
/// control API. A rapid newer edit supersedes this task; if its first request
/// observes a stale generation it retries the same latest intent against the
/// generation returned by the server.
fn request_remote_selection() {
    let Some(mut request) = desired_remote_selection() else { return };
    let Some(graph_revision) = EXPECTED_GRAPH_REVISION.with(std::cell::Cell::get) else {
        *SOLVE_MSG.write() = "remote layout unavailable: graph has no server revision".into();
        return;
    };
    let token = CONTROL_REQUEST_TOKEN.peek().wrapping_add(1);
    *CONTROL_REQUEST_TOKEN.write() = token;
    *CONTROL_BUSY.write() = true;
    set_stream_lease(None);
    wasm_bindgen_futures::spawn_local(async move {
        let mut expected = KNOWN_SELECTION_GENERATION.with(std::cell::Cell::get);
        for _ in 0..3 {
            request.expected_generation = (expected != 0).then_some(expected);
            let response = put_json::<_, ComputeLayoutPutResp>("/compute/layout", &request).await;
            if *CONTROL_REQUEST_TOKEN.peek() != token {
                return;
            }
            match response {
                Ok(resp) => {
                    if resp.selection_generation != 0 {
                        KNOWN_SELECTION_GENERATION.with(|g| g.set(resp.selection_generation));
                    }
                    if resp.ok {
                        if resp.graph_revision != graph_revision || resp.selection_generation == 0 {
                            *SOLVE_MSG.write() = format!(
                                "remote selection identity mismatch (graph r{}, generation {})",
                                resp.graph_revision, resp.selection_generation
                            );
                            break;
                        }
                        set_stream_lease(Some(RemoteLease {
                            graph_revision: resp.graph_revision,
                            selection_generation: resp.selection_generation,
                        }));
                        *PREV_APPLIED.write() = None;
                        *SOLVE_MSG.write() = if resp.changed {
                            format!("worker selection generation {}", resp.selection_generation)
                        } else {
                            format!("worker selection unchanged at generation {}", resp.selection_generation)
                        };
                        apply_engine(false);
                        break;
                    }
                    if resp.selection_generation != 0 && resp.selection_generation != expected {
                        expected = resp.selection_generation;
                        continue;
                    }
                    *SOLVE_MSG.write() = resp.error.unwrap_or_else(|| "remote selection rejected".into());
                    break;
                }
                Err(e) => {
                    *SOLVE_MSG.write() = format!("remote selection failed: {e}");
                    break;
                }
            }
        }
        if *CONTROL_REQUEST_TOKEN.peek() == token {
            *CONTROL_BUSY.write() = false;
        }
    });
}

/// One worker connection (URL + reconnect backoff) shared by every remote
/// engine. Both bridges keep their own `url`/`reconnect_backoff_ms` on the
/// wire, so this writes through to both settings blocks at once — the panel
/// exposes a single "worker" row instead of duplicating the fields per engine.
fn set_worker_connection(url: Option<String>, reconnect_ms: Option<u32>) {
    edit::<RemoteFa2Settings>(BRIDGE_REMOTE_FA2, |s| {
        if let Some(u) = &url {
            s.url = u.clone();
        }
        if let Some(r) = reconnect_ms {
            s.reconnect_backoff_ms = r;
        }
    });
    edit::<LensConfig>(BRIDGE_GEOMETRIC, |c| {
        if let Some(u) = &url {
            c.url = u.clone();
        }
        if let Some(r) = reconnect_ms {
            c.reconnect_backoff_ms = r;
        }
    });
}

/// Current worker URL (both bridges are kept in sync by
/// [`set_worker_connection`]; read the fa2 block as the canonical copy).
fn worker_url() -> String {
    typed_settings::<RemoteFa2Settings>(BRIDGE_REMOTE_FA2).url
}

fn worker_reconnect_ms() -> u32 {
    typed_settings::<RemoteFa2Settings>(BRIDGE_REMOTE_FA2).reconnect_backoff_ms
}

/// Force the active engine to rebuild from scratch: clearing the applied key
/// makes the next [`apply_engine`] treat it as a first-apply, which for the
/// remote bridges tears down and reopens the WebSocket stream (a "restart"),
/// and for local engines re-seeds/re-solves. The single primary-action button
/// routes remote engines here so "Restart stream" actually reconnects.
fn restart_active() {
    *PREV_APPLIED.write() = None;
    apply_engine(true);
}

/// Select the geometric-gpu engine and stage the self-assembly regime that
/// matches a `/compute/soup` morphology, so the Layout panel reflects (and can
/// keep tuning) what the Generate panel's "Assemble" just started on the
/// worker. Keeps the two self-assembly surfaces from silently disagreeing.
pub(crate) fn stage_self_assembly(
    morphology: &str,
    graph_revision: u64,
    selection_generation: u64,
) {
    let preset = match morphology {
        "chains" => Some(SelfAssemblyPreset::LipidChain),
        "sheet" => Some(SelfAssemblyPreset::HoneycombSheet),
        "tube" => Some(SelfAssemblyPreset::Tube),
        "vesicle" => Some(SelfAssemblyPreset::Vesicle),
        _ => None,
    };
    let mut st = STATE.peek().clone();
    st.active = BRIDGE_GEOMETRIC.to_string();
    let json = st
        .settings
        .entry(BRIDGE_GEOMETRIC.to_string())
        .or_insert_with(|| default_settings(BRIDGE_GEOMETRIC));
    let mut cfg: LensConfig =
        serde_json::from_value(json.clone()).unwrap_or_else(|_| default_lens());
    cfg.use_gpu = true;
    if let Some(preset) = preset {
        preset.apply_to(&mut cfg);
    }
    if let Ok(value) = serde_json::to_value(cfg) {
        *json = value;
    }
    persist(&st);
    *STATE.write() = st;
    KNOWN_SELECTION_GENERATION.with(|g| g.set(selection_generation));
    if EXPECTED_GRAPH_REVISION.with(std::cell::Cell::get) == Some(graph_revision)
        && graph_revision != 0
        && selection_generation != 0
    {
        set_stream_lease(Some(RemoteLease { graph_revision, selection_generation }));
        *PREV_APPLIED.write() = None;
        apply_engine(false);
    } else {
        *SOLVE_MSG.write() = "self-assembly control identity does not match the loaded graph".into();
    }
}

// --- apply (the App::apply_layout_to_gpu port) ---------------------------------------

/// Push the active engine + settings onto the live render host. `solve_requested`
/// is the one-shot Solve/Wake channel: Static layouts re-solve, Physics layouts
/// `wake()` (egui shares the flag the same way).
fn apply_engine(solve_requested: bool) {
    let Some(n_nodes) = render::with_host(|h| h.pipes.n_nodes() as usize) else {
        if solve_requested {
            *SOLVE_MSG.write() = "renderer not mounted — open the Graph panel first".to_string();
        }
        return;
    };

    // Lazy-init missing settings. gpu-force gets size-tuned defaults
    // (`for_n_nodes`) so spring_len/repulsion match the loaded graph size
    // instead of the dense-ball anchor defaults.
    {
        let mut st = STATE.read().clone();
        let active = st.active.clone();
        if !st.settings.contains_key(&active) {
            let initial = if active == "gpu-force" {
                serde_json::to_value(GpuForceOptions::for_n_nodes(n_nodes))
                    .unwrap_or_else(|_| default_settings(&active))
            } else {
                default_settings(&active)
            };
            st.settings.insert(active, initial);
            persist(&st);
            *STATE.write() = st;
        }
    }

    let st = STATE.read().clone();
    let active = st.active.clone();
    let mut json = st.settings.get(&active).cloned().unwrap_or(Value::Null);

    // gpu-force: derive repulsion_radius from spring_len (4×, the legacy
    // rule) and cap steps_per_call so persisted high values can't pin the
    // GPU — both ported from apply_layout_to_gpu.
    if active == "gpu-force" {
        if let Ok(mut o) = serde_json::from_value::<GpuForceOptions>(json.clone()) {
            o.repulsion_radius = (4.0 * o.spring_len).max(1.0);
            const MAX_STEPS_PER_CALL: u32 = 16;
            if o.steps_per_call > MAX_STEPS_PER_CALL {
                o.steps_per_call = MAX_STEPS_PER_CALL;
            }
            json = serde_json::to_value(&o).unwrap_or(json);
        }
    }

    // A stale-generation key is treated as "never applied" — the rebuilt
    // host is running the boot gpu-force layout regardless of what we
    // pushed before the remount.
    let generation = render::mount_generation();
    let prev = PREV_APPLIED
        .peek()
        .clone()
        .filter(|p| p.generation == generation);
    // Same key + no pending Solve/Wake → nothing to do (the egui
    // `prev_layout_key` short-circuit; lets the poll loop call this freely).
    if !solve_requested
        && prev
            .as_ref()
            .map(|p| p.active == active && p.json == json)
            .unwrap_or(false)
    {
        return;
    }
    let active_changed = prev.as_ref().map(|p| p.active != active).unwrap_or(false);
    // `set_options` doesn't re-precompute, so a seed-mode change can't take
    // effect through a plain settings push — force a swap.
    let seed_mode = json.get("seed_mode").cloned();
    let seed_mode_changed = match &prev {
        Some(p) if p.active == active && active == "gpu-force" => {
            p.json.get("seed_mode") != seed_mode.as_ref()
        }
        _ => false,
    };
    // First apply against a fresh host: the host always boots gpu-force, so
    // any other persisted engine must swap in rather than settings-push.
    let boot_mismatch = prev.is_none() && active != "gpu-force";

    let Some(desc) = descriptor_for(&active) else {
        *SOLVE_MSG.write() = format!("no engine registered for id {active:?}");
        return;
    };

    let result: Result<(), String> = render::with_host(|h| match desc.kind {
        LayoutKind::Physics => {
            if active_changed || seed_mode_changed || boot_mismatch {
                match build_physics(&active, &json) {
                    Some(l) => h.swap_physics_layout(l),
                    None => return Err(format!("no physics builder for {active:?}")),
                }
            } else {
                h.pipes.set_physics_layout_settings_json(&json);
            }
            if solve_requested {
                h.pipes.wake_physics_layout();
            }
            Ok(())
        }
        LayoutKind::Static => {
            // Solve when the algorithm just changed to a static backend, or
            // when the Solve button was pressed. Settings-only edits don't
            // auto-solve — the user hits Solve to commit them.
            if active_changed || solve_requested || boot_mismatch {
                match build_static(&active) {
                    Some(l) => {
                        let r = h.run_static_solve(l.as_ref(), &json);
                        if r.is_ok() {
                            *LAST_SOLVED.write() = Some((active.clone(), json.clone()));
                        }
                        r
                    }
                    None => Err(format!("no static solver for {active:?}")),
                }
            } else {
                Ok(())
            }
        }
    })
    .unwrap_or(Ok(()));

    *SOLVE_MSG.write() = result.err().unwrap_or_default();
    *PREV_APPLIED.write() = Some(AppliedKey {
        generation,
        active,
        json,
    });
}

// --- gpu-force presets (port of ui/layout/algorithms/gpu_force.rs) -------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
enum LayoutPreset {
    Fast,
    #[default]
    Balanced,
    Pretty,
}

impl LayoutPreset {
    const ALL: [LayoutPreset; 3] = [
        LayoutPreset::Fast,
        LayoutPreset::Balanced,
        LayoutPreset::Pretty,
    ];

    fn label(self) -> &'static str {
        match self {
            LayoutPreset::Fast => "Fast",
            LayoutPreset::Balanced => "Balanced",
            LayoutPreset::Pretty => "Pretty",
        }
    }

    fn apply_to(self, o: &mut GpuForceOptions) {
        match self {
            LayoutPreset::Fast => {
                o.repulsion = 150.0;
                o.spring_k = 0.10;
                o.spring_len = 40.0;
                o.gravity = 0.005;
                o.damping = 0.85;
                o.dt = 0.045;
                o.steps_per_call = 1;
                o.cooling_alpha = 0.99;
                o.cooling_floor = 0.65;
                o.energy_threshold = 0.5;
            }
            LayoutPreset::Balanced => {
                o.repulsion = 250.0;
                o.spring_k = 0.06;
                o.spring_len = 60.0;
                o.gravity = 0.003;
                o.damping = 0.92;
                o.dt = 0.04;
                o.steps_per_call = 2;
                o.cooling_alpha = 0.999;
                o.cooling_floor = 0.85;
                o.energy_threshold = 0.005;
            }
            LayoutPreset::Pretty => {
                o.repulsion = 400.0;
                o.spring_k = 0.05;
                o.spring_len = 80.0;
                o.gravity = 0.002;
                o.damping = 0.92;
                o.dt = 0.025;
                o.steps_per_call = 4;
                o.cooling_alpha = 0.999;
                o.cooling_floor = 0.55;
                o.energy_threshold = 0.02;
            }
        }
    }

    /// Best-effort guess of which preset produced this options block —
    /// purely for highlighting the active preset button.
    fn detect(o: &GpuForceOptions) -> Option<Self> {
        for p in Self::ALL {
            let mut probe = GpuForceOptions::default();
            p.apply_to(&mut probe);
            if (probe.repulsion - o.repulsion).abs() < 0.001
                && (probe.spring_k - o.spring_k).abs() < 0.0001
                && (probe.spring_len - o.spring_len).abs() < 0.001
                && (probe.dt - o.dt).abs() < 0.0001
                && probe.steps_per_call == o.steps_per_call
            {
                return Some(p);
            }
        }
        None
    }
}

// --- remote bridges (ports of remote_fa2.rs / geometric.rs renderer layers) ----------

/// Derive the stream URL from the configured graph-api base (the egui wasm
/// build derives it from window.origin; the Dioxus app has an explicit,
/// user-configurable server URL instead).
fn default_stream_url() -> String {
    let base = crate::api::server_url();
    let ws = if let Some(rest) = base.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = base.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        format!("ws://{base}")
    };
    format!("{ws}/graph/layout/stream")
}

fn versioned_stream_url(base: &str) -> Option<String> {
    let lease = expected_stream_lease()?;
    let sep = if base.contains('?') { '&' } else { '?' };
    Some(format!(
        "{base}{sep}graph_revision={}&selection_generation={}",
        lease.graph_revision, lease.selection_generation
    ))
}

fn default_lens() -> LensConfig {
    let mut c = LensConfig::default();
    c.url = default_stream_url();
    c
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct RemoteFa2Settings {
    url: String,
    reconnect_backoff_ms: u32,
    /// Worker engine id selected through the authoritative control PUT.
    #[serde(default = "default_layout_id")]
    layout_id: String,
}

fn default_layout_id() -> String {
    "fa2-bh".to_string()
}

impl Default for RemoteFa2Settings {
    fn default() -> Self {
        Self {
            url: default_stream_url(),
            reconnect_backoff_ms: 1000,
            layout_id: default_layout_id(),
        }
    }
}

#[derive(Debug)]
struct RemoteFrame {
    graph_revision: u64,
    selection_generation: u64,
    positions: Vec<f32>,
}

/// Shared latch — the WS consumer task drops the latest decoded frame here;
/// `step_with_encoder` validates its topology identity before GPU upload.
type Latch = Arc<Mutex<Option<RemoteFrame>>>;

thread_local! {
    static EXPECTED_GRAPH_REVISION: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
    static EXPECTED_SELECTION_GENERATION: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
    static KNOWN_SELECTION_GENERATION: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

fn set_stream_lease(lease: Option<RemoteLease>) {
    EXPECTED_SELECTION_GENERATION.with(|g| g.set(lease.map(|l| l.selection_generation)));
    *REMOTE_LEASE_VIEW.write() = lease;
}

/// Set by the graph-session owner on every replacement. `None` intentionally
/// disables remote frames for browser-only and legacy/unidentified graphs.
pub(crate) fn set_expected_graph_revision(revision: Option<u64>) {
    EXPECTED_GRAPH_REVISION.with(|r| r.set(revision.filter(|v| *v != 0)));
    set_stream_lease(None);
}

fn frame_matches_expected(
    graph_revision: u64,
    selection_generation: u64,
    expected: Option<RemoteLease>,
) -> bool {
    expected == Some(RemoteLease { graph_revision, selection_generation })
        && graph_revision != 0
        && selection_generation != 0
}

fn expected_stream_lease() -> Option<RemoteLease> {
    let graph_revision = EXPECTED_GRAPH_REVISION.with(std::cell::Cell::get)?;
    let selection_generation = EXPECTED_SELECTION_GENERATION.with(std::cell::Cell::get)?;
    Some(RemoteLease { graph_revision, selection_generation })
}

fn frame_matches_active_graph(graph_revision: u64, selection_generation: u64) -> bool {
    frame_matches_expected(graph_revision, selection_generation, expected_stream_lease())
}

/// Wire format (matches graph-api's ws_handler):
/// `[u64 graph_revision][u64 selection_generation][u64 frame][u32 n][xyz]`
fn parse_frame(bytes: &[u8]) -> Option<(u64, u64, u64, u32, Vec<f32>)> {
    if bytes.len() < 28 {
        return None;
    }
    let graph_revision = u64::from_le_bytes(bytes[0..8].try_into().ok()?);
    let selection_generation = u64::from_le_bytes(bytes[8..16].try_into().ok()?);
    let frame = u64::from_le_bytes(bytes[16..24].try_into().ok()?);
    let n = u32::from_le_bytes(bytes[24..28].try_into().ok()?);
    let body = &bytes[28..];
    if body.len() != (n as usize) * 12 {
        return None;
    }
    // Copy via from_le_bytes — `body` comes from a Vec<u8>, alignment isn't
    // guaranteed for a bytemuck cast.
    let mut out = Vec::with_capacity((n as usize) * 3);
    for chunk in body.chunks_exact(4) {
        out.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }
    Some((graph_revision, selection_generation, frame, n, out))
}

fn spawn_ws_consumer(url: String, backoff_ms: u32, latch: Latch) {
    wasm_bindgen_futures::spawn_local(async move {
        ws_consumer_loop(url, backoff_ms, latch).await;
    });
}

async fn ws_consumer_loop(url: String, base_backoff_ms: u32, latch: Latch) {
    use futures::StreamExt;
    use gloo_net::websocket::{futures::WebSocket, Message};

    let mut backoff = base_backoff_ms.max(100) as u64;
    loop {
        // The bridge layout owning the other Arc half was dropped (engine
        // swap / stream URL change) — stop instead of reconnecting forever.
        if Arc::strong_count(&latch) <= 1 {
            return;
        }
        match WebSocket::open(&url) {
            Ok(ws) => {
                tracing::info!("[layout] stream connected: {url}");
                backoff = base_backoff_ms.max(100) as u64;
                let (_sink, mut stream) = ws.split();
                while let Some(msg) = stream.next().await {
                    match msg {
                        Ok(Message::Bytes(bytes)) => {
                            if let Some((graph_revision, selection_generation, _frame, _n, positions)) =
                                parse_frame(&bytes)
                            {
                                if !frame_matches_active_graph(graph_revision, selection_generation) {
                                    continue;
                                }
                                if let Ok(mut g) = latch.lock() {
                                    *g = Some(RemoteFrame {
                                        graph_revision,
                                        selection_generation,
                                        positions,
                                    });
                                }
                            }
                        }
                        Ok(Message::Text(_)) => continue,
                        Err(_) => break,
                    }
                }
                tracing::warn!("[layout] stream closed; reconnect in {backoff}ms");
            }
            Err(e) => {
                tracing::warn!("[layout] connect {url} failed: {e:?}; retry in {backoff}ms");
            }
        }
        gloo_timers::future::TimeoutFuture::new(backoff as u32).await;
        backoff = (backoff.saturating_mul(2)).min(30_000);
    }
}

/// Generic remote bridge: consumes the versioned `/graph/layout/stream` and
/// writes the latched frames into the shared positions buffer each tick.
/// No compute pass — purely a "remote sink".
struct RemoteFa2Layout {
    settings: RemoteFa2Settings,
    latch: Latch,
    n_nodes: u32,
    spawned_url: Option<String>,
}

impl RemoteFa2Layout {
    fn create(settings: RemoteFa2Settings) -> Self {
        Self {
            settings,
            latch: Arc::new(Mutex::new(None)),
            n_nodes: 0,
            spawned_url: None,
        }
    }

    fn stream_url(&self) -> Option<String> {
        versioned_stream_url(&self.settings.url)
    }

    /// (Re)spawn the WS consumer when the effective URL changed. Called from
    /// both init and step so a newly-issued selection generation reaches a
    /// fresh read-only stream even when the bridge id itself did not change.
    /// Replacing the latch drops the old consumer's other Arc and ends it.
    fn ensure_stream(&mut self) {
        if self.n_nodes == 0 {
            return;
        }
        let Some(url) = self.stream_url() else {
            if self.spawned_url.take().is_some() {
                self.latch = Arc::new(Mutex::new(None));
            }
            return;
        };
        if self.spawned_url.as_deref() == Some(url.as_str()) {
            return;
        }
        self.spawned_url = Some(url.clone());
        self.latch = Arc::new(Mutex::new(None));
        let backoff_ms = self.settings.reconnect_backoff_ms.max(100);
        spawn_ws_consumer(url, backoff_ms, Arc::clone(&self.latch));
    }
}

impl PhysicsLayout for RemoteFa2Layout {
    type Settings = RemoteFa2Settings;

    fn descriptor() -> LayoutDescriptor {
        LayoutDescriptor {
            id: BRIDGE_REMOTE_FA2,
            kind: LayoutKind::Physics,
            display_name: "Remote (compute)",
            description: "Stream positions from the remote graph-compute worker over WebSocket. \
                 Engine selection happens once through graph-api's versioned control API; \
                 stream reconnects are read-only.",
            requirements: LayoutRequirements {
                needs_edges: false,
                needs_cpu_positions: false,
                needs_gpu_positions_buffer: true,
            },
        }
    }

    fn new(settings: Self::Settings) -> Self {
        Self::create(settings)
    }

    fn init_with_device(
        &mut self,
        _device: &wgpu::Device,
        _queue: &wgpu::Queue,
        graph: &Graph,
        _positions_buf: &wgpu::Buffer,
    ) -> Result<(), String> {
        self.n_nodes = graph.nodes.len() as u32;
        self.ensure_stream();
        Ok(())
    }

    fn step_with_encoder(
        &mut self,
        _device: &wgpu::Device,
        queue: &wgpu::Queue,
        _encoder: &mut wgpu::CommandEncoder,
        positions_buf: &wgpu::Buffer,
    ) {
        self.ensure_stream();
        let positions = match self.latch.lock() {
            Ok(mut g) => g.take(),
            Err(_) => return,
        };
        let Some(frame) = positions else { return };
        if !frame_matches_active_graph(frame.graph_revision, frame.selection_generation) {
            return;
        }
        // Frames whose n mismatches the local topology are dropped — guards
        // against a worker that hasn't picked up the same graph load yet.
        if frame.positions.len() == 3 * (self.n_nodes as usize) && self.n_nodes > 0 {
            queue.write_buffer(positions_buf, 0, bytemuck::cast_slice(&frame.positions));
        }
    }

    fn set_settings(&mut self, settings: Self::Settings) {
        self.settings = settings;
    }
    fn settings(&self) -> &Self::Settings {
        &self.settings
    }
}

/// Geometric-engine bridge: same versioned stream sink. The resolved lens and
/// CPU/GPU choice are applied once through the control-plane PUT.
struct RemoteGeometricLayout {
    settings: LensConfig,
    latch: Latch,
    n_nodes: u32,
    spawned_url: Option<String>,
}

impl RemoteGeometricLayout {
    fn create(settings: LensConfig) -> Self {
        Self {
            settings,
            latch: Arc::new(Mutex::new(None)),
            n_nodes: 0,
            spawned_url: None,
        }
    }

    fn stream_url(&self) -> Option<String> {
        versioned_stream_url(&self.settings.url)
    }

    fn ensure_stream(&mut self) {
        if self.n_nodes == 0 {
            return;
        }
        let Some(url) = self.stream_url() else {
            if self.spawned_url.take().is_some() {
                self.latch = Arc::new(Mutex::new(None));
            }
            return;
        };
        if self.spawned_url.as_deref() == Some(url.as_str()) {
            return;
        }
        self.spawned_url = Some(url.clone());
        self.latch = Arc::new(Mutex::new(None));
        let backoff_ms = self.settings.reconnect_backoff_ms.max(100);
        spawn_ws_consumer(url, backoff_ms, Arc::clone(&self.latch));
    }
}

impl PhysicsLayout for RemoteGeometricLayout {
    type Settings = LensConfig;

    fn descriptor() -> LayoutDescriptor {
        LayoutDescriptor {
            id: BRIDGE_GEOMETRIC,
            kind: LayoutKind::Physics,
            display_name: "Geometric Engine",
            description: "Remote geometric constraint solver via graph-api. Supports both CPU \
                          and GPU-accelerated backends.",
            requirements: LayoutRequirements {
                needs_edges: false,
                needs_cpu_positions: false,
                needs_gpu_positions_buffer: true,
            },
        }
    }

    fn new(settings: Self::Settings) -> Self {
        Self::create(settings)
    }

    fn init_with_device(
        &mut self,
        _device: &wgpu::Device,
        _queue: &wgpu::Queue,
        graph: &Graph,
        _positions_buf: &wgpu::Buffer,
    ) -> Result<(), String> {
        self.n_nodes = graph.nodes.len() as u32;
        self.ensure_stream();
        Ok(())
    }

    fn step_with_encoder(
        &mut self,
        _device: &wgpu::Device,
        queue: &wgpu::Queue,
        _encoder: &mut wgpu::CommandEncoder,
        positions_buf: &wgpu::Buffer,
    ) {
        // Settings-only updates (CPU↔GPU toggle, lens edits) change the
        // effective URL without a layout swap — respawn here.
        self.ensure_stream();
        let positions = match self.latch.lock() {
            Ok(mut g) => g.take(),
            Err(_) => return,
        };
        let Some(frame) = positions else { return };
        if !frame_matches_active_graph(frame.graph_revision, frame.selection_generation) {
            return;
        }
        if frame.positions.len() == 3 * (self.n_nodes as usize) && self.n_nodes > 0 {
            queue.write_buffer(positions_buf, 0, bytemuck::cast_slice(&frame.positions));
        }
    }

    fn set_settings(&mut self, settings: Self::Settings) {
        self.settings = settings;
    }
    fn settings(&self) -> &Self::Settings {
        &self.settings
    }
}

// --- geometric presets (port of geometric.rs LensPreset / SelfAssemblyPreset) --------

#[derive(Clone, Copy, Debug, PartialEq)]
enum LensPreset {
    CrystallizeMotifs,
    SeparateCommunities,
    CorePeriphery,
    Molecular,
}

impl LensPreset {
    const ALL: [LensPreset; 4] = [
        LensPreset::CrystallizeMotifs,
        LensPreset::SeparateCommunities,
        LensPreset::CorePeriphery,
        LensPreset::Molecular,
    ];

    fn label(self) -> &'static str {
        match self {
            LensPreset::CrystallizeMotifs => "Crystallize motifs",
            LensPreset::SeparateCommunities => "Separate communities",
            LensPreset::CorePeriphery => "Core–periphery",
            LensPreset::Molecular => "Molecular",
        }
    }

    fn apply_to(self, c: &mut LensConfig) {
        // Integrator triple validated on the real vault (9,724 nodes / 48k
        // edges, graph-layout-stability harness): the engine defaults
        // (dt=1, damping=0.9) overshoot at vault stiffness (K·dt² ≫ 2) and
        // saturate the max_step clamp into a ±10 flip-flop. dt=0.1 /
        // damping=0.6 decays monotonically on BOTH engines (geometric-gpu on
        // the vault: median step 4.9 → 0.04 over 600 ticks; the stiffer CPU
        // engine limit-cycles at dt≥0.25 but settles to ~0.01 here).
        // Preset-only — engine defaults stay golden-pinned.
        c.time_step = 0.1;
        c.damping = 0.6;
        c.max_step = 10.0;
        match self {
            LensPreset::CrystallizeMotifs => {
                c.class = ClassLens::Uniform;
                c.coordination = CoordinationLens::Degree;
                c.angle_stiffness = 0.5;
            }
            LensPreset::SeparateCommunities => {
                c.class = ClassLens::Louvain;
                c.affinity_strength = -50.0;
                c.angle_stiffness = 0.01;
                // De-hairball: let global shortcut edges stretch so communities
                // can pull apart (small-world layout fix).
                c.edge_length = EdgeLengthLens::JaccardStrength;
                c.edge_strength_spread = 3.0;
            }
            LensPreset::CorePeriphery => {
                c.mass = MassLens::PageRank;
                c.gravity = 0.05;
            }
            LensPreset::Molecular => {
                c.angle_stiffness = 1.0;
            }
        }
    }
}

/// Self-assembly example presets — each sets `bonding_enabled` plus the
/// PARAMETER REGIME validated in graph-compute's `geometric_solver.rs`
/// canaries. Chains/sheets form spontaneously; tube/vesicle closure needs a
/// seeded disk (see the egui source for the full honesty note).
#[derive(Clone, Copy, Debug, PartialEq)]
enum SelfAssemblyPreset {
    LipidChain,
    HoneycombSheet,
    Tube,
    Vesicle,
}

impl SelfAssemblyPreset {
    const ALL: [SelfAssemblyPreset; 4] = [
        SelfAssemblyPreset::LipidChain,
        SelfAssemblyPreset::HoneycombSheet,
        SelfAssemblyPreset::Tube,
        SelfAssemblyPreset::Vesicle,
    ];

    fn label(self) -> &'static str {
        match self {
            SelfAssemblyPreset::LipidChain => "Lipid chains (valence 2)",
            SelfAssemblyPreset::HoneycombSheet => "Honeycomb sheet (valence 3 @120°)",
            SelfAssemblyPreset::Tube => "Tube (curved sheet)",
            SelfAssemblyPreset::Vesicle => "Vesicle (rim seam + curvature)",
        }
    }

    fn tooltip(self) -> &'static str {
        match self {
            SelfAssemblyPreset::LipidChain => {
                "P2 valence-2 @180°. Spontaneous from a cohering Brownian soup."
            }
            SelfAssemblyPreset::HoneycombSheet => {
                "P2 valence-3 @120° + membrane flattening (anisotropy + GB-side + \
                 tilt). Honeycomb patches form spontaneously."
            }
            SelfAssemblyPreset::Tube => {
                "Sheet regime + spontaneous curvature → a rolled tube. Seed a \
                 disk/sheet; full closure is a kinetic trap (logged)."
            }
            SelfAssemblyPreset::Vesicle => {
                "P3 rim line-tension (γ=4) + curvature (c₀=0.5): folds a seeded \
                 bonded disk toward a shell. Spontaneous soup→vesicle not reached \
                 (logged) — seed a disk."
            }
        }
    }

    fn apply_to(self, c: &mut LensConfig) {
        // Common dynamic-bond + Brownian-soup base (validated soup_settings
        // regime: σ=1.0 contact, cohesion well, thermostat).
        c.bonding_enabled = true;
        c.exclusion_strength = 1.0;
        c.gravity = 0.1;
        c.r_bond = 1.1; // just past contact σ=1.0
        c.r_break = 1.5; // ≈1.36·r_bond hysteresis band
        c.bond_stiffness = 0.4;
        c.bond_every = 4;
        c.well_depth = 2.0;
        c.well_width = 1.0;
        c.temperature = 0.2;
        // Membrane terms default OFF; sheet/tube/vesicle regimes turn them on.
        c.anisotropy_strength = 0.0;
        c.gb_side_strength = 0.0;
        c.tilt_coupling_strength = 0.0;
        c.spont_curvature = 0.0;
        c.line_tension = 0.0;

        match self {
            SelfAssemblyPreset::LipidChain => {
                c.default_max_valence = 2;
                c.default_bond_angle = 180.0;
                c.angle_stiffness = 0.15;
            }
            SelfAssemblyPreset::HoneycombSheet => {
                c.default_max_valence = 3;
                c.default_bond_angle = 120.0;
                c.angle_stiffness = 0.3;
                c.well_depth = 2.5;
                c.anisotropy_strength = 1.0;
                c.gb_side_strength = 1.5;
                c.tilt_coupling_strength = 1.0;
                c.spont_curvature = 0.0; // flat target
                c.gravity = 0.05;
            }
            SelfAssemblyPreset::Tube => {
                c.default_max_valence = 3;
                c.default_bond_angle = 120.0;
                c.angle_stiffness = 0.3;
                c.well_depth = 2.5;
                c.anisotropy_strength = 1.0;
                c.gb_side_strength = 1.5;
                c.tilt_coupling_strength = 1.0;
                c.spont_curvature = 0.25; // intermediate c₀ → tube curvature
                c.gravity = 0.05;
            }
            SelfAssemblyPreset::Vesicle => {
                c.default_max_valence = 3;
                c.default_bond_angle = 120.0;
                c.angle_stiffness = 0.3;
                c.well_depth = 2.5;
                c.anisotropy_strength = 1.0;
                c.gb_side_strength = 1.5;
                c.tilt_coupling_strength = 1.0;
                c.spont_curvature = 0.5; // validated closure curvature
                c.line_tension = 4.0; // validated rim seam strength
                c.gravity = 0.05;
            }
        }
    }
}

// --- initial seed (port of ui/sections/seed.rs) --------------------------------------

/// Indices into `tvix_wasm::seed_demos()` that are exposed as first-class
/// "built-in" strategies in the picker. The catalog also ships a "No seed" and
/// a "Custom (flat line)" entry, but those are surfaced via the dedicated
/// `No seed` and `Custom (Nix)` picker options instead, so they are excluded
/// here by name (verbatim `seed.rs::builtin_demo_indices`).
fn builtin_demo_indices() -> Vec<usize> {
    tvix_wasm::seed_demos()
        .iter()
        .enumerate()
        .filter(|(_, d)| d.name != "No seed" && !d.name.starts_with("Custom"))
        .map(|(i, _)| i)
        .collect()
}

/// Write the gpu-force layout's `seed_mode` so the running sim treats the
/// current/applied positions correctly: `keep == true` ("No seed", or right
/// after an explicit Apply) sets `"none"` so the re-init the settings change
/// provokes skips the buffer upload; `keep == false` restores `"random"`.
fn set_gpu_force_seed_mode(keep: bool) {
    let mode = if keep { "none" } else { "random" };
    let n = render::with_host(|h| h.pipes.n_nodes() as usize).unwrap_or(0);
    let mut st = STATE.read().clone();
    let entry = st
        .settings
        .entry("gpu-force".to_string())
        .or_insert_with(|| {
            serde_json::to_value(GpuForceOptions::for_n_nodes(n))
                .unwrap_or_else(|_| serde_json::json!({}))
        });
    if let Some(obj) = entry.as_object_mut() {
        obj.insert("seed_mode".to_string(), serde_json::json!(mode));
    }
    persist(&st);
    *STATE.write() = st;
    apply_engine(false);
}

fn set_seed_strategy(s: SeedStrategy) {
    let is_none = matches!(s, SeedStrategy::None);
    let mut st = STATE.read().clone();
    st.seed_strategy = s;
    persist(&st);
    *STATE.write() = st;
    *SEED_ERROR.write() = None;
    *SEED_STATUS.write() = None;
    if is_none {
        // "No seed" = keep the current buffer through future re-inits.
        set_gpu_force_seed_mode(true);
    }
}

fn apply_builtin_seed(i: usize) {
    let expr = tvix_wasm::seed_demos()
        .get(i)
        .map(|d| d.expr.to_string())
        .unwrap_or_default();
    apply_seed_expr(&expr);
}

/// Evaluate a seed expression via `tvix_wasm::eval_seed` against the live node
/// count and write the result into the GPU positions buffer — the egui
/// `seed.rs::apply_button` + `set_pending` semantics, with the renderer host
/// standing in for the `state.seed.pending` → `App::update` drain:
///   * `Ok(non-empty)` applies the positions and flips the gpu-force
///     `seed_mode` to `"none"` so they survive the sim re-init,
///   * `Ok(empty)` is the "no seed" sentinel — status only, nothing applied,
///   * `Err` lands in the panel's error chrome.
/// The eval runs synchronously on the click handler, the same cost the egui
/// app paid (its seed path never went through a background job).
fn apply_seed_expr(expr: &str) {
    let Some(n) = render::with_host(|h| h.pipes.n_nodes() as usize) else {
        *SEED_ERROR.write() = Some("renderer not mounted — open the Graph panel first".to_string());
        return;
    };
    match tvix_wasm::eval_seed(expr, n) {
        Ok(positions) => {
            *SEED_ERROR.write() = None;
            if positions.is_empty() {
                *SEED_STATUS.write() = Some("no seed applied (empty result)".to_string());
                return;
            }
            let flat: Vec<f32> = positions.into_iter().flatten().collect();
            if active_is_remote() {
                apply_remote_initial_placement(flat, n);
            } else {
                match render::with_host(|h| h.apply_positions(&flat)) {
                    Some(Ok(())) => {
                        *SEED_STATUS.write() = Some(format!("applied {n} positions locally"));
                        // Keep the just-applied positions through the sim re-init.
                        set_gpu_force_seed_mode(true);
                    }
                    Some(Err(e)) => *SEED_ERROR.write() = Some(e),
                    None => *SEED_ERROR.write() = Some("renderer not mounted".to_string()),
                }
            }
        }
        Err(err) => *SEED_ERROR.write() = Some(err),
    }
}

fn apply_remote_initial_placement(positions: Vec<f32>, n_nodes: usize) {
    let Some(expected) = expected_stream_lease() else {
        *SEED_ERROR.write() = Some(
            "remote placement unavailable: select a worker engine for this graph first".into(),
        );
        return;
    };
    let bytes = match positions_le_bytes(&positions, n_nodes) {
        Ok(bytes) => bytes,
        Err(error) => {
            *SEED_ERROR.write() = Some(error);
            return;
        }
    };
    *SEED_STATUS.write() = Some("sending starting placement to the remote solver…".into());
    *SEED_ERROR.write() = None;
    wasm_bindgen_futures::spawn_local(async move {
        let path = format!(
            "/compute/initial-placement?graph_revision={}",
            expected.graph_revision
        );
        let response = put_raw_json::<InitialPlacementResp>(&path, bytes).await;
        if expected_stream_lease() != Some(expected) {
            *SEED_STATUS.write() = None;
            *SEED_ERROR.write() = Some("graph or worker selection changed while placing nodes".into());
            return;
        }
        match response.and_then(|r| {
            validate_placement_response(&r, expected, n_nodes)?;
            Ok(r)
        }) {
            Ok(_) => match render::with_host(|h| h.apply_positions(&positions)) {
                Some(Ok(())) => {
                    *SEED_STATUS.write() = Some(format!(
                        "remote solver accepted {n_nodes} starting positions; view updated"
                    ));
                    *SEED_ERROR.write() = None;
                }
                Some(Err(error)) => {
                    *SEED_STATUS.write() = None;
                    *SEED_ERROR.write() = Some(format!(
                        "worker accepted placement, but the view could not update: {error}"
                    ));
                }
                None => {
                    *SEED_STATUS.write() = None;
                    *SEED_ERROR.write() = Some(
                        "worker accepted placement, but the Graph panel is not mounted".into(),
                    );
                }
            },
            Err(error) => {
                *SEED_STATUS.write() = None;
                *SEED_ERROR.write() = Some(error);
            }
        }
    });
}

/// Minimal "No seed" placement for a freshly generated graph: a small
/// deterministic jitter so nodes aren't coincident (degenerate), WITHOUT the
/// big pre-spread sphere — the force sim builds the layout from here. Radius
/// is a few units, growing slowly with `n` so a large graph isn't
/// pathologically dense (verbatim `generate.rs::jitter_positions`).
fn jitter_positions(n: usize) -> Vec<f32> {
    let r = 2.0 + (n as f32).max(1.0).cbrt();
    let mut out = vec![0.0f32; 3 * n];
    for (i, slot) in out.iter_mut().enumerate() {
        // SplitMix64 finaliser on the index → deterministic unit in [0,1).
        let mut z = (i as u64).wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        let unit = (z >> 40) as f32 / (1u64 << 24) as f32;
        *slot = (unit * 2.0 - 1.0) * r;
    }
    out
}

/// Resolve the INITIAL positions for a freshly generated graph of `n` nodes
/// from this panel's active Initial-seed strategy — instead of always applying
/// the default sphere shell. Port of the egui
/// `crates/graph-renderer/src/generate.rs::seed_positions_for` (called by the
/// Generate panel, the `App::drain_generated_graph` contract):
///   - `None` → a minimal jitter (the sim arranges from there),
///   - `BuiltIn(i)` → the embedded `seed_demos()[i]` strategy via `eval_seed`,
///   - `Custom` → the user's Nix seed expression via `eval_seed`.
/// Any eval failure / wrong-length result falls back to the jitter.
pub(crate) fn seed_positions_for_generated(n: usize) -> Vec<f32> {
    let st = STATE.read().clone();
    let expr: Option<String> = match st.seed_strategy {
        SeedStrategy::None => None,
        SeedStrategy::BuiltIn(i) => tvix_wasm::seed_demos().get(i).map(|d| d.expr.to_string()),
        SeedStrategy::Custom => Some(st.seed_custom),
    };
    match expr {
        None => jitter_positions(n),
        Some(src) => match tvix_wasm::eval_seed(&src, n) {
            Ok(p) if p.len() == n => p.into_iter().flatten().collect(),
            _ => jitter_positions(n),
        },
    }
}

// --- shared widgets -------------------------------------------------------------------

fn fmt_f(v: f64) -> String {
    format!("{}", v as f32)
}

/// Slider row: range input (optionally log-mapped, like egui's
/// `.logarithmic(true)`) + a number box for direct entry.
#[component]
fn Slider(
    label: String,
    min: f64,
    max: f64,
    value: f64,
    on: EventHandler<f64>,
    log: Option<bool>,
    title: Option<String>,
) -> Element {
    let log = log.unwrap_or(false);
    let pos = if log {
        1000.0 * (value.max(min) / min).ln() / (max / min).ln()
    } else {
        value
    };
    let (rmin, rmax, rstep) = if log {
        (0.0, 1000.0, 1.0)
    } else {
        (min, max, ((max - min) / 1000.0).max(1e-6))
    };
    rsx! {
        div { class: "lay-row", title: title.unwrap_or_default(),
            span { class: "lay-k", "{label}" }
            input {
                r#type: "range",
                class: "lay-range",
                min: "{rmin}",
                max: "{rmax}",
                step: "{rstep}",
                value: "{pos}",
                oninput: move |e| {
                    if let Ok(p) = e.value().parse::<f64>() {
                        let v = if log { min * (max / min).powf(p / 1000.0) } else { p };
                        on.call(v);
                    }
                },
            }
            input {
                r#type: "number",
                class: "lay-num",
                value: "{fmt_f(value)}",
                onchange: move |e| {
                    if let Ok(v) = e.value().parse::<f64>() {
                        on.call(v.clamp(min, max));
                    }
                },
            }
        }
    }
}

/// Integer drag-value stand-in (egui `DragValue`): a clamped number box.
#[component]
fn IntRow(
    label: String,
    min: i64,
    max: i64,
    value: i64,
    on: EventHandler<i64>,
    title: Option<String>,
    note: Option<String>,
) -> Element {
    rsx! {
        div { class: "lay-row", title: title.unwrap_or_default(),
            span { class: "lay-k", "{label}" }
            input {
                r#type: "number",
                class: "lay-num",
                min: "{min}",
                max: "{max}",
                step: "1",
                value: "{value}",
                onchange: move |e| {
                    if let Ok(v) = e.value().parse::<i64>() {
                        on.call(v.clamp(min, max));
                    }
                },
            }
            if let Some(n) = note {
                span { class: "lay-note", "{n}" }
            }
        }
    }
}

#[component]
fn CheckRow(
    label: String,
    value: bool,
    on: EventHandler<bool>,
    text: Option<String>,
    title: Option<String>,
) -> Element {
    rsx! {
        div { class: "lay-row", title: title.unwrap_or_default(),
            span { class: "lay-k", "{label}" }
            label { class: "lay-check",
                input {
                    r#type: "checkbox",
                    checked: value,
                    onchange: move |e| on.call(e.checked()),
                }
                if let Some(t) = text {
                    span { "{t}" }
                }
            }
        }
    }
}

#[component]
fn TextRow(
    label: String,
    value: String,
    on: EventHandler<String>,
    title: Option<String>,
) -> Element {
    rsx! {
        div { class: "lay-row", title: title.unwrap_or_default(),
            span { class: "lay-k", "{label}" }
            input {
                r#type: "text",
                class: "lay-text",
                value: "{value}",
                onchange: move |e| on.call(e.value()),
            }
        }
    }
}

/// Seed (u64) row: re-roll button + text entry. Text, not number — re-rolled
/// seeds exceed the f64-safe integer range a number input would mangle.
#[component]
fn SeedRow(label: String, value: u64, on: EventHandler<u64>) -> Element {
    rsx! {
        div { class: "lay-row",
            span { class: "lay-k", "{label}" }
            button {
                class: "lay-btn lay-small",
                onclick: move |_| on.call(value.wrapping_mul(6364136223846793005).wrapping_add(1)),
                "re-roll"
            }
            input {
                r#type: "text",
                class: "lay-text",
                value: "{value}",
                onchange: move |e| {
                    if let Ok(v) = e.value().trim().parse::<u64>() {
                        on.call(v);
                    }
                },
            }
        }
    }
}

// --- panel ------------------------------------------------------------------------------

/// Which engine gallery the panel (and the header switch) is showing.
/// `Local` = the engines that run in this process (gpu-force physics + the
/// static solvers); `Cluster` = the engines advertised by the graph-compute
/// worker. Session-only, never persisted: the view re-joins the running
/// engine's backend whenever the engine changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Backend {
    Local,
    Cluster,
}

fn backend_of(engine_id: &str) -> Backend {
    match engine_id {
        BRIDGE_GEOMETRIC | BRIDGE_REMOTE_FA2 => Backend::Cluster,
        _ => Backend::Local,
    }
}

fn backend_label(backend: Backend) -> &'static str {
    match backend {
        Backend::Local => "this device",
        Backend::Cluster => "the compute cluster",
    }
}

static VIEW_BACKEND: GlobalSignal<Backend> = Signal::global(|| backend_of(&load_state().active));

/// Live worker status glyph for the header's Compute Cluster segment
/// (○/◐/●). Kept separate from [`HEALTH`] so the header — rendered in the
/// App scope, not the panel's — re-renders only when the glyph flips, not
/// on every 2 s health poll.
static WORKER_DOT: GlobalSignal<&'static str> = Signal::global(|| "○");

/// (glyph, css-state) for the worker health: ● ready / ◐ connected but
/// degraded / ○ disconnected or unknown.
fn worker_status(health: Option<&ComputeHealth>) -> (&'static str, &'static str) {
    match health {
        Some(h) if h.connected && h.worker_ok && h.worker_status_error.is_none() => ("●", "ok"),
        Some(h) if h.connected => ("◐", "warn"),
        _ => ("○", "off"),
    }
}

/// One-line health summary without the "compute worker" prefix — the worker
/// status card supplies its own title.
fn worker_health_sub(health: Option<&ComputeHealth>) -> String {
    let Some(h) = health else {
        return "no /compute/health yet".into();
    };
    if !h.connected {
        return format!("disconnected — {}", h.url);
    }
    if let Some(error) = h.worker_status_error.as_deref() {
        return format!("connected, status unavailable — {error}");
    }
    if !h.worker_ok {
        return "connected, not ready".into();
    }
    format!("ready — {}", h.url)
}

/// Store the latest `/compute/health` and keep [`WORKER_DOT`] in sync.
fn store_health(health: Option<ComputeHealth>) {
    let dot = worker_status(health.as_ref()).0;
    if *WORKER_DOT.peek() != dot {
        *WORKER_DOT.write() = dot;
    }
    *HEALTH.write() = health;
}

pub(crate) fn active_is_remote() -> bool {
    matches!(
        STATE.peek().active.as_str(),
        BRIDGE_GEOMETRIC | BRIDGE_REMOTE_FA2
    )
}

/// Segmented This Device / Compute Cluster backend switch, rendered into
/// the Settings panel's header-actions slot while the Layout tab is active
/// (the Layout surface is a Settings tab, so it has no panel chrome of its
/// own — main.rs mounts this there). The highlighted segment is the
/// *viewed* gallery: clicking browses the other backend without touching
/// the running engine, and the view re-joins the running engine's backend
/// on the next engine change (see the effect in [`panel`]). The cluster
/// segment carries the live worker status dot (○/◐/●).
pub(crate) fn backend_switch_header() -> Element {
    let view = *VIEW_BACKEND.read();
    let dot = *WORKER_DOT.read();
    // peek: the App-scope header must not subscribe to the full settings
    // bag (every slider drag would re-render the whole workspace).
    let running = backend_of(&STATE.peek().active);
    let health_text = worker_health_view(HEALTH.peek().as_ref()).text;
    let local_title = match running {
        Backend::Local => "Show local engines — the running engine lives here".to_string(),
        Backend::Cluster => "Show the engines that run on this device".to_string(),
    };
    let cluster_title = match running {
        Backend::Cluster => {
            format!("Show compute-cluster engines — the running engine lives here · {health_text}")
        }
        Backend::Local => format!("Show compute-cluster engines — {health_text}"),
    };
    rsx! {
        panel_kit::PanelHeaderButton {
            label: "This Device",
            title: local_title,
            active: view == Backend::Local,
            on_press: move |_| *VIEW_BACKEND.write() = Backend::Local,
        }
        panel_kit::PanelHeaderButton {
            label: format!("Compute Cluster {dot}"),
            title: cluster_title,
            active: view == Backend::Cluster,
            on_press: move |_| *VIEW_BACKEND.write() = Backend::Cluster,
        }
    }
}

pub fn panel(ctx: Ctx) -> Element {
    // Re-push the persisted engine onto the live host once per panel mount
    // (the host always boots gpu-force; PREV_APPLIED gates redundant swaps).
    use_future(|| async {
        apply_engine(false);
    });

    // Fetch the remote engine list, and KEEP RETRYING (every RETRY_SECS)
    // while it is unavailable — self-heals across graph-api cold starts /
    // broker dial races, then idles once connected. Health is polled on the
    // same cadence for the status line.
    use_future(|| async {
        const RETRY_MS: u32 = 2000;
        loop {
            // Re-apply onto a rebuilt host (canvas remount) — a no-op while
            // the applied key (generation + engine + settings) is current.
            apply_engine(false);
            let needs = {
                let snap = COMPUTE.peek();
                match &*snap {
                    None => true,
                    Some(Ok(e)) => !e.connected,
                    Some(Err(_)) => true,
                }
            };
            if needs {
                fetch_compute().await;
            } else {
                match get_json::<ComputeHealth>("/compute/health").await {
                    Ok(h) => store_health(Some(h)),
                    Err(_) => store_health(None),
                }
            }
            gloo_timers::future::TimeoutFuture::new(RETRY_MS).await;
        }
    });

    // The viewed gallery re-joins the running engine's backend on every
    // engine change (card click, Generate-panel self-assembly staging, boot
    // restore). Clicking a header segment only re-points VIEW_BACKEND, so
    // browsing the other backend never disturbs the running engine.
    use_effect(move || {
        let backend = backend_of(&STATE.read().active);
        if *VIEW_BACKEND.peek() != backend {
            *VIEW_BACKEND.write() = backend;
        }
    });

    let st = STATE.read().clone();
    let server_backed = ctx.graph_session.read().is_server_backed();
    let active = st.active.clone();
    let lens: LensConfig = typed_settings(BRIDGE_GEOMETRIC);
    let fa2: RemoteFa2Settings = typed_settings(BRIDGE_REMOTE_FA2);

    // The worker engine id the active bridge selection maps to — the card
    // that reads as selected in the cluster gallery (replaces the old
    // `remote:<id>` <select> encoding).
    let selected_remote: Option<String> = match active.as_str() {
        BRIDGE_GEOMETRIC => Some(
            if lens.use_gpu { "geometric-gpu" } else { BRIDGE_GEOMETRIC }.to_string(),
        ),
        BRIDGE_REMOTE_FA2 => Some(fa2.layout_id.clone()),
        _ => None,
    };

    let snap = COMPUTE.read().clone();
    let health = HEALTH.read().clone();
    let health_view = worker_health_view(health.as_ref());

    let desc = descriptor_for(&active);
    let is_static = desc
        .as_ref()
        .map(|d| d.kind == LayoutKind::Static)
        .unwrap_or(false);
    let is_physics = desc
        .as_ref()
        .map(|d| d.kind == LayoutKind::Physics)
        .unwrap_or(false);
    let is_bridge = active == BRIDGE_GEOMETRIC || active == BRIDGE_REMOTE_FA2;
    // Live sim state for the Pause/Resume toggle (physics + remote engines).
    let sim_running = *render::SIM_RUNNING.read();
    // Static engines don't auto-apply slider edits — flag when the current
    // settings differ from what was last actually solved so the user knows to
    // press Solve. Physics engines apply live, so this never fires for them.
    let static_dirty = is_static
        && LAST_SOLVED
            .read()
            .as_ref()
            .map(|(id, json)| *id == active && *json != settings_or_default(&active))
            .unwrap_or(false);
    let worker_url_val = worker_url();
    let worker_reconnect_val = worker_reconnect_ms();
    let solve_msg = SOLVE_MSG.read().clone();
    let control_busy = *CONTROL_BUSY.read();
    let remote_lease = *REMOTE_LEASE_VIEW.read();
    let remote_solver = remote_solver_status(health.as_ref(), remote_lease, control_busy);
    let history_mode = crate::panels::timeline::history_mode_label();

    let active_backend = backend_of(&active);
    let view = *VIEW_BACKEND.read();
    // Display name of the running engine for the cross-backend banner: the
    // worker's own display name for bridge selections, the registry
    // descriptor otherwise.
    let running_name: String = match &selected_remote {
        Some(sel) => match &snap {
            Some(Ok(eng)) => eng
                .engines
                .iter()
                .find(|e| &e.id == sel)
                .map(|e| e.display_name.clone())
                .unwrap_or_else(|| sel.clone()),
            _ => sel.clone(),
        },
        None => desc
            .as_ref()
            .map(|d| d.display_name.to_string())
            .unwrap_or_else(|| active.clone()),
    };
    let params_name = desc
        .as_ref()
        .map(|d| d.display_name.to_string())
        .unwrap_or_else(|| active.clone());

    rsx! {
        div { class: "lay",
            // Cross-backend banner: browsing one gallery while the running
            // engine belongs to the other. Keeps "view ≠ running" honest —
            // one click jumps back to the running engine's gallery.
            if view != active_backend {
                div { class: "lay-running",
                    span { class: "lay-running-dot", "●" }
                    span { class: "lay-running-text",
                        "Running on {backend_label(active_backend)}: {running_name}"
                    }
                    button {
                        class: "lay-btn lay-small",
                        r#type: "button",
                        title: "Show the gallery that contains the running engine",
                        onclick: move |_| *VIEW_BACKEND.write() = active_backend,
                        "show"
                    }
                }
            }

            match view {
                Backend::Local => local_gallery(&active),
                Backend::Cluster => cluster_gallery(
                    &snap,
                    health.as_ref(),
                    server_backed,
                    selected_remote.as_deref(),
                    is_bridge,
                ),
            }

            // While a remote engine runs and the user browses the local
            // gallery, keep the worker health + browser-owned warnings
            // visible (the cluster view carries them on its status card).
            if is_bridge && view == Backend::Local {
                div { class: "{health_view.class}", "{health_view.text}" }
                if !server_backed {
                    div { class: "lay-health bad",
                        "Remote layouts are disabled: this graph exists only in the browser. \
                         Choose a local engine or host the graph through graph-api."
                    }
                }
            }

            if is_bridge {
                div { class: "lay-hint",
                    "solver: {remote_solver}"
                }
                div { class: "lay-hint",
                    "display: "
                    if sim_running { "following worker frames" } else { "frozen locally · worker still running" }
                }
                div { class: "lay-hint", "history: {history_mode}" }

                // One worker connection, shared by every remote engine (was
                // duplicated per-bridge). Only shown when a worker engine is active.
                div { class: "lay-sub", "Worker connection" }
                TextRow { label: "URL", value: worker_url_val,
                    on: move |v: String| set_worker_connection(Some(v), None) }
                IntRow { label: "Reconnect", min: 100, max: 30_000, value: worker_reconnect_val as i64,
                    note: "ms",
                    on: move |v: i64| set_worker_connection(None, Some(v as u32)) }
            }

            hr { class: "lay-sep" }

            {seed_section(is_bridge)}

            hr { class: "lay-sep" }

            // Parameters belong to the *running* engine regardless of which
            // gallery is being browsed — the header names it and carries the
            // reset-to-defaults action.
            div { class: "lay-params-head",
                span { class: "lay-sub", "Parameters" }
                span { class: "lay-params-engine", title: "{params_name}", "{params_name}" }
                button {
                    class: "lay-btn lay-small",
                    r#type: "button",
                    title: "Reset {params_name} to its default settings",
                    onclick: move |_| {
                        let id = STATE.read().active.clone();
                        put_settings(&id, default_settings(&id));
                    },
                    "↺"
                }
            }
            {engine_params(&active)}

            // One primary-action slot for every engine kind (was two separate
            // Solve / Wake buttons that both called apply_engine(true)), plus a
            // Pause/Resume toggle for anything that runs a live sim.
            if is_static || is_physics {
                hr { class: "lay-sep" }
                div { class: "lay-row",
                    span { class: "lay-k" }
                    if is_static {
                        button {
                            class: if static_dirty { "lay-btn active" } else { "lay-btn" },
                            title: "Run the static layout solver with the current settings.",
                            onclick: move |_| apply_engine(true),
                            "Solve"
                        }
                    } else if is_bridge {
                        button {
                            class: "lay-btn",
                            title: "Reconnect to the compute worker and restart the position stream.",
                            onclick: move |_| restart_active(),
                            "Restart stream"
                        }
                    } else {
                        button {
                            class: "lay-btn",
                            title: "Re-energize the sim. Useful when the layout looks frozen \
                                    (KE below energy_threshold → auto-halt fired).",
                            onclick: move |_| apply_engine(true),
                            "Wake"
                        }
                    }
                    if is_physics {
                        button {
                            class: "lay-btn",
                            title: if is_bridge {
                                "Freeze or resume position updates in this view; the remote worker keeps running."
                            } else {
                                "Pause or resume the local running layout (also on the graph HUD)."
                            },
                            onclick: move |_| render::set_sim_running(!sim_running),
                            { if is_bridge {
                                if sim_running { "Freeze view" } else { "Follow live" }
                            } else if sim_running { "Pause" } else { "Resume" } }
                        }
                    }
                }
                if static_dirty {
                    div { class: "lay-hint", "Unsolved slider changes — press Solve to apply." }
                }
            }
            if !solve_msg.is_empty() {
                div { class: "lay-err", "{solve_msg}" }
            }
        }
    }
}

// --- engine gallery ----------------------------------------------------------------------

/// One rich engine card: name + live "running" pill, kind/processor chips,
/// a clamped description, and a disabled-with-reason state. A real
/// `<button>` so selection is keyboard-reachable (Enter/Space) — one click
/// switches the engine.
#[component]
fn EngineCard(
    name: String,
    description: String,
    kind_label: String,
    kind_class: &'static str,
    proc_label: String,
    proc_class: &'static str,
    active: bool,
    disabled: bool,
    disabled_reason: Option<String>,
    /// Dashed-border "ghost" treatment for the kept-but-unadvertised remote
    /// selection.
    #[props(default)]
    ghost: bool,
    title: String,
    on_select: EventHandler<()>,
) -> Element {
    let mut class = String::from("lay-card");
    if active {
        class.push_str(" active");
    }
    if ghost {
        class.push_str(" ghost");
    }
    rsx! {
        button {
            class: "{class}",
            r#type: "button",
            disabled,
            title: "{title}",
            aria_pressed: if active { "true" } else { "false" },
            onclick: move |_| on_select.call(()),
            div { class: "lay-card-head",
                span { class: "lay-card-name", "{name}" }
                if active {
                    span { class: "lay-live", "● running" }
                }
            }
            div { class: "lay-card-chips",
                span { class: "lay-chip {kind_class}", "{kind_label}" }
                span { class: "lay-chip {proc_class}", "{proc_label}" }
            }
            if !description.is_empty() {
                p { class: "lay-card-desc", "{description}" }
            }
            if let Some(reason) = disabled_reason {
                span { class: "lay-card-why", "{reason}" }
            }
        }
    }
}

fn local_card(d: &LayoutDescriptor, active: bool) -> Element {
    let id = d.id;
    let (kind_label, kind_class) = match d.kind {
        LayoutKind::Static => ("static", "kind-static"),
        LayoutKind::Physics => ("physics", "kind-physics"),
    };
    let (proc_label, proc_class) = if id == "gpu-force" {
        ("gpu", "proc-gpu")
    } else {
        ("cpu", "proc-cpu")
    };
    rsx! {
        EngineCard {
            key: "{id}",
            name: d.display_name.to_string(),
            description: d.description.to_string(),
            kind_label: kind_label.to_string(),
            kind_class,
            proc_label: proc_label.to_string(),
            proc_class,
            active,
            disabled: false,
            disabled_reason: None,
            title: d.description.to_string(),
            on_select: move |_| set_active(id),
        }
    }
}

/// The This Device gallery: one rich card per local engine (gpu-force
/// physics + the static solvers).
fn local_gallery(active: &str) -> Element {
    rsx! {
        div {
            class: "lay-gallery",
            role: "group",
            aria_label: "Layout engines that run on this device",
            for d in local_descriptors() {
                {local_card(d, d.id == active)}
            }
        }
        div { class: "lay-hint",
            "These engines solve in this app — no worker needed. Static engines run \
             once per Solve; physics engines run live."
        }
    }
}

fn remote_card(e: &EngineInfo, selected: bool, server_backed: bool) -> Element {
    let id = e.id.clone();
    let lower = e.kind.to_lowercase();
    let (kind_label, kind_class) = if lower.contains("phys") {
        ("physics".to_string(), "kind-physics")
    } else if lower.contains("static") {
        ("static".to_string(), "kind-static")
    } else if e.kind.is_empty() {
        ("engine".to_string(), "kind-remote")
    } else {
        (e.kind.clone(), "kind-remote")
    };
    let title = if e.description.is_empty() {
        e.kind.clone()
    } else {
        format!("{} — {}", e.kind, e.description)
    };
    let key = id.clone();
    rsx! {
        EngineCard {
            key: "{key}",
            name: e.display_name.clone(),
            description: e.description.clone(),
            kind_label,
            kind_class,
            proc_label: "cluster".to_string(),
            proc_class: "proc-cluster",
            active: selected,
            disabled: !server_backed,
            disabled_reason: (!server_backed)
                .then(|| "browser-owned graph — host it through graph-api".to_string()),
            title,
            on_select: move |_| select_remote_engine(&id),
        }
    }
}

/// The Compute Cluster gallery: a worker status card (health dot, one-line
/// summary, reconnect + refresh) above the engines advertised by
/// `GET /compute/engines`. Unavailable states render as designed empty
/// cards carrying the reason and the recovery action — the same states the
/// old picker's disabled placeholder options carried.
fn cluster_gallery(
    snap: &Option<Result<ComputeEngines, String>>,
    health: Option<&ComputeHealth>,
    server_backed: bool,
    selected_remote: Option<&str>,
    is_bridge: bool,
) -> Element {
    let (dot, state) = worker_status(health);
    let sub = worker_health_sub(health);
    let engines: &[EngineInfo] = match snap {
        Some(Ok(eng)) if eng.connected => &eng.engines,
        _ => &[],
    };
    // Active bridge engine missing from the advertised list — keep the
    // gallery honest about what is selected (the old picker's ghost option).
    let ghost = selected_remote
        .filter(|sel| !engines.iter().any(|e| e.id == *sel))
        .map(str::to_string);

    rsx! {
        div { class: "lay-worker {state}",
            span { class: "lay-worker-dot", "{dot}" }
            div { class: "lay-worker-info",
                div { class: "lay-worker-title", "Compute worker" }
                div { class: "lay-worker-sub", title: "{sub}", "{sub}" }
            }
            if is_bridge {
                button {
                    class: "lay-btn lay-small",
                    r#type: "button",
                    title: "Reconnect to the compute worker and restart the position stream",
                    onclick: move |_| restart_active(),
                    "reconnect"
                }
            }
            button {
                class: "lay-btn lay-small",
                r#type: "button",
                title: "Refresh the remote engine list and worker health",
                onclick: move |_| {
                    spawn(async move { fetch_compute().await; });
                },
                "↻"
            }
        }

        if !server_backed {
            div { class: "lay-health bad",
                "Remote layouts are disabled: this graph exists only in the browser. \
                 Choose a local engine or host the graph through graph-api."
            }
        }

        match snap {
            None => rsx! {
                div { class: "lay-empty",
                    panel_kit::Spinner { label: "contacting graph-api for the engine list…" }
                }
            },
            Some(Err(error)) => rsx! {
                div { class: "lay-empty",
                    span { class: "lay-empty-title", "✕ Engines unavailable" }
                    span { class: "lay-empty-text", "{error}" }
                    button {
                        class: "lay-btn lay-small",
                        r#type: "button",
                        title: "Retry fetching the engine list",
                        onclick: move |_| {
                            spawn(async move { fetch_compute().await; });
                        },
                        "retry"
                    }
                }
            },
            Some(Ok(eng)) if !eng.connected => rsx! {
                div { class: "lay-empty",
                    span { class: "lay-empty-title", "○ No compute worker" }
                    span { class: "lay-empty-text",
                        "graph-api has no compute broker configured (--compute-url). \
                         Worker engines appear here as soon as one connects."
                    }
                }
            },
            Some(Ok(_)) => rsx! {},
        }

        if !engines.is_empty() {
            div {
                class: "lay-gallery",
                role: "group",
                aria_label: "Layout engines on the compute cluster",
                for e in engines {
                    {remote_card(e, selected_remote == Some(e.id.as_str()), server_backed)}
                }
            }
            div { class: "lay-hint",
                "Compute-cluster engines stream from graph-compute via graph-api."
            }
        }

        if let Some(ghost_id) = ghost {
            {
                let id = ghost_id.clone();
                rsx! {
                    EngineCard {
                        name: ghost_id,
                        description: "Not advertised by the connected worker — the selection is kept."
                            .to_string(),
                        kind_label: "engine".to_string(),
                        kind_class: "kind-remote",
                        proc_label: "cluster".to_string(),
                        proc_class: "proc-cluster",
                        active: true,
                        disabled: !server_backed,
                        disabled_reason: (!server_backed)
                            .then(|| "browser-owned graph — host it through graph-api".to_string()),
                        ghost: true,
                        title: "The connected worker does not advertise this engine; the selection is kept."
                            .to_string(),
                        on_select: move |_| select_remote_engine(&id),
                    }
                }
            }
        }
    }
}

// --- seed section --------------------------------------------------------------------

fn seed_section(remote: bool) -> Element {
    let st = STATE.read().clone();
    let strategy = st.seed_strategy.clone();

    let strategy_value = match &strategy {
        SeedStrategy::None => "none".to_string(),
        SeedStrategy::BuiltIn(i) => format!("b{i}"),
        SeedStrategy::Custom => "custom".to_string(),
    };

    let body = match strategy.clone() {
        SeedStrategy::None => rsx! {
            div { class: "lay-hint", "No seed will be applied — positions stay as-is." }
        },
        SeedStrategy::BuiltIn(i) => rsx! {
            div { class: "lay-row",
                span { class: "lay-k" }
                button {
                    class: "lay-btn",
                    title: "Evaluate the seed and place the nodes",
                    onclick: move |_| apply_builtin_seed(i),
                    "Apply seed"
                }
            }
        },
        SeedStrategy::Custom => rsx! {
            div { class: "lay-hint",
                "Implement seed : {{ n, ... }} -> [ {{ x; y; z; }} ]. `n` is bound to the \
                 live node count. Return exactly n positions (or [] for no seed)."
            }
            textarea {
                class: "lay-seed-src",
                spellcheck: false,
                placeholder: "import /jc/src/seed.nix {{}} ...",
                value: "{st.seed_custom}",
                oninput: move |e| {
                    let mut s = STATE.read().clone();
                    s.seed_custom = e.value();
                    persist(&s);
                    *STATE.write() = s;
                },
            }
            div { class: "lay-row",
                span { class: "lay-k" }
                button {
                    class: "lay-btn",
                    title: "Evaluate the seed expression and place the nodes",
                    onclick: move |_| {
                        let src = STATE.read().seed_custom.clone();
                        apply_seed_expr(&src);
                    },
                    "Apply seed"
                }
            }
        },
    };

    let status = SEED_STATUS.read().clone();
    let error = SEED_ERROR.read().clone();

    rsx! {
        div { class: "lay-sub", if remote { "Remote starting placement" } else { "Starting placement" } }
        div { class: "lay-hint",
            if remote {
                "Apply evaluates the seed, sends those starting positions to the remote worker, \
                 and updates this view only after the worker accepts them."
            } else {
                "Reposition the current graph before waking the local layout. Pick a built-in, \
                 write a custom Nix seed, or leave positions unchanged."
            }
        }
        div { class: "lay-row",
            span { class: "lay-k", "seed" }
            select {
                class: "lay-select",
                value: "{strategy_value}",
                onchange: move |e| {
                    match e.value().as_str() {
                        "none" => set_seed_strategy(SeedStrategy::None),
                        "custom" => set_seed_strategy(SeedStrategy::Custom),
                        v => {
                            if let Some(i) = v.strip_prefix('b').and_then(|s| s.parse::<usize>().ok()) {
                                set_seed_strategy(SeedStrategy::BuiltIn(i));
                            }
                        }
                    }
                },
                option {
                    value: "none",
                    title: "Leave the current positions untouched",
                    selected: matches!(strategy, SeedStrategy::None),
                    "No seed"
                }
                for i in builtin_demo_indices() {
                    option {
                        key: "{i}",
                        value: "b{i}",
                        selected: strategy == SeedStrategy::BuiltIn(i),
                        {tvix_wasm::seed_demos()[i].name}
                    }
                }
                option {
                    value: "custom",
                    title: "Author a seed as a Nix expression",
                    selected: matches!(strategy, SeedStrategy::Custom),
                    "Custom (Nix)"
                }
            }
        }
        {body}
        if let Some(s) = status {
            div { class: "lay-hint", "{s}" }
        }
        if let Some(e) = error {
            div { class: "lay-err", "{e}" }
        }
    }
}

// --- per-engine parameter UIs ----------------------------------------------------------

fn engine_params(active: &str) -> Element {
    match active {
        "gpu-force" => gpu_force_ui(),
        "random" => random_ui(),
        "circle" => circle_ui(),
        "grid" => grid_ui(),
        "sphere" => sphere_ui(),
        "concentric" => concentric_ui(),
        "hilbert" => hilbert_ui(),
        "spectral" => spectral_ui(),
        "fcose" => fcose_ui(),
        "cose_bilkent" => cose_bilkent_ui(),
        "cise" => cise_ui(),
        "dagre" => dagre_ui(),
        "klay" => klay_ui(),
        BRIDGE_REMOTE_FA2 => remote_fa2_ui(),
        BRIDGE_GEOMETRIC => geometric_ui(),
        _ => rsx! {
            div { class: "lay-hint", "No layout registered for active id — pick one above." }
        },
    }
}

#[cfg(test)]
mod frame_tests {
    use super::{
        frame_matches_expected, parse_frame, positions_le_bytes, remote_solver_status,
        validate_placement_response, ComputeHealth, InitialPlacementResp, RemoteLease,
    };

    fn lease() -> RemoteLease {
        RemoteLease {
            graph_revision: 17,
            selection_generation: 9,
        }
    }

    fn health(requested: &str, effective: &str) -> ComputeHealth {
        ComputeHealth {
            connected: true,
            url: "http://worker".into(),
            worker_ok: true,
            requested_layout_id: requested.into(),
            effective_layout_id: effective.into(),
            fallback_reason: None,
            worker_status_error: None,
            selection_generation: 9,
            graph_revision: 17,
        }
    }

    #[test]
    fn remote_frame_carries_graph_revision() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&17_u64.to_le_bytes());
        bytes.extend_from_slice(&9_u64.to_le_bytes());
        bytes.extend_from_slice(&4_u64.to_le_bytes());
        bytes.extend_from_slice(&1_u32.to_le_bytes());
        for value in [1.0_f32, -2.0, 3.5] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        let (revision, generation, frame, n, positions) =
            parse_frame(&bytes).expect("valid frame");
        assert_eq!((revision, generation, frame, n), (17, 9, 4, 1));
        assert_eq!(positions, vec![1.0, -2.0, 3.5]);
    }

    #[test]
    fn remote_frame_rejects_legacy_and_truncated_headers() {
        assert!(parse_frame(&[0; 12]).is_none());
        let mut bytes = vec![0; 28];
        bytes[24..28].copy_from_slice(&1_u32.to_le_bytes());
        assert!(parse_frame(&bytes).is_none());
    }

    #[test]
    fn frame_policy_requires_both_revision_and_selection_generation() {
        let expected = Some(lease());
        assert!(frame_matches_expected(17, 9, expected));
        assert!(!frame_matches_expected(18, 9, expected));
        assert!(!frame_matches_expected(17, 10, expected));
        assert!(!frame_matches_expected(17, 9, None));
        assert!(!frame_matches_expected(0, 9, Some(RemoteLease {
            graph_revision: 0,
            selection_generation: 9,
        })));
    }

    #[test]
    fn solver_status_exposes_requested_to_effective_fallback() {
        let mut view = health("cise", "fcose");
        view.fallback_reason = Some("cise unavailable on this worker".into());
        assert_eq!(
            remote_solver_status(Some(&view), Some(lease()), false),
            "requested cise → effective fcose · fallback: cise unavailable on this worker · selection g9"
        );
    }

    #[test]
    fn solver_status_never_applies_health_from_another_lease() {
        let mut view = health("fcose", "fcose");
        view.selection_generation = 10;
        assert_eq!(
            remote_solver_status(Some(&view), Some(lease()), false),
            "refreshing status for graph r17 · selection g9"
        );
    }

    #[test]
    fn placement_response_must_preserve_the_exact_lease_and_arity() {
        let accepted = InitialPlacementResp {
            ok: true,
            n_nodes: 2,
            graph_revision: 17,
            selection_generation: 9,
            error: None,
        };
        assert!(validate_placement_response(&accepted, lease(), 2).is_ok());

        let mut wrong_nodes = accepted.clone();
        wrong_nodes.n_nodes = 3;
        assert!(validate_placement_response(&wrong_nodes, lease(), 2).is_err());
        let mut wrong_revision = accepted.clone();
        wrong_revision.graph_revision = 18;
        assert!(validate_placement_response(&wrong_revision, lease(), 2).is_err());
        let mut wrong_generation = accepted;
        wrong_generation.selection_generation = 10;
        assert!(validate_placement_response(&wrong_generation, lease(), 2).is_err());
    }

    #[test]
    fn placement_bytes_are_exact_little_endian_xyz_and_finite() {
        let positions = [1.0_f32, -2.0, 3.5];
        let encoded = positions_le_bytes(&positions, 1).expect("valid xyz");
        let expected: Vec<u8> = positions
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect();
        assert_eq!(encoded, expected);
        assert!(positions_le_bytes(&positions, 2).is_err());
        assert!(positions_le_bytes(&[0.0, f32::NAN, 1.0], 1).is_err());
    }
}

fn gpu_force_ui() -> Element {
    let opts: GpuForceOptions = typed_settings("gpu-force");
    let active_preset = LayoutPreset::detect(&opts).unwrap_or_default();
    let repulsion_mode = opts.repulsion_mode;

    rsx! {
        // Reset lives in the top-row ↺ (resets the active engine); no
        // per-engine duplicate here.
        div { class: "lay-sub", "Preset" }
        div { class: "lay-presets",
            for preset in LayoutPreset::ALL {
                button {
                    key: "{preset.label()}",
                    class: if preset == active_preset { "lay-btn active" } else { "lay-btn" },
                    onclick: move |_| edit::<GpuForceOptions>("gpu-force", |o| preset.apply_to(o)),
                    {preset.label()}
                }
            }
        }

        hr { class: "lay-sep" }

        div { class: "lay-sub", "Physics" }
        Slider { label: "repulsion", min: 0.1, max: 100_000.0, value: opts.repulsion as f64, log: true,
            on: move |v: f64| edit::<GpuForceOptions>("gpu-force", |o| o.repulsion = v as f32) }
        Slider { label: "spring_k", min: 0.0001, max: 10.0, value: opts.spring_k as f64, log: true,
            on: move |v: f64| edit::<GpuForceOptions>("gpu-force", |o| o.spring_k = v as f32) }
        Slider { label: "spring_len", min: 1.0, max: 10_000.0, value: opts.spring_len as f64, log: true,
            on: move |v: f64| edit::<GpuForceOptions>("gpu-force", |o| o.spring_len = v as f32) }
        Slider { label: "gravity", min: 0.00001, max: 1.0, value: opts.gravity as f64, log: true,
            on: move |v: f64| edit::<GpuForceOptions>("gpu-force", |o| o.gravity = v as f32) }
        Slider { label: "damping", min: 0.0, max: 1.0, value: opts.damping as f64,
            on: move |v: f64| edit::<GpuForceOptions>("gpu-force", |o| o.damping = v as f32) }
        Slider { label: "dt", min: 0.0001, max: 1.0, value: opts.dt as f64, log: true,
            on: move |v: f64| edit::<GpuForceOptions>("gpu-force", |o| o.dt = v as f32) }
        Slider { label: "steps/call", min: 1.0, max: 64.0, value: opts.steps_per_call as f64,
            on: move |v: f64| edit::<GpuForceOptions>("gpu-force", |o| {
                o.steps_per_call = v.round().max(1.0) as u32;
            }) }

        hr { class: "lay-sep" }

        div { class: "lay-sub", "Cooling" }
        div { class: "lay-hint", "Drives sim toward steady state" }
        Slider { label: "cooling α", min: 0.9, max: 1.0, value: opts.cooling_alpha as f64,
            on: move |v: f64| edit::<GpuForceOptions>("gpu-force", |o| o.cooling_alpha = v as f32) }
        Slider { label: "cooling floor", min: 0.0, max: 1.0, value: opts.cooling_floor as f64,
            on: move |v: f64| edit::<GpuForceOptions>("gpu-force", |o| o.cooling_floor = v as f32) }

        hr { class: "lay-sep" }

        div { class: "lay-sub", "Auto-halt" }
        div { class: "lay-hint", "Stop dispatching when truly settled" }
        Slider { label: "energy halt", min: 0.0, max: 1.0, value: opts.energy_threshold as f64,
            on: move |v: f64| edit::<GpuForceOptions>("gpu-force", |o| o.energy_threshold = v as f32) }

        hr { class: "lay-sep" }

        div { class: "lay-sub", "Repulsion backend" }
        div { class: "lay-hint", "Grid: dense small; BH: clustered; NS: huge" }
        div { class: "lay-row",
            span { class: "lay-k", "mode" }
            select {
                class: "lay-select",
                value: match repulsion_mode {
                    RepulsionMode::Grid => "grid",
                    RepulsionMode::BarnesHut => "bh",
                    RepulsionMode::NegativeSampling => "ns",
                },
                onchange: move |e| edit::<GpuForceOptions>("gpu-force", |o| {
                    o.repulsion_mode = match e.value().as_str() {
                        "bh" => RepulsionMode::BarnesHut,
                        "ns" => RepulsionMode::NegativeSampling,
                        _ => RepulsionMode::Grid,
                    };
                }),
                option { value: "grid", selected: repulsion_mode == RepulsionMode::Grid, "Grid (27-cell)" }
                option { value: "bh", selected: repulsion_mode == RepulsionMode::BarnesHut, "Barnes-Hut" }
                option { value: "ns", selected: repulsion_mode == RepulsionMode::NegativeSampling, "Negative sampling" }
            }
        }
        if repulsion_mode == RepulsionMode::NegativeSampling {
            Slider { label: "K samples", min: 1.0, max: 32.0, value: opts.repulsion_samples as f64,
                on: move |v: f64| edit::<GpuForceOptions>("gpu-force", |o| {
                    o.repulsion_samples = v.round().max(1.0) as u32;
                }) }
        }
    }
}

fn random_ui() -> Element {
    let s: RandomSettings = typed_settings("random");
    rsx! {
        SeedRow { label: "seed", value: s.seed,
            on: move |v: u64| edit::<RandomSettings>("random", |s| s.seed = v) }
        Slider { label: "radius", min: 1.0, max: 2000.0, value: s.radius as f64,
            on: move |v: f64| edit::<RandomSettings>("random", |s| s.radius = v as f32) }
    }
}

fn circle_ui() -> Element {
    let s: CircleSettings = typed_settings("circle");
    let axis = s.axis;
    rsx! {
        Slider { label: "radius", min: 1.0, max: 2000.0, value: s.radius as f64,
            on: move |v: f64| edit::<CircleSettings>("circle", |s| s.radius = v as f32) }
        div { class: "lay-row",
            span { class: "lay-k", "axis" }
            select {
                class: "lay-select",
                value: match axis { CircleAxis::Z => "z", CircleAxis::X => "x", CircleAxis::Y => "y" },
                onchange: move |e| edit::<CircleSettings>("circle", |s| {
                    s.axis = match e.value().as_str() {
                        "x" => CircleAxis::X,
                        "y" => CircleAxis::Y,
                        _ => CircleAxis::Z,
                    };
                }),
                option { value: "z", selected: axis == CircleAxis::Z, "Z (xy plane)" }
                option { value: "x", selected: axis == CircleAxis::X, "X (yz plane)" }
                option { value: "y", selected: axis == CircleAxis::Y, "Y (xz plane)" }
            }
        }
    }
}

fn grid_ui() -> Element {
    let s: GridSettings = typed_settings("grid");
    rsx! {
        Slider { label: "spacing", min: 1.0, max: 500.0, value: s.spacing as f64,
            on: move |v: f64| edit::<GridSettings>("grid", |s| s.spacing = v as f32) }
        Slider { label: "aspect", min: 0.25, max: 4.0, value: s.aspect as f64,
            on: move |v: f64| edit::<GridSettings>("grid", |s| s.aspect = v as f32) }
        IntRow { label: "layers", min: 1, max: 32, value: s.layers as i64,
            on: move |v: i64| edit::<GridSettings>("grid", |s| s.layers = v as u32) }
        CheckRow { label: "center", value: s.center, text: "center",
            on: move |v: bool| edit::<GridSettings>("grid", |s| s.center = v) }
    }
}

fn sphere_ui() -> Element {
    let s: SphereSettings = typed_settings("sphere");
    rsx! {
        Slider { label: "radius", min: 1.0, max: 2000.0, value: s.radius as f64,
            on: move |v: f64| edit::<SphereSettings>("sphere", |s| s.radius = v as f32) }
        Slider { label: "jitter", min: 0.0, max: 1.0, value: s.jitter as f64,
            on: move |v: f64| edit::<SphereSettings>("sphere", |s| s.jitter = v as f32) }
        SeedRow { label: "seed", value: s.seed,
            on: move |v: u64| edit::<SphereSettings>("sphere", |s| s.seed = v) }
    }
}

fn concentric_ui() -> Element {
    let s: ConcentricSettings = typed_settings("concentric");
    let metric = s.metric;
    rsx! {
        div { class: "lay-row",
            span { class: "lay-k", "metric" }
            select {
                class: "lay-select",
                value: match metric {
                    ConcentricMetric::Degree => "degree",
                    ConcentricMetric::InDegree => "in",
                    ConcentricMetric::OutDegree => "out",
                    ConcentricMetric::Alphabetical => "alpha",
                },
                onchange: move |e| edit::<ConcentricSettings>("concentric", |s| {
                    s.metric = match e.value().as_str() {
                        "in" => ConcentricMetric::InDegree,
                        "out" => ConcentricMetric::OutDegree,
                        "alpha" => ConcentricMetric::Alphabetical,
                        _ => ConcentricMetric::Degree,
                    };
                }),
                option { value: "degree", selected: metric == ConcentricMetric::Degree, "Degree (in + out)" }
                option { value: "in", selected: metric == ConcentricMetric::InDegree, "In-degree" }
                option { value: "out", selected: metric == ConcentricMetric::OutDegree, "Out-degree" }
                option { value: "alpha", selected: metric == ConcentricMetric::Alphabetical, "Alphabetical (by id)" }
            }
        }
        Slider { label: "min radius", min: 1.0, max: 1000.0, value: s.min_radius as f64,
            on: move |v: f64| edit::<ConcentricSettings>("concentric", |s| s.min_radius = v as f32) }
        Slider { label: "level spacing", min: 1.0, max: 500.0, value: s.level_spacing as f64,
            on: move |v: f64| edit::<ConcentricSettings>("concentric", |s| s.level_spacing = v as f32) }
        CheckRow { label: "clockwise", value: s.clockwise, text: "clockwise",
            on: move |v: bool| edit::<ConcentricSettings>("concentric", |s| s.clockwise = v) }
        IntRow { label: "buckets", min: 0, max: 64, value: s.bucket_count as i64,
            note: "(0 = distinct values)",
            on: move |v: i64| edit::<ConcentricSettings>("concentric", |s| s.bucket_count = v as u32) }
    }
}

fn hilbert_ui() -> Element {
    let s: HilbertSettings = typed_settings("hilbert");
    rsx! {
        Slider { label: "extent", min: 10.0, max: 10_000.0, value: s.extent as f64,
            on: move |v: f64| edit::<HilbertSettings>("hilbert", |s| s.extent = v as f32) }
        IntRow { label: "order", min: 1, max: 10, value: s.order as i64,
            on: move |v: i64| edit::<HilbertSettings>("hilbert", |s| s.order = v as u32) }
        CheckRow { label: "flatten", value: s.flatten,
            on: move |v: bool| edit::<HilbertSettings>("hilbert", |s| s.flatten = v) }
        CheckRow { label: "center", value: s.center,
            on: move |v: bool| edit::<HilbertSettings>("hilbert", |s| s.center = v) }
    }
}

fn spectral_ui() -> Element {
    let s: SpectralSettings = typed_settings("spectral");
    rsx! {
        Slider { label: "radius", min: 1.0, max: 2000.0, value: s.radius as f64,
            on: move |v: f64| edit::<SpectralSettings>("spectral", |s| s.radius = v as f32) }
        Slider { label: "iterations", min: 10.0, max: 1000.0, value: s.iterations as f64,
            title: "Power-iteration steps per Fiedler axis. Clustered graphs converge in few.",
            on: move |v: f64| edit::<SpectralSettings>("spectral", |s| s.iterations = v.round() as u32) }
        CheckRow { label: "3D", value: s.three_d, text: "3D (third Fiedler axis)",
            on: move |v: bool| edit::<SpectralSettings>("spectral", |s| s.three_d = v) }
        div { class: "lay-hint", "Seed only — follow with a force/geometric layout to refine." }
    }
}

fn fcose_ui() -> Element {
    let s: FcoseSettings = typed_settings("fcose");
    let quality = s.quality;
    rsx! {
        Slider { label: "node repulsion", min: 100.0, max: 10000.0, value: s.node_repulsion,
            on: move |v: f64| edit::<FcoseSettings>("fcose", |s| s.node_repulsion = v) }
        Slider { label: "ideal edge length", min: 10.0, max: 300.0, value: s.ideal_edge_length,
            on: move |v: f64| edit::<FcoseSettings>("fcose", |s| s.ideal_edge_length = v) }
        Slider { label: "node overlap", min: 0.0, max: 100.0, value: s.node_overlap,
            on: move |v: f64| edit::<FcoseSettings>("fcose", |s| s.node_overlap = v) }
        div { class: "lay-row",
            span { class: "lay-k", "quality" }
            select {
                class: "lay-select",
                value: match quality {
                    FcoseQuality::Draft => "draft",
                    FcoseQuality::Default => "default",
                    FcoseQuality::Proof => "proof",
                },
                onchange: move |e| edit::<FcoseSettings>("fcose", |s| {
                    s.quality = match e.value().as_str() {
                        "draft" => FcoseQuality::Draft,
                        "proof" => FcoseQuality::Proof,
                        _ => FcoseQuality::Default,
                    };
                }),
                option { value: "draft", selected: quality == FcoseQuality::Draft, "Draft" }
                option { value: "default", selected: quality == FcoseQuality::Default, "Default" }
                option { value: "proof", selected: quality == FcoseQuality::Proof, "Proof" }
            }
        }
    }
}

fn cose_bilkent_ui() -> Element {
    let s: CoseBilkentSettings = typed_settings("cose_bilkent");
    rsx! {
        Slider { label: "node repulsion", min: 100.0, max: 10000.0, value: s.node_repulsion,
            on: move |v: f64| edit::<CoseBilkentSettings>("cose_bilkent", |s| s.node_repulsion = v) }
        Slider { label: "ideal edge length", min: 10.0, max: 300.0, value: s.ideal_edge_length,
            on: move |v: f64| edit::<CoseBilkentSettings>("cose_bilkent", |s| s.ideal_edge_length = v) }
        IntRow { label: "iterations", min: 1, max: 2000, value: s.iterations as i64,
            on: move |v: i64| edit::<CoseBilkentSettings>("cose_bilkent", |s| s.iterations = v as u32) }
    }
}

fn cise_ui() -> Element {
    let s: CiseSettings = typed_settings("cise");
    rsx! {
        Slider { label: "circle spacing", min: 1.0, max: 200.0, value: s.circle_spacing,
            on: move |v: f64| edit::<CiseSettings>("cise", |s| s.circle_spacing = v) }
    }
}

fn dagre_ui() -> Element {
    let s: DagreSettings = typed_settings("dagre");
    let dir = s.rank_direction;
    let ranker = s.ranker;
    rsx! {
        div { class: "lay-row",
            span { class: "lay-k", "rank direction" }
            select {
                class: "lay-select",
                value: match dir {
                    RankDirection::TB => "tb",
                    RankDirection::BT => "bt",
                    RankDirection::LR => "lr",
                    RankDirection::RL => "rl",
                },
                onchange: move |e| edit::<DagreSettings>("dagre", |s| {
                    s.rank_direction = match e.value().as_str() {
                        "bt" => RankDirection::BT,
                        "lr" => RankDirection::LR,
                        "rl" => RankDirection::RL,
                        _ => RankDirection::TB,
                    };
                }),
                option { value: "tb", selected: dir == RankDirection::TB, "Top → Bottom" }
                option { value: "bt", selected: dir == RankDirection::BT, "Bottom → Top" }
                option { value: "lr", selected: dir == RankDirection::LR, "Left → Right" }
                option { value: "rl", selected: dir == RankDirection::RL, "Right → Left" }
            }
        }
        div { class: "lay-row",
            span { class: "lay-k", "ranker" }
            select {
                class: "lay-select",
                value: match ranker {
                    DagreRanker::NetworkSimplex => "ns",
                    DagreRanker::TightTree => "tt",
                    DagreRanker::LongestPath => "lp",
                },
                onchange: move |e| edit::<DagreSettings>("dagre", |s| {
                    s.ranker = match e.value().as_str() {
                        "tt" => DagreRanker::TightTree,
                        "lp" => DagreRanker::LongestPath,
                        _ => DagreRanker::NetworkSimplex,
                    };
                }),
                option { value: "ns", selected: ranker == DagreRanker::NetworkSimplex, "Network simplex" }
                option { value: "tt", selected: ranker == DagreRanker::TightTree, "Tight tree" }
                option { value: "lp", selected: ranker == DagreRanker::LongestPath, "Longest path" }
            }
        }
        Slider { label: "rank separation", min: 10.0, max: 300.0, value: s.rank_separation,
            on: move |v: f64| edit::<DagreSettings>("dagre", |s| s.rank_separation = v) }
        Slider { label: "node separation", min: 10.0, max: 300.0, value: s.node_separation,
            on: move |v: f64| edit::<DagreSettings>("dagre", |s| s.node_separation = v) }
        CheckRow { label: "acyclic", value: s.acyclic, text: "acyclic (break cycles)",
            on: move |v: bool| edit::<DagreSettings>("dagre", |s| s.acyclic = v) }
    }
}

fn klay_ui() -> Element {
    let s: KlaySettings = typed_settings("klay");
    rsx! {
        Slider { label: "layer_spacing", min: 10.0, max: 300.0, value: s.layer_spacing,
            on: move |v: f64| edit::<KlaySettings>("klay", |s| s.layer_spacing = v) }
        Slider { label: "node_spacing", min: 10.0, max: 300.0, value: s.node_spacing,
            on: move |v: f64| edit::<KlaySettings>("klay", |s| s.node_spacing = v) }
    }
}

fn remote_fa2_ui() -> Element {
    let s: RemoteFa2Settings = typed_settings(BRIDGE_REMOTE_FA2);
    rsx! {
        div { class: "lay-hint",
            { format!(
                "Streams the '{}' engine from the compute worker. Set the address \
                 in Worker connection above.",
                s.layout_id
            ) }
        }
    }
}

fn geometric_ui() -> Element {
    let opts: LensConfig = typed_settings(BRIDGE_GEOMETRIC);
    let class = opts.class;
    let coordination = opts.coordination;
    let mass = opts.mass;
    let edge_length = opts.edge_length;
    let strength_lens = matches!(
        edge_length,
        EdgeLengthLens::JaccardStrength | EdgeLengthLens::CorrectedOverlapStrength
    );
    let bonding = opts.bonding_enabled;

    rsx! {
        // Connection (URL / reconnect) is the shared "Worker connection" row at
        // the top of the panel; reset is the top-row ↺. GPU is the geometric vs
        // geometric-gpu entry in the Engine picker.
        div { class: "lay-sub", "Options" }
        CheckRow { label: "Multilevel", value: opts.use_multilevel, text: "coarsen",
            title: "Solve on a coarsened graph hierarchy first, then refine — faster \
                    convergence on large graphs. Off by default.",
            on: move |v: bool| edit::<LensConfig>(BRIDGE_GEOMETRIC, |c| c.use_multilevel = v) }

        hr { class: "lay-sep" }

        div { class: "lay-sub", "Presets" }
        div { class: "lay-presets",
            for preset in LensPreset::ALL {
                button {
                    key: "{preset.label()}",
                    class: "lay-btn",
                    onclick: move |_| edit::<LensConfig>(BRIDGE_GEOMETRIC, |c| preset.apply_to(c)),
                    {preset.label()}
                }
            }
        }

        hr { class: "lay-sep" }

        div { class: "lay-sub", "Roles" }
        div { class: "lay-row",
            span { class: "lay-k", "Class" }
            select {
                class: "lay-select",
                value: match class {
                    ClassLens::Uniform => "uniform",
                    ClassLens::DegreeBuckets => "degree_buckets",
                    ClassLens::Louvain => "louvain",
                    // Field/Tag/NodeType have no picker entry (egui parity:
                    // its combo offered the same three) — show no selection.
                    _ => "other",
                },
                onchange: move |e| edit::<LensConfig>(BRIDGE_GEOMETRIC, |c| {
                    c.class = match e.value().as_str() {
                        "degree_buckets" => ClassLens::DegreeBuckets,
                        "louvain" => ClassLens::Louvain,
                        _ => ClassLens::Uniform,
                    };
                }),
                option { value: "uniform", selected: class == ClassLens::Uniform, "Uniform" }
                option { value: "degree_buckets", selected: class == ClassLens::DegreeBuckets, "DegreeBuckets" }
                option { value: "louvain", selected: class == ClassLens::Louvain, "Louvain" }
            }
        }
        div { class: "lay-row",
            span { class: "lay-k", "Coordination" }
            select {
                class: "lay-select",
                value: match coordination {
                    CoordinationLens::Degree => "degree",
                    CoordinationLens::Uniform(_) => "uniform",
                    _ => "other", // Field(_) — no picker entry (egui parity)
                },
                onchange: move |e| edit::<LensConfig>(BRIDGE_GEOMETRIC, |c| {
                    c.coordination = match e.value().as_str() {
                        "uniform" => CoordinationLens::Uniform(0),
                        _ => CoordinationLens::Degree,
                    };
                }),
                option { value: "degree", selected: coordination == CoordinationLens::Degree, "Degree" }
                option { value: "uniform", selected: matches!(coordination, CoordinationLens::Uniform(_)), "Uniform" }
            }
        }
        div { class: "lay-row",
            span { class: "lay-k", "Mass" }
            select {
                class: "lay-select",
                value: match mass {
                    MassLens::Uniform => "uniform",
                    MassLens::Degree => "degree",
                    MassLens::PageRank => "pagerank",
                    _ => "other", // Field(_) — no picker entry (egui parity)
                },
                onchange: move |e| edit::<LensConfig>(BRIDGE_GEOMETRIC, |c| {
                    c.mass = match e.value().as_str() {
                        "degree" => MassLens::Degree,
                        "pagerank" => MassLens::PageRank,
                        _ => MassLens::Uniform,
                    };
                }),
                option { value: "uniform", selected: mass == MassLens::Uniform, "Uniform" }
                option { value: "degree", selected: mass == MassLens::Degree, "Degree" }
                option { value: "pagerank", selected: mass == MassLens::PageRank, "PageRank" }
            }
        }
        div { class: "lay-row",
            span { class: "lay-k", "Edge Length" }
            select {
                class: "lay-select",
                value: match edge_length {
                    EdgeLengthLens::Uniform => "uniform",
                    EdgeLengthLens::Weight => "weight",
                    EdgeLengthLens::EdgeType => "edge_type",
                    EdgeLengthLens::JaccardStrength => "jaccard",
                    EdgeLengthLens::CorrectedOverlapStrength => "corrected",
                },
                onchange: move |e| edit::<LensConfig>(BRIDGE_GEOMETRIC, |c| {
                    c.edge_length = match e.value().as_str() {
                        "weight" => EdgeLengthLens::Weight,
                        "edge_type" => EdgeLengthLens::EdgeType,
                        "jaccard" => EdgeLengthLens::JaccardStrength,
                        "corrected" => EdgeLengthLens::CorrectedOverlapStrength,
                        _ => EdgeLengthLens::Uniform,
                    };
                }),
                option { value: "uniform", selected: edge_length == EdgeLengthLens::Uniform, "Uniform" }
                option { value: "weight", selected: edge_length == EdgeLengthLens::Weight, "Weight" }
                option { value: "edge_type", selected: edge_length == EdgeLengthLens::EdgeType, "EdgeType" }
                option {
                    value: "jaccard",
                    selected: edge_length == EdgeLengthLens::JaccardStrength,
                    title: "De-hairball: short rest length for intra-cluster edges, long for global shortcuts.",
                    "Strength (Jaccard)"
                }
                option {
                    value: "corrected",
                    selected: edge_length == EdgeLengthLens::CorrectedOverlapStrength,
                    title: "Edge strength via Batagelj corrected overlap; damps tiny-dense-subgraph over-emphasis.",
                    "Strength (corrected)"
                }
            }
        }
        if strength_lens {
            Slider { label: "Strength Spread", min: 0.0, max: 8.0, value: opts.edge_strength_spread as f64,
                title: "Shortcut edges target rest_len·(1+spread); 0 disables the de-hairball effect.",
                on: move |v: f64| edit::<LensConfig>(BRIDGE_GEOMETRIC, |c| c.edge_strength_spread = v as f32) }
        }

        hr { class: "lay-sep" }

        div { class: "lay-sub", "Physics" }
        Slider { label: "Exclusion", min: 0.1, max: 10_000.0, value: opts.exclusion_strength as f64, log: true,
            on: move |v: f64| edit::<LensConfig>(BRIDGE_GEOMETRIC, |c| c.exclusion_strength = v as f32) }
        Slider { label: "Affinity", min: -100.0, max: 100.0, value: opts.affinity_strength as f64,
            on: move |v: f64| edit::<LensConfig>(BRIDGE_GEOMETRIC, |c| c.affinity_strength = v as f32) }
        Slider { label: "Edge Stiffness", min: 0.0, max: 1.0, value: opts.edge_stiffness as f64,
            on: move |v: f64| edit::<LensConfig>(BRIDGE_GEOMETRIC, |c| c.edge_stiffness = v as f32) }
        Slider { label: "Angle Stiffness", min: 0.0, max: 1.0, value: opts.angle_stiffness as f64,
            on: move |v: f64| edit::<LensConfig>(BRIDGE_GEOMETRIC, |c| c.angle_stiffness = v as f32) }
        Slider { label: "Gravity", min: 0.0, max: 0.1, value: opts.gravity as f64,
            on: move |v: f64| edit::<LensConfig>(BRIDGE_GEOMETRIC, |c| c.gravity = v as f32) }

        div { class: "lay-sub", "Integrator" }
        Slider { label: "Time step", min: 0.01, max: 1.0, value: opts.time_step as f64, log: true,
            title: "Integration dt. The strongest stabilizer: if the layout flip-flops at the \
                    Max step cap (stiff forces, K·dt² > 2), halve this first.",
            on: move |v: f64| edit::<LensConfig>(BRIDGE_GEOMETRIC, |c| c.time_step = v as f32) }
        Slider { label: "Damping", min: 0.0, max: 1.0, value: opts.damping as f64,
            title: "Velocity retention per step (1 = frictionless). Lower dissipates overshoot \
                    oscillation faster.",
            on: move |v: f64| edit::<LensConfig>(BRIDGE_GEOMETRIC, |c| c.damping = v as f32) }
        Slider { label: "Max step", min: 0.0, max: 20.0, value: opts.max_step as f64,
            title: "Per-step displacement cap per node (0 = uncapped). A spike guard, not a \
                    stabilizer — if every step saturates it, lower Time step instead.",
            on: move |v: f64| edit::<LensConfig>(BRIDGE_GEOMETRIC, |c| c.max_step = v as f32) }

        hr { class: "lay-sep" }

        div { class: "lay-sub", "Self-assembly" }
        div { class: "lay-hint",
            "Tune bonding on the current graph here. The Generate panel's "
            "\"Assemble\" spawns a fresh particle soup and hands it to this engine "
            "(it stages the matching regime automatically)."
        }
        CheckRow { label: "Bonding", value: bonding, text: "enabled",
            title: "Add/remove edges (bonds) each step under a proximity + class + valence + \
                    angle constraint, so chains → sheets → tubes → vesicles emerge on an \
                    evolving graph. OFF = byte-identical default engine behaviour.",
            on: move |v: bool| edit::<LensConfig>(BRIDGE_GEOMETRIC, |c| c.bonding_enabled = v) }

        div { class: "lay-presets",
            for preset in SelfAssemblyPreset::ALL {
                button {
                    key: "{preset.label()}",
                    class: "lay-btn",
                    title: preset.tooltip(),
                    onclick: move |_| edit::<LensConfig>(BRIDGE_GEOMETRIC, |c| preset.apply_to(c)),
                    {preset.label()}
                }
            }
        }

        if bonding {
            Slider { label: "r_bond", min: 0.5, max: 3.0, value: opts.r_bond as f64,
                title: "Bond creation cutoff (σ): a compatible pair closer than this bonds.",
                on: move |v: f64| edit::<LensConfig>(BRIDGE_GEOMETRIC, |c| c.r_bond = v as f32) }
            Slider { label: "r_break", min: 0.5, max: 4.0, value: opts.r_break as f64,
                title: "Bond break cutoff (hysteresis ≈1.2–1.5·r_bond, prevents flicker).",
                on: move |v: f64| edit::<LensConfig>(BRIDGE_GEOMETRIC, |c| c.r_break = v as f32) }
            Slider { label: "Bond stiffness", min: 0.0, max: 1.0, value: opts.bond_stiffness as f64,
                on: move |v: f64| edit::<LensConfig>(BRIDGE_GEOMETRIC, |c| c.bond_stiffness = v as f32) }
            IntRow { label: "Bond every", min: 1, max: 64, value: opts.bond_every as i64,
                title: "Rebuild the bond set every N steps (Verlet amortisation).",
                on: move |v: i64| edit::<LensConfig>(BRIDGE_GEOMETRIC, |c| c.bond_every = v as u32) }
            IntRow { label: "Max valence", min: 0, max: 8, value: opts.default_max_valence as i64,
                title: "Per-node bond cap: 0=uncapped, 2=chain, 3=honeycomb, 4=square net.",
                on: move |v: i64| edit::<LensConfig>(BRIDGE_GEOMETRIC, |c| c.default_max_valence = v as u32) }
            Slider { label: "Bond angle", min: 60.0, max: 180.0, value: opts.default_bond_angle as f64,
                title: "Target angle between a node's bonds (°): 180=chain, 120=honeycomb, 90=square.",
                on: move |v: f64| edit::<LensConfig>(BRIDGE_GEOMETRIC, |c| c.default_bond_angle = v as f32) }
            Slider { label: "Line tension", min: 0.0, max: 8.0, value: opts.line_tension as f64,
                title: "Rim seam force on under-coordinated boundary nodes — closes an open sheet \
                        (needs a valence cap). 0=OFF.",
                on: move |v: f64| edit::<LensConfig>(BRIDGE_GEOMETRIC, |c| c.line_tension = v as f32) }
            Slider { label: "Spont. curvature", min: 0.0, max: 1.0, value: opts.spont_curvature as f64,
                title: "Preferred tilt across each bond (radians): 0=flat, intermediate=tube, higher=vesicle.",
                on: move |v: f64| edit::<LensConfig>(BRIDGE_GEOMETRIC, |c| c.spont_curvature = v as f32) }

            div { class: "lay-sub", "Membrane" }
            Slider { label: "Well depth", min: 0.0, max: 5.0, value: opts.well_depth as f64,
                title: "Cooke–Deserno cohesion-well depth ε — condenses the soup so bonds can form. \
                        0=no cohesion.",
                on: move |v: f64| edit::<LensConfig>(BRIDGE_GEOMETRIC, |c| c.well_depth = v as f32) }
            Slider { label: "Well width", min: 0.5, max: 2.5, value: opts.well_width as f64,
                on: move |v: f64| edit::<LensConfig>(BRIDGE_GEOMETRIC, |c| c.well_width = v as f32) }
            Slider { label: "Temperature", min: 0.0, max: 1.0, value: opts.temperature as f64,
                title: "Langevin kT — the Brownian drive self-assembly emerges from. \
                        0=deterministic minimizer.",
                on: move |v: f64| edit::<LensConfig>(BRIDGE_GEOMETRIC, |c| c.temperature = v as f32) }
            Slider { label: "Anisotropy", min: 0.0, max: 3.0, value: opts.anisotropy_strength as f64,
                title: "Patchy-well orientation anisotropy — drives nematic/membrane order.",
                on: move |v: f64| edit::<LensConfig>(BRIDGE_GEOMETRIC, |c| c.anisotropy_strength = v as f32) }
            Slider { label: "GB side bias", min: 0.0, max: 3.0, value: opts.gb_side_strength as f64,
                title: "Gay–Berne side-by-side packing bias — flat lamella over a droplet.",
                on: move |v: f64| edit::<LensConfig>(BRIDGE_GEOMETRIC, |c| c.gb_side_strength = v as f32) }
            Slider { label: "Tilt coupling", min: 0.0, max: 3.0, value: opts.tilt_coupling_strength as f64,
                title: "Director→position coupling — turns the orientational preference into real \
                        (flat/curved) geometry.",
                on: move |v: f64| edit::<LensConfig>(BRIDGE_GEOMETRIC, |c| c.tilt_coupling_strength = v as f32) }
        }
    }
}
