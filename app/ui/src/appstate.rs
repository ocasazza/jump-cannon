//! AppState round-trip system (phase 4).
//!
//! Port of `crates/graph-renderer/src/ui/{state,share,persist}.rs` (723af10):
//! one serializable [`AppState`] aggregating the panel states, with YAML/JSON
//! export-import, the share-link codec (JSON → DEFLATE → base64url, the same
//! pipeline + constants as the egui `ui/share.rs`), the snapshot-ring
//! timeline, `?config=<name>` / `#s=<hash>` URL bootstrapping, and
//! mutation-site attribution (`snapshot_source` + the frontend event log).
//!
//! Storage mapping vs the egui app:
//!   * egui persisted the whole `AppState` as ONE sessionStorage blob; this
//!     app's panels each own a `jc_*` localStorage key (established in
//!     phases 2-3). [`capture`] aggregates those into the egui-shaped
//!     struct; [`apply`] fans a struct back out over the keys.
//!   * egui's snapshot ring was in-memory (`#[serde(skip)]`, per-session);
//!     here it lives in sessionStorage — the same per-session lifetime, and
//!     it survives the page reload that [`apply`] needs (every panel's
//!     `GlobalSignal`s re-seed from localStorage only at boot, so a fresh
//!     boot is the only way to swap state from outside the panel modules —
//!     the egui equivalent of `*state = imported`).

use std::collections::BTreeMap;

use base64::Engine as _;
use dioxus::prelude::*;
use gloo_storage::{LocalStorage, SessionStorage, Storage};
use serde::{Deserialize, Serialize};

use crate::panels::{camera, debug, filter, metrics, style, timeline};

/// localStorage prefix that scopes this app's persisted state.
const STATE_PREFIX: &str = "jc_";

/// sessionStorage key for the snapshot ring. A NEW key (the pre-phase-4
/// `jc_instances_ring` held flat key→value captures whose JSON would decode
/// into an all-defaults `AppState` and silently reset everything on Restore).
const RING_KEY: &str = "jc_appstate_ring";

/// URL-fragment key the encoded state rides under: `#s=<hash>` — same key as
/// the egui share codec (`ui/share.rs::FRAGMENT_KEY`).
pub(crate) const FRAGMENT_KEY: &str = "s";

/// URL query param naming a dev-server preset to load at boot:
/// `?config=<name>` → `GET /configs/<name>` → [`apply`]. The link shape the
/// graph-api `/configs` endpoints were built for (server.rs comment).
const CONFIG_PARAM: &str = "config";

/// DEFLATE compression level (0..=10 in miniz_oxide) — egui's `DEFLATE_LEVEL`.
const DEFLATE_LEVEL: u8 = 9;

/// base64url alphabet, no padding — fragment-safe (`-`/`_`, no `+`/`/`/`=`).
const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// Auto-snapshot cadence — egui `tick_snapshots`'s `SNAPSHOT_INTERVAL`.
const SNAPSHOT_INTERVAL_MS: u32 = 250;

/// The `jc_*` localStorage keys covered by [`AppState`]'s typed fields.
/// Everything else under the prefix rides in [`AppState::extra`] raw.
const COVERED_KEYS: &[&str] = &[
    "jc_style_v1",
    "jc_camera_v1",
    "jc_metrics_v1",
    "jc_filter_v1",
    "jc_timeline_v1",
    "jc_debug_v1",
    "jc_layout_v1",
    "jc_generate_v1",
];

// --- the aggregate state -----------------------------------------------------------

/// The full persisted UI state — field inventory (and serde names) match the
/// egui `ui/state.rs::AppState`'s persisted subset where this app has the
/// counterpart panel, so an egui-era preset YAML / share hash decodes into
/// the shared fields (`#[serde(default)]` everywhere absorbs the rest).
///
/// `layout` / `generate` are owned by sibling modules this file may not
/// import state types from, so they ride as raw JSON (`jc_layout_v1` /
/// `jc_generate_v1` payloads — `layout` is the egui `{ active, settings }`
/// shape plus the folded-in seed picker).
// PARITY GAP: the egui `generate` field is `{ editor: { source }, backend }`
// and `seed` is a separate AppState field; this app's generate payload is
// flat `{ source, backend, soup_* }` and the seed picker lives inside
// `layout` — an egui-era blob's generate/seed sub-trees therefore reset to
// defaults instead of carrying over.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct AppState {
    #[serde(default)]
    pub(crate) style: style::StyleState,
    /// Raw `jc_layout_v1` payload (`Null` = key absent / reset to default).
    #[serde(default)]
    pub(crate) layout: serde_json::Value,
    #[serde(default)]
    pub(crate) camera: camera::CameraState,
    #[serde(default)]
    pub(crate) focus: camera::FocusState,
    #[serde(default)]
    pub(crate) metrics: metrics::Persisted,
    #[serde(default)]
    pub(crate) query: filter::QueryModel,
    #[serde(default)]
    pub(crate) filter_behavior: filter::FilterBehavior,
    /// Raw `jc_generate_v1` payload (`Null` = key absent).
    #[serde(default)]
    pub(crate) generate: serde_json::Value,
    #[serde(default)]
    pub(crate) timeline: timeline::PersistedKnobs,
    #[serde(default)]
    pub(crate) debug_view_mode: debug::ViewMode,
    /// Every other `jc_*` localStorage key, raw — the panel-kit workspace
    /// layout (`jc_layout_v3`, this app's egui `tiles`/`dock` analog), the
    /// server URL, panel prefs. Keyed by storage key so nothing is lost.
    #[serde(default)]
    pub(crate) extra: BTreeMap<String, String>,
}

/// Aggregate the live panel states (each panel's `GlobalSignal`s, plus the
/// raw localStorage payloads for the modules this file may not reach into).
/// Signal reads subscribe the calling scope, so a panel rendering the live
/// export (Instances) re-renders when any covered state mutates — the egui
/// "re-serialized every frame the panel is open" contract.
pub(crate) fn capture() -> AppState {
    let (cam, focus) = camera::state_snapshot();
    let (query, filter_behavior) = filter::state_snapshot();
    AppState {
        style: style::state_snapshot(),
        layout: raw_json("jc_layout_v1"),
        camera: cam,
        focus,
        metrics: metrics::state_snapshot(),
        query,
        filter_behavior,
        generate: raw_json("jc_generate_v1"),
        timeline: timeline::state_snapshot(),
        debug_view_mode: debug::state_snapshot(),
        extra: capture_extra(),
    }
}

/// Full replacement of the persisted state — the egui `*state = imported`
/// swap. Fans the struct out over the `jc_*` localStorage keys (removing
/// stale ones absent from the import), stamps a `source` timeline entry
/// (the ring survives in sessionStorage, like egui preserving the in-memory
/// ring across the swap), then reloads so every panel re-seeds from the new
/// keys.
pub(crate) fn apply(state: &AppState, source: &str) {
    style::state_restore(&state.style);
    camera::state_restore(&state.camera, &state.focus);
    metrics::state_restore(&state.metrics);
    filter::state_restore(&state.query, state.filter_behavior);
    timeline::state_restore(&state.timeline);
    debug::state_restore(state.debug_view_mode);
    set_raw_json("jc_layout_v1", &state.layout);
    set_raw_json("jc_generate_v1", &state.generate);

    let storage = LocalStorage::raw();
    for (key, val) in &state.extra {
        // Foreign keys never leak into storage — only this app's prefix.
        if key.starts_with(STATE_PREFIX) {
            let _ = storage.set_item(key, val);
        }
    }

    // Stale-key sweep: anything under the prefix that the import neither
    // covers nor carries must not survive the swap.
    let len = storage.length().unwrap_or(0);
    let mut stale = Vec::new();
    for i in 0..len {
        let Ok(Some(key)) = storage.key(i) else { continue };
        if key.starts_with(STATE_PREFIX)
            && !COVERED_KEYS.contains(&key.as_str())
            && !state.extra.contains_key(&key)
        {
            stale.push(key);
        }
    }
    for key in &stale {
        let _ = storage.remove_item(key);
    }

    SNAPSHOTS
        .write()
        .push_json(serde_json::to_string(state).unwrap_or_default(), source);
    reload_page();
}

/// Snapshot every *uncovered* `jc_*` localStorage key as raw strings.
fn capture_extra() -> BTreeMap<String, String> {
    let storage = LocalStorage::raw();
    let mut map = BTreeMap::new();
    let len = storage.length().unwrap_or(0);
    for i in 0..len {
        let Ok(Some(key)) = storage.key(i) else { continue };
        if !key.starts_with(STATE_PREFIX) || COVERED_KEYS.contains(&key.as_str()) {
            continue;
        }
        if let Ok(Some(val)) = storage.get_item(&key) {
            map.insert(key, val);
        }
    }
    map
}

/// Parse a covered localStorage key's raw payload as JSON (`Null` if absent
/// or unparseable — gloo stores every value as a serde_json string).
fn raw_json(key: &str) -> serde_json::Value {
    LocalStorage::raw()
        .get_item(key)
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or(serde_json::Value::Null)
}

/// Write a covered key's raw JSON payload back; `Null` removes the key
/// (the owning panel then boots from its own defaults).
fn set_raw_json(key: &str, value: &serde_json::Value) {
    let storage = LocalStorage::raw();
    if value.is_null() {
        let _ = storage.remove_item(key);
    } else {
        let _ = storage.set_item(key, &value.to_string());
    }
}

// --- export / import ----------------------------------------------------------------

/// Serialize the live state to a YAML document — the egui
/// `export_state_yaml` (same serializer crate, `serde_yml`).
pub(crate) fn export_yaml() -> String {
    serde_yml::to_string(&capture()).unwrap_or_else(|e| format!("# export error: {e}"))
}

/// Deserialize an `AppState` from YAML or JSON. Unknown fields are silently
/// ignored (serde default) so configs from other schema generations degrade
/// gracefully — the egui `import_state_yaml` contract.
pub(crate) fn import_str(text: &str) -> Result<AppState, String> {
    // JSON first (a YAML 1.1 parse of JSON can mangle e.g. unquoted `y`),
    // then YAML for the human-authored configs.
    serde_json::from_str(text)
        .or_else(|_| serde_yml::from_str(text))
        .map_err(|e| e.to_string())
}

// --- share-link codec (port of ui/share.rs) ------------------------------------------

/// Encode an [`AppState`] into a short, URL-fragment-safe hash string:
/// compact JSON → DEFLATE (miniz_oxide, level 9) → base64url (no padding).
/// Byte-identical pipeline to the egui codec; the payload schema is this
/// app's `AppState` (egui-era hashes still decode via the shared field
/// names + serde defaults, modulo the generate/seed gap noted on the type).
pub(crate) fn encode_share(state: &AppState) -> Result<String, String> {
    let json = serde_json::to_string(state).map_err(|e| format!("serialize: {e}"))?;
    let compressed = miniz_oxide::deflate::compress_to_vec(json.as_bytes(), DEFLATE_LEVEL);
    Ok(B64.encode(compressed))
}

/// Decode a hash produced by [`encode_share`]. Tolerates a leading `#`, an
/// `s=` / `#s=` fragment prefix, a whole pasted link, and whitespace.
pub(crate) fn decode_share(hash: &str) -> Result<AppState, String> {
    let cleaned = strip_fragment(hash);
    if cleaned.is_empty() {
        return Err("empty hash".to_string());
    }
    let compressed = B64
        .decode(cleaned.as_bytes())
        .map_err(|e| format!("base64 decode: {e}"))?;
    let json = miniz_oxide::inflate::decompress_to_vec(&compressed)
        .map_err(|e| format!("inflate: {e:?}"))?;
    serde_json::from_slice(&json).map_err(|e| format!("deserialize: {e}"))
}

/// Strip a leading `#`, an `s=` / `#s=` fragment prefix, and surrounding
/// whitespace so both a bare hash and a whole pasted link decode.
fn strip_fragment(input: &str) -> &str {
    let s = input.trim();
    // Whole URL pasted? Take everything after the last `#`.
    let s = match s.rsplit_once('#') {
        Some((_, frag)) => frag,
        None => s,
    };
    s.strip_prefix("s=").unwrap_or(s).trim()
}

// --- snapshot ring (port of state.rs::{StateSnapshot, SnapshotRing}) -----------------

/// One timeline entry — port of `state::StateSnapshot`.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct StateSnapshot {
    /// Unix epoch milliseconds at the moment of capture.
    pub(crate) timestamp_ms: u64,
    /// Short human-readable description of what caused the snapshot
    /// (e.g. "default", "import yaml", "restore @ …", "Style", "misc").
    pub(crate) source: String,
    /// Serialized [`AppState`] JSON — round-trips back through [`apply`].
    pub(crate) state_json: String,
}

/// Write-through ring of [`StateSnapshot`]s — port of `state::SnapshotRing`
/// (cap 50, oldest evicted on push), persisted to sessionStorage.
pub(crate) struct SnapshotRing {
    pub(crate) entries: Vec<StateSnapshot>,
    /// Cap on the timeline length.
    pub(crate) max: usize,
}

impl SnapshotRing {
    fn restore() -> Self {
        let entries: Vec<StateSnapshot> = SessionStorage::get(RING_KEY).unwrap_or_default();
        Self { entries, max: 50 }
    }

    pub(crate) fn push_json(&mut self, state_json: String, source: impl Into<String>) {
        self.entries.push(StateSnapshot {
            timestamp_ms: js_sys::Date::now() as u64,
            source: source.into(),
            state_json,
        });
        while self.entries.len() > self.max {
            self.entries.remove(0);
        }
        let _ = SessionStorage::set(RING_KEY, &self.entries);
    }
}

pub(crate) static SNAPSHOTS: GlobalSignal<SnapshotRing> = Signal::global(SnapshotRing::restore);

/// "Clear timeline" — leaves capacity untouched and reseeds a single
/// `cleared` snapshot of the current state so the panel never becomes empty
/// mid-session (egui contract).
pub(crate) fn clear_timeline() {
    let json = serde_json::to_string(&capture()).unwrap_or_default();
    let mut ring = SNAPSHOTS.write();
    ring.entries.clear();
    ring.push_json(json, "cleared");
}

/// Restore the ring entry at `idx`: decode its stored `AppState` JSON and
/// [`apply`] it tagged `restore @ <orig_timestamp>` — the restore itself
/// becomes a timeline event the user can undo (egui contract).
pub(crate) fn restore_snapshot(idx: usize) -> Result<(), String> {
    let entry = SNAPSHOTS
        .peek()
        .entries
        .get(idx)
        .cloned()
        .ok_or_else(|| "no such snapshot".to_string())?;
    let state: AppState = serde_json::from_str(&entry.state_json)
        .map_err(|e| format!("restore failed: {e}"))?;
    apply(
        &state,
        &format!("restore @ {}", format_timestamp_ms(entry.timestamp_ms)),
    );
    Ok(())
}

/// Format a Unix-epoch-millis timestamp as `HH:MM:SS.mmm` in UTC.
/// Tiny by-hand helper — same rationale as the egui original.
pub(crate) fn format_timestamp_ms(ms: u64) -> String {
    let secs = ms / 1000;
    let millis = ms % 1000;
    let h = (secs / 3600) % 24;
    let m = (secs / 60) % 60;
    let s = secs % 60;
    format!("{h:02}:{m:02}:{s:02}.{millis:03}")
}

// --- mutation attribution ------------------------------------------------------------

/// Best-effort attribution label for the next auto-snapshot — the egui
/// `AppState::snapshot_source`. Drained by the ticker every interval so a
/// stale label never mislabels a later unrelated diff.
static SNAPSHOT_SOURCE: GlobalSignal<Option<String>> = Signal::global(|| None);

/// Record a UI mutation for snapshot attribution + the frontend event log.
///
/// Mutation sites call this with the originating panel/source name and a
/// short human description of what changed (the egui app's
/// `snapshot_source` + `FrontendEventLog` pair): the snapshot label becomes
/// `"<source>: <what>"` and the event log gets `(source, what)` — exactly
/// the egui palette-execute shape (`"palette: {label}"` +
/// `push("palette", label)`, app.rs:1492). Must run inside the Dioxus
/// runtime (event handlers / spawned tasks) — every current call site does.
pub(crate) fn note_mutation(source: &'static str, what: &str) {
    *SNAPSHOT_SOURCE.write() = Some(format!("{source}: {what}"));
    debug::push_event(source, what.to_string());
}

/// Stamp `snapshot_source` only — for continuous mutations (slider drags)
/// where a log line per input event would flood the 500-entry log. Matches
/// egui, whose sections stamped the source every frame but only pushed
/// events for discrete actions (palette / section toggle / debug mode).
pub(crate) fn note_source(source: &'static str) {
    *SNAPSHOT_SOURCE.write() = Some(source.to_string());
}

// --- boot + auto-snapshot ticker -------------------------------------------------------

static INIT: GlobalSignal<bool> = Signal::global(|| false);

/// One-shot boot, called from every owned panel's render entry (whichever
/// mounts first arms it — Nodes is open in the default layout, so this
/// effectively runs at launch):
///
/// 1. arm the long-lived panel loops the egui App ran from launch
///    (timeline capture, style apply, camera follow/fit);
/// 2. `#s=<hash>` share-fragment bootstrap (decode → apply → reload, with
///    the fragment stripped first so the reload can't loop);
/// 3. `?config=<name>` dev-server preset bootstrap (`GET /configs/<name>`,
///    YAML → apply — the link shape graph-api's /configs comment names);
/// 4. start the auto-snapshot ticker (port of `App::tick_snapshots`),
///    which first seeds the session ring with `default` + `restored`
///    (the egui `App::new` timeline contract).
pub(crate) fn ensure_init() {
    if *INIT.peek() {
        return;
    }
    *INIT.write() = true;

    // Arm the long-lived panel loops the egui App ran from launch inside
    // its update loop: timeline frame capture, style apply, camera
    // follow/fit. The first-rendering panel is this app's effective launch.
    timeline::ensure_ticker();
    style::ensure_init();
    camera::ensure_init();

    // Share-link bootstrap: a `#s=<hash>` fragment OVERRIDES the persisted
    // state (egui App::new). Strip the consumed fragment before `apply`'s
    // reload so the imported state isn't re-imported forever.
    if let Some(hash) = share_fragment_from_location() {
        match decode_share(&hash) {
            Ok(state) => {
                strip_share_fragment();
                apply(&state, "share fragment");
                return; // reloading
            }
            Err(e) => tracing::warn!("[appstate] #s= share fragment decode failed: {e}"),
        }
    } else if let Some(name) = config_param_from_location() {
        // Preset bootstrap — async fetch, then the same strip → apply →
        // reload dance. `spawn_forever`: the arming panel may unmount.
        spawn_forever(async move {
            match fetch_config(&name).await.and_then(|yaml| import_str(&yaml)) {
                Ok(state) => {
                    strip_config_param();
                    apply(&state, "import yaml");
                }
                Err(e) => tracing::warn!("[appstate] ?config={name} preset failed: {e}"),
            }
        });
    }

    // Auto-snapshot ticker — port of `App::tick_snapshots`: diff the
    // serialized state on the egui debounce cadence, push a ring entry on
    // change labelled by the drained `snapshot_source` (fallback "misc").
    // First observation seeds the hash without pushing (the `default` /
    // `restored` entries below cover the starting state). Runs as a task
    // (not in the arming panel's render scope) so `capture`'s signal reads
    // don't subscribe that panel to every covered signal.
    spawn_forever(async move {
        // Ring seeding — the egui `App::new` timeline contract. Session-
        // scoped: a reload (e.g. the one `apply` issues) keeps the existing
        // entries, exactly like egui's in-memory ring surviving the
        // `*state = imported` swap.
        {
            let default_json = serde_json::to_string(&AppState::default()).unwrap_or_default();
            let live_json = serde_json::to_string(&capture()).unwrap_or_default();
            let mut ring = SNAPSHOTS.write();
            if ring.entries.is_empty() {
                ring.push_json(default_json, "default");
                ring.push_json(live_json, "restored");
            }
        }
        let mut prev: Option<u64> = None;
        loop {
            gloo_timers::future::TimeoutFuture::new(SNAPSHOT_INTERVAL_MS).await;
            // Drain attribution up-front, diff or not, so a label from a
            // no-op mutation can't bleed onto a later unrelated change.
            let drained = SNAPSHOT_SOURCE.write().take();
            let json = serde_json::to_string(&capture()).unwrap_or_default();
            let hash = {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                json.hash(&mut h);
                h.finish()
            };
            let changed = prev.is_some() && prev != Some(hash);
            prev = Some(hash);
            if changed {
                SNAPSHOTS
                    .write()
                    .push_json(json, drained.unwrap_or_else(|| "misc".to_string()));
            }
        }
    });
}

// --- browser plumbing -------------------------------------------------------------------
// Reflect/eval-based instead of typed web_sys calls: the crate's web-sys
// feature set is frozen with Cargo.toml and Location/History are not in it
// (same rationale as the Instances panel's plumbing).

fn js_get(obj: &wasm_bindgen::JsValue, key: &str) -> Option<wasm_bindgen::JsValue> {
    js_sys::Reflect::get(obj, &wasm_bindgen::JsValue::from_str(key))
        .ok()
        .filter(|v| !v.is_undefined() && !v.is_null())
}

fn location_part(key: &str) -> Option<String> {
    let win = web_sys::window()?;
    let win: &wasm_bindgen::JsValue = win.as_ref();
    js_get(win, "location").and_then(|l| js_get(&l, key))?.as_string()
}

pub(crate) fn reload_page() {
    let _ = js_sys::eval("window.location.reload()");
}

/// The page's `#s=<hash>` URL fragment, if present and non-empty.
fn share_fragment_from_location() -> Option<String> {
    let hash = location_part("hash")?; // e.g. "#s=AbCd"
    let raw = hash.trim_start_matches('#');
    let cleaned = strip_fragment(&hash);
    if raw.starts_with(&format!("{FRAGMENT_KEY}=")) && !cleaned.is_empty() {
        Some(cleaned.to_string())
    } else {
        None
    }
}

/// The page's `?config=<name>` query param, if present and non-empty.
fn config_param_from_location() -> Option<String> {
    let search = location_part("search")?; // e.g. "?config=lavender"
    for kv in search.trim_start_matches('?').split('&') {
        if let Some((k, v)) = kv.split_once('=') {
            if k == CONFIG_PARAM && !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// Remove the consumed `#s=` fragment from the URL (history.replaceState,
/// no navigation) so the post-apply reload boots clean.
fn strip_share_fragment() {
    let _ = js_sys::eval(
        "history.replaceState(null, '', window.location.pathname + window.location.search)",
    );
}

/// Remove the consumed `?config=` param from the URL, keeping everything else.
fn strip_config_param() {
    let _ = js_sys::eval(
        "(function(){var u = new URL(window.location.href);\
          u.searchParams.delete('config');\
          history.replaceState(null, '', u.toString());})()",
    );
}

/// `GET /configs/:name` — one preset's YAML as plain text.
async fn fetch_config(name: &str) -> Result<String, String> {
    let path = format!("/configs/{name}");
    let resp = gloo_net::http::Request::get(&crate::api::url(&path))
        .send()
        .await
        .map_err(crate::api::err)?;
    if !resp.ok() {
        return Err(format!("{} -> HTTP {}", path, resp.status()));
    }
    resp.text().await.map_err(crate::api::err)
}
