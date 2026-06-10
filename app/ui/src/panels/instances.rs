//! Instances panel — Dioxus port of crates/graph-renderer/src/ui/sections/instances.rs.
//!
//! The egui section is the AppState round-trip surface: state timeline
//! (snapshot ring + per-entry Restore), share link codec, YAML export/import
//! with file download/upload + dev-server presets (`GET /configs`), the
//! two-step reset, and the read-only ActionInstance cards. This app's state
//! is the union of the `jc_*` localStorage keys (panel-kit layout under
//! "jc_layout" + every panel's settings key), so the round-trip payload is a
//! flat key→value JSON object instead of the egui `AppState` schema.
//!
//! Panel-local state lives in `GlobalSignal`s inside this module (not on
//! `crate::Ctx`) so each panel file is self-contained.

use dioxus::prelude::*;
use gloo_storage::{LocalStorage, SessionStorage, Storage};
use serde::{Deserialize, Serialize};
use wasm_bindgen::{JsCast, JsValue};

use crate::api::{configs, err, url, ConfigEntry};
use crate::Ctx;

/// localStorage prefix that scopes this app's persisted state: the panel-kit
/// layout ("jc_layout") plus every panel's "jc_<panel>_v1" settings key.
const STATE_PREFIX: &str = "jc_";

/// sessionStorage key for the snapshot ring. sessionStorage (not local) so
/// the ring survives the reload that applying an import requires while
/// staying per-session — the same lifetime as egui's in-memory ring. Living
/// outside localStorage also keeps it out of the captured state, the
/// equivalent of the egui ring's `#[serde(skip)]` (no recursive ring bloat).
const RING_KEY: &str = "jc_instances_ring";

/// localStorage key for the panel's persisted settings.
const PREFS_KEY: &str = "jc_instances_v1";

/// URL-fragment key the encoded state rides under: `#s=<hash>` (same key as
/// the egui share codec, though the payloads are not interchangeable — see
/// the PARITY GAP on [`encode_share`]).
const FRAGMENT_KEY: &str = "s";

// --- panel-local state ---------------------------------------------------------

/// Persisted user-facing settings (everything else in this panel is UI
/// scratch — `#[serde(skip)]` in the egui AppState — and stays session-only).
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
struct Prefs {
    /// Last preset viewed via the Presets row, re-fetched on next open.
    last_preset: Option<String>,
}

impl Prefs {
    fn restore() -> Self {
        LocalStorage::get(PREFS_KEY).unwrap_or_default()
    }
    fn save(&self) {
        let _ = LocalStorage::set(PREFS_KEY, self);
    }
}

/// One timeline entry — port of `state::StateSnapshot`.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
struct StateSnapshot {
    /// Unix epoch milliseconds at the moment of capture.
    timestamp_ms: u64,
    /// Short human-readable description of what caused the snapshot
    /// (e.g. "default", "import json", "restore @ …", "misc").
    source: String,
    /// Captured state: compact JSON object of every `jc_*` localStorage key.
    state_json: String,
}

/// Write-through ring of [`StateSnapshot`]s — port of `state::SnapshotRing`.
struct SnapshotRing {
    entries: Vec<StateSnapshot>,
    /// Cap on the timeline length. Oldest evicted on push.
    max: usize,
}

impl SnapshotRing {
    /// Session restore; an empty ring is seeded with a "default" capture so
    /// the panel shows an entry the instant it opens (egui boot contract).
    fn restore() -> Self {
        let entries: Vec<StateSnapshot> = SessionStorage::get(RING_KEY).unwrap_or_default();
        let mut ring = Self { entries, max: 50 };
        if ring.entries.is_empty() {
            ring.push_json(compact_state_json(), "default");
        }
        ring
    }

    fn push_json(&mut self, state_json: String, source: impl Into<String>) {
        self.entries.push(StateSnapshot {
            timestamp_ms: now_ms(),
            source: source.into(),
            state_json,
        });
        while self.entries.len() > self.max {
            self.entries.remove(0);
        }
        let _ = SessionStorage::set(RING_KEY, &self.entries);
    }
}

#[derive(Clone, PartialEq)]
enum PresetList {
    Idle,
    Loading,
    Ready(Vec<ConfigEntry>),
    Failed(String),
}

#[derive(Clone, PartialEq)]
struct PresetView {
    name: String,
    /// None while the fetch is in flight.
    content: Option<Result<String, String>>,
}

static SNAPSHOTS: GlobalSignal<SnapshotRing> = Signal::global(SnapshotRing::restore);
static PREFS: GlobalSignal<Prefs> = Signal::global(Prefs::restore);
static IMPORT_BUF: GlobalSignal<String> = Signal::global(String::new);
static IMPORT_ERR: GlobalSignal<Option<String>> = Signal::global(|| None);
static RESET_ARMED: GlobalSignal<bool> = Signal::global(|| false);
static SHARE_BUF: GlobalSignal<String> = Signal::global(String::new);
static SHARE_IMPORT_BUF: GlobalSignal<String> = Signal::global(String::new);
static SHARE_ERR: GlobalSignal<Option<String>> = Signal::global(|| None);
static PRESETS: GlobalSignal<PresetList> = Signal::global(|| PresetList::Idle);
static PRESET_VIEW: GlobalSignal<Option<PresetView>> = Signal::global(|| None);
static TICKER: GlobalSignal<bool> = Signal::global(|| false);

// --- state capture / apply -------------------------------------------------------

/// Snapshot every `jc_*` localStorage key as a flat key→raw-value object.
/// Values stay raw strings: panels encode their own JSON, and the capture
/// must not re-interpret (or normalize) them.
fn capture_map() -> serde_json::Map<String, serde_json::Value> {
    let storage = LocalStorage::raw();
    let mut map = serde_json::Map::new();
    let len = storage.length().unwrap_or(0);
    for i in 0..len {
        let Ok(Some(key)) = storage.key(i) else { continue };
        if !key.starts_with(STATE_PREFIX) {
            continue;
        }
        if let Ok(Some(val)) = storage.get_item(&key) {
            map.insert(key, serde_json::Value::String(val));
        }
    }
    map
}

/// Compact form — the snapshot/share payload (diffed by the auto-snapshot
/// ticker, so it must be deterministic; serde_json's BTreeMap-backed object
/// sorts keys).
fn compact_state_json() -> String {
    serde_json::Value::Object(capture_map()).to_string()
}

/// Pretty form — the live export textarea / Copy / Download payload.
// PARITY GAP: egui exports AppState as YAML (export_state_yaml). serde_yaml
// is not a dependency of this crate and Cargo.toml is frozen, so the
// round-trip format here is JSON. Import accepts JSON only for the same
// reason.
fn pretty_state_json() -> String {
    serde_json::to_string_pretty(&serde_json::Value::Object(capture_map()))
        .unwrap_or_else(|e| format!("// export error: {e}"))
}

/// Full replacement of the persisted state, like the egui `*state = imported`
/// swap: stale `jc_*` keys absent from the import must not survive.
fn apply_state_json(json: &str) -> Result<(), String> {
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("parse: {e}"))?;
    let obj = value
        .as_object()
        .ok_or_else(|| "expected a top-level JSON object".to_string())?;
    let storage = LocalStorage::raw();
    let len = storage.length().unwrap_or(0);
    let mut stale = Vec::new();
    for i in 0..len {
        if let Ok(Some(key)) = storage.key(i) {
            if key.starts_with(STATE_PREFIX) && !obj.contains_key(&key) {
                stale.push(key);
            }
        }
    }
    for key in &stale {
        let _ = storage.remove_item(key);
    }
    for (key, val) in obj {
        // Foreign keys never leak into storage — only this app's prefix.
        if !key.starts_with(STATE_PREFIX) {
            continue;
        }
        let raw = val.as_str().map(str::to_owned).unwrap_or_else(|| val.to_string());
        let _ = storage.set_item(key, &raw);
    }
    Ok(())
}

/// Apply imported state, stamp the timeline, reload. The reload is this
/// app's equivalent of egui's live `*state = imported` swap: every panel's
/// GlobalSignals and the panel-kit layout read localStorage once at boot, so
/// a fresh boot is the only way to re-seed them from outside their modules.
/// The ring survives in sessionStorage (egui preserves it across the swap).
fn apply_import(json: &str, source: &str) -> Result<(), String> {
    apply_state_json(json)?;
    SNAPSHOTS.write().push_json(compact_state_json(), source);
    reload_page();
    Ok(())
}

// --- share codec -----------------------------------------------------------------

/// Encode the current state into a URL-fragment-safe token.
// PARITY GAP: the egui codec is JSON → DEFLATE (miniz_oxide) → base64url.
// miniz_oxide/base64 are not dependencies of this crate (Cargo.toml frozen),
// so this is base64url over the uncompressed JSON: tokens are longer and not
// interchangeable with the egui app's `#s=` hashes (the state schemas differ
// anyway — flat jc_* map here vs AppState there).
fn encode_share() -> String {
    b64url_encode(compact_state_json().as_bytes())
}

/// Decode a token produced by [`encode_share`] back into the state JSON.
fn decode_share(hash: &str) -> Result<String, String> {
    let cleaned = strip_fragment(hash);
    if cleaned.is_empty() {
        return Err("empty hash".to_string());
    }
    let bytes = b64url_decode(cleaned)?;
    String::from_utf8(bytes).map_err(|e| format!("utf8: {e}"))
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

/// base64url alphabet, no padding — fragment-safe (`-`/`_`, no `+`/`/`/`=`).
/// Hand-rolled because the base64 crate is not a dependency of this crate.
const B64_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn b64url_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(B64_ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(B64_ALPHABET[(n >> 12) as usize & 63] as char);
        if chunk.len() > 1 {
            out.push(B64_ALPHABET[(n >> 6) as usize & 63] as char);
        }
        if chunk.len() > 2 {
            out.push(B64_ALPHABET[n as usize & 63] as char);
        }
    }
    out
}

fn b64url_decode(s: &str) -> Result<Vec<u8>, String> {
    fn val(c: u8) -> Result<u32, String> {
        match c {
            b'A'..=b'Z' => Ok(u32::from(c - b'A')),
            b'a'..=b'z' => Ok(u32::from(c - b'a') + 26),
            b'0'..=b'9' => Ok(u32::from(c - b'0') + 52),
            b'-' => Ok(62),
            b'_' => Ok(63),
            _ => Err(format!("base64 decode: invalid byte {c:#04x}")),
        }
    }
    let bytes = s.as_bytes();
    if bytes.len() % 4 == 1 {
        return Err("base64 decode: truncated input".to_string());
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3 + 2);
    for chunk in bytes.chunks(4) {
        let mut n = 0u32;
        for &c in chunk {
            n = (n << 6) | val(c)?;
        }
        n <<= 6 * (4 - chunk.len() as u32);
        out.push((n >> 16) as u8);
        if chunk.len() > 2 {
            out.push((n >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(n as u8);
        }
    }
    Ok(out)
}

// --- browser plumbing --------------------------------------------------------------
// Reflect-based instead of typed web_sys calls: the crate's web-sys feature
// set (Window/Document/Element/…) is frozen with Cargo.toml, and Location/
// Clipboard/HtmlElement are not in it.

fn now_ms() -> u64 {
    js_sys::Date::now() as u64
}

fn js_get(obj: &JsValue, key: &str) -> Option<JsValue> {
    js_sys::Reflect::get(obj, &JsValue::from_str(key))
        .ok()
        .filter(|v| !v.is_undefined() && !v.is_null())
}

/// `navigator.clipboard.writeText(text)` — fire-and-forget, like the egui
/// `ui.output_mut(|o| o.copied_text = …)` path.
fn copy_to_clipboard(text: &str) {
    let Some(win) = web_sys::window() else { return };
    let win: &JsValue = win.as_ref();
    let Some(clip) = js_get(win, "navigator").and_then(|n| js_get(&n, "clipboard")) else {
        return;
    };
    if let Some(f) = js_get(&clip, "writeText").and_then(|f| f.dyn_into::<js_sys::Function>().ok())
    {
        let _ = f.call1(&clip, &JsValue::from_str(text));
    }
}

/// The page origin (`https://host`), or `None` — the panel then shows the
/// bare hash instead of a full link (egui native fallback).
fn page_origin() -> Option<String> {
    let win = web_sys::window()?;
    let win: &JsValue = win.as_ref();
    js_get(win, "location").and_then(|l| js_get(&l, "origin"))?.as_string()
}

fn reload_page() {
    let _ = js_sys::eval("window.location.reload()");
}

/// Anchor-triggered data-URL download (the egui port uses a Blob; data URLs
/// avoid the Blob/Url web-sys features).
fn download_text(filename: &str, mime: &str, contents: &str) -> Result<(), String> {
    let doc = web_sys::window()
        .and_then(|w| w.document())
        .ok_or("no document")?;
    let anchor = doc
        .create_element("a")
        .map_err(|e| format!("create anchor: {e:?}"))?;
    let href = format!("data:{mime};charset=utf-8,{}", urlencoding::encode(contents));
    anchor
        .set_attribute("href", &href)
        .map_err(|e| format!("href: {e:?}"))?;
    anchor
        .set_attribute("download", filename)
        .map_err(|e| format!("download: {e:?}"))?;
    let target: &JsValue = anchor.as_ref();
    let click = js_get(target, "click")
        .and_then(|f| f.dyn_into::<js_sys::Function>().ok())
        .ok_or("no click()")?;
    click.call0(target).map_err(|e| format!("click: {e:?}"))?;
    Ok(())
}

/// Format a Unix-epoch-millis timestamp as `HH:MM:SS.mmm` in UTC.
/// Tiny by-hand helper — same rationale as the egui original.
fn format_timestamp_ms(ms: u64) -> String {
    let secs = ms / 1000;
    let millis = ms % 1000;
    let h = (secs / 3600) % 24;
    let m = (secs / 60) % 60;
    let s = secs % 60;
    format!("{h:02}:{m:02}:{s:02}.{millis:03}")
}

// --- endpoint helpers --------------------------------------------------------------

/// `GET /configs/:name` — one preset's YAML as plain text (api.rs has no
/// text fetch, so this stays private to the panel).
async fn get_text(path: &str) -> Result<String, String> {
    let resp = gloo_net::http::Request::get(&url(path))
        .send()
        .await
        .map_err(err)?;
    if !resp.ok() {
        return Err(format!("{} -> HTTP {}", path, resp.status()));
    }
    resp.text().await.map_err(err)
}

fn view_preset(name: String) {
    {
        let mut prefs = PREFS.peek().clone();
        if prefs.last_preset.as_deref() != Some(name.as_str()) {
            prefs.last_preset = Some(name.clone());
            prefs.save();
            *PREFS.write() = prefs;
        }
    }
    *PRESET_VIEW.write() = Some(PresetView { name: name.clone(), content: None });
    spawn(async move {
        let res = get_text(&format!("/configs/{name}")).await;
        let mut view = PRESET_VIEW.write();
        if let Some(v) = view.as_mut() {
            // Drop stale responses if the user clicked another preset.
            if v.name == name {
                v.content = Some(res);
            }
        }
    });
}

/// One-shot lazy init: preset list fetch, last-viewed preset restore, and
/// the auto-snapshot ticker (the egui app stamps a snapshot from
/// `tick_snapshots` whenever the state hash changes; the ticker is this
/// app's frame loop).
fn init_once() {
    if matches!(*PRESETS.peek(), PresetList::Idle) {
        *PRESETS.write() = PresetList::Loading;
        spawn(async move {
            *PRESETS.write() = match configs().await {
                Ok(list) => PresetList::Ready(list),
                Err(e) => PresetList::Failed(e),
            };
        });
        if let Some(name) = PREFS.peek().last_preset.clone() {
            view_preset(name);
        }
    }
    if !*TICKER.peek() {
        *TICKER.write() = true;
        spawn(async move {
            // PARITY GAP: egui mutation sites attribute each auto-snapshot
            // ("Style", "palette: …") via `snapshot_source`. Sibling panels
            // cannot reach into this module, so cross-panel diffs all stamp
            // as "misc" (the egui fallback label).
            loop {
                gloo_timers::future::TimeoutFuture::new(1500).await;
                let json = compact_state_json();
                let differs = SNAPSHOTS
                    .peek()
                    .entries
                    .last()
                    .map(|e| e.state_json != json)
                    .unwrap_or(true);
                if differs {
                    SNAPSHOTS.write().push_json(json, "misc");
                }
            }
        });
    }
}

// --- panel ---------------------------------------------------------------------

pub fn panel(_ctx: Ctx) -> Element {
    init_once();
    // Order matters: same rationale as the egui section — the State timeline
    // is the most-frequently-useful sub-region, so it sits ABOVE the bulky
    // import/export block.
    rsx! {
        div { class: "inst",
            { timeline_section() }
            hr { class: "inst-sep" }
            { share_section() }
            hr { class: "inst-sep" }
            { io_section() }
            hr { class: "inst-sep" }
            { actions_section() }
        }
    }
}

/// State timeline (newest first) with one Restore per row plus a footer with
/// capacity + clear — port of `state_timeline_panel`.
fn timeline_section() -> Element {
    let (len, cap, entries) = {
        let ring = SNAPSHOTS.read();
        let rows: Vec<(usize, StateSnapshot)> =
            ring.entries.iter().cloned().enumerate().rev().collect();
        (ring.entries.len(), ring.max, rows)
    };
    rsx! {
        div { class: "inst-subhead", "State timeline" }
        div { class: "inst-row",
            span { class: "inst-cap", "{len} / {cap}" }
            span { class: "inst-flex" }
            button {
                class: "btn inst-small",
                // Leaves capacity untouched and reseeds a single "cleared"
                // snapshot of the current state so the panel never becomes
                // empty mid-session (egui contract).
                onclick: move |_| {
                    let mut ring = SNAPSHOTS.write();
                    ring.entries.clear();
                    ring.push_json(compact_state_json(), "cleared");
                },
                "Clear timeline"
            }
        }
        if entries.is_empty() {
            div { class: "inst-hint", "Timeline empty." }
        } else {
            div { class: "inst-timeline",
                for (idx, entry) in entries {
                    div { key: "{entry.timestamp_ms}-{idx}", class: "inst-row",
                        span { class: "inst-ts", { format_timestamp_ms(entry.timestamp_ms) } }
                        span { class: "inst-src", "{entry.source}" }
                        span { class: "inst-flex" }
                        button {
                            class: "btn inst-small",
                            onclick: move |_| restore_snapshot(idx),
                            "Restore"
                        }
                    }
                }
            }
        }
    }
}

/// Restore stamps a `restore @ <orig_timestamp>` entry before the reload —
/// the restore itself becomes a timeline event the user can undo.
fn restore_snapshot(idx: usize) {
    let entry = SNAPSHOTS.peek().entries.get(idx).cloned();
    let Some(entry) = entry else { return };
    match apply_state_json(&entry.state_json) {
        Ok(()) => {
            SNAPSHOTS.write().push_json(
                compact_state_json(),
                format!("restore @ {}", format_timestamp_ms(entry.timestamp_ms)),
            );
            reload_page();
        }
        Err(e) => *IMPORT_ERR.write() = Some(format!("restore failed: {e}")),
    }
}

/// Shareable hash / link sub-region — port of `share_link_panel`.
fn share_section() -> Element {
    let buf = SHARE_BUF.read().clone();
    let import = SHARE_IMPORT_BUF.read().clone();
    let error = SHARE_ERR.read().clone();
    let has_buf = !buf.is_empty();
    let has_import = !import.trim().is_empty();
    rsx! {
        div { class: "inst-subhead", "Share link" }
        div { class: "inst-row",
            button {
                class: "btn",
                title: "Encode the entire UI state into a hash and copy a shareable link",
                onclick: move |_| {
                    let hash = encode_share();
                    let link = page_origin()
                        .map(|o| format!("{o}/#{FRAGMENT_KEY}={hash}"))
                        .unwrap_or(hash);
                    copy_to_clipboard(&link);
                    *SHARE_BUF.write() = link;
                },
                "Copy share link"
            }
            button {
                class: "btn inst-small",
                disabled: !has_buf,
                title: "Clear",
                onclick: move |_| SHARE_BUF.write().clear(),
                "✕"
            }
        }
        if has_buf {
            textarea { class: "inst-code", readonly: true, rows: "2", value: "{buf}" }
        }
        div { class: "inst-subhead", "Load from link / hash" }
        input {
            class: "inst-line",
            placeholder: "Paste a share link or #s=<hash>, then Load.",
            value: "{import}",
            oninput: move |e| *SHARE_IMPORT_BUF.write() = e.value(),
        }
        div { class: "inst-row",
            button {
                class: "btn",
                disabled: !has_import,
                onclick: move |_| {
                    let pasted = SHARE_IMPORT_BUF.peek().clone();
                    match decode_share(&pasted).and_then(|json| apply_import(&json, "load share link")) {
                        Ok(()) => *SHARE_ERR.write() = None,
                        Err(e) => *SHARE_ERR.write() = Some(e),
                    }
                },
                "Load"
            }
            button {
                class: "btn",
                onclick: move |_| {
                    SHARE_IMPORT_BUF.write().clear();
                    *SHARE_ERR.write() = None;
                },
                "Clear"
            }
        }
        if let Some(e) = error {
            div { class: "inst-err", "Decode error: {e}" }
        }
    }
}

/// Import / Export sub-region — port of `yaml_io_panel` (JSON payloads here;
/// see the PARITY GAP on [`pretty_state_json`]).
fn io_section() -> Element {
    // Live mirror of the current state: re-serialized every render the panel
    // is open, so what you see / copy / download is never a stale snapshot.
    let live = pretty_state_json();
    let live_copy = live.clone();
    let live_dl = live.clone();
    let import = IMPORT_BUF.read().clone();
    let has_import = !import.trim().is_empty();
    let import_err = IMPORT_ERR.read().clone();
    let armed = *RESET_ARMED.read();
    rsx! {
        div { class: "inst-subhead", "Import / Export state" }
        div { class: "inst-row",
            span { class: "inst-live", "Live state" }
            button {
                class: "btn",
                title: "Copy the current state as JSON",
                onclick: move |_| copy_to_clipboard(&live_copy),
                "Copy"
            }
            button {
                class: "btn",
                title: "Download the entire current app state as a .json file",
                onclick: move |_| {
                    if let Err(e) = download_text(
                        "jump-cannon-appstate.json",
                        "application/json",
                        &live_dl,
                    ) {
                        *IMPORT_ERR.write() = Some(format!("download: {e}"));
                    }
                },
                "⬇ File"
            }
        }
        textarea { class: "inst-code", readonly: true, rows: "12", value: "{live}" }

        div { class: "inst-subhead", "Paste JSON to import" }
        textarea {
            class: "inst-code",
            rows: "12",
            placeholder: "Paste an exported state JSON object here, then click Load.",
            value: "{import}",
            oninput: move |e| *IMPORT_BUF.write() = e.value(),
        }
        div { class: "inst-row",
            button {
                class: "btn",
                disabled: !has_import,
                onclick: move |_| {
                    let text = IMPORT_BUF.peek().clone();
                    match apply_import(&text, "import json") {
                        Ok(()) => *IMPORT_ERR.write() = None,
                        Err(e) => *IMPORT_ERR.write() = Some(e),
                    }
                },
                "Load"
            }
            button {
                class: "btn",
                onclick: move |_| {
                    IMPORT_BUF.write().clear();
                    *IMPORT_ERR.write() = None;
                },
                "Clear"
            }
        }
        if let Some(e) = import_err {
            div { class: "inst-err", "Parse error: {e}" }
        }

        div { class: "inst-subhead", "Load from file or dev-server preset" }
        div { class: "inst-presets",
            label { class: "btn",
                title: "Pick an exported .json state file and load the full app state from it",
                "⬆ Upload .json"
                input {
                    r#type: "file",
                    accept: ".json,application/json",
                    style: "display:none",
                    onchange: move |evt| {
                        if let Some(engine) = evt.files() {
                            spawn(async move {
                                for name in engine.files() {
                                    match engine.read_file_to_string(&name).await {
                                        Some(text) => {
                                            if let Err(e) = apply_import(&text, "import file") {
                                                *IMPORT_ERR.write() =
                                                    Some(format!("upload parse: {e}"));
                                            }
                                        }
                                        None => {
                                            *IMPORT_ERR.write() =
                                                Some("upload: read failed".to_string());
                                        }
                                    }
                                }
                            });
                        }
                    },
                }
            }
            span { class: "inst-cap", "·" }
            span { class: "inst-src", "Presets:" }
            { presets_row() }
        }
        { preset_viewer() }

        div { class: "inst-row",
            // Two-step reset: first click arms, second commits. Wipes every
            // "jc_*" key, stamps the reset, reloads — the timeline survives
            // in sessionStorage so the user can roll back (egui preserves
            // the ring across the swap for the same reason).
            button {
                class: if armed { "btn inst-danger armed" } else { "btn inst-danger" },
                onclick: move |_| {
                    if *RESET_ARMED.peek() {
                        let storage = LocalStorage::raw();
                        let len = storage.length().unwrap_or(0);
                        let mut keys = Vec::new();
                        for i in 0..len {
                            if let Ok(Some(k)) = storage.key(i) {
                                if k.starts_with(STATE_PREFIX) {
                                    keys.push(k);
                                }
                            }
                        }
                        for k in &keys {
                            let _ = storage.remove_item(k);
                        }
                        SNAPSHOTS.write().push_json(compact_state_json(), "reset to defaults");
                        reload_page();
                    } else {
                        *RESET_ARMED.write() = true;
                    }
                },
                { if armed { "Confirm reset" } else { "Reset to defaults" } }
            }
            if armed {
                button {
                    class: "btn inst-small",
                    onclick: move |_| *RESET_ARMED.write() = false,
                    "Cancel"
                }
            }
        }
    }
}

/// Preset buttons from `GET /configs` (egui hardcodes `PRESET_NAMES`; the
/// server lists the same directory, so the dynamic list is the same set).
fn presets_row() -> Element {
    match PRESETS.read().clone() {
        PresetList::Idle | PresetList::Loading => rsx! {
            span { class: "inst-hint", "fetching presets…" }
        },
        PresetList::Failed(e) => rsx! {
            span { class: "inst-err", "presets: {e}" }
            button {
                class: "btn inst-small",
                onclick: move |_| {
                    // Re-arm the lazy fetch; the next render re-runs it.
                    *PRESETS.write() = PresetList::Idle;
                },
                "Retry"
            }
        },
        PresetList::Ready(list) if list.is_empty() => rsx! {
            span { class: "inst-hint", "none (dev server only)" }
        },
        PresetList::Ready(list) => rsx! {
            for entry in list {
                {
                    let name = entry.name.clone();
                    let title = entry
                        .description
                        .clone()
                        .unwrap_or_else(|| format!("View /configs/{name} from the dev server"));
                    rsx! {
                        button {
                            key: "{entry.name}",
                            class: "btn inst-small",
                            title: "{title}",
                            onclick: move |_| view_preset(name.clone()),
                            "{entry.name}"
                        }
                    }
                }
            }
        },
    }
}

/// Read-only viewer for the selected preset's YAML.
// PARITY GAP: egui applies the fetched preset to the live AppState
// (apply_imported_yaml). The preset YAML targets the egui AppState schema,
// which does not map 1:1 onto this app's jc_* key map, so the content is
// rendered read-only and there is no 1:1 import path yet.
fn preset_viewer() -> Element {
    let Some(view) = PRESET_VIEW.read().clone() else {
        return rsx! {};
    };
    rsx! {
        div { class: "inst-row",
            span { class: "inst-live", "Preset: {view.name}" }
            span { class: "inst-flex" }
            button {
                class: "btn inst-small",
                title: "Close preset view",
                onclick: move |_| *PRESET_VIEW.write() = None,
                "✕"
            }
        }
        match view.content {
            None => rsx! { div { class: "inst-hint", "fetching…" } },
            Some(Ok(yaml)) => rsx! {
                textarea { class: "inst-code", readonly: true, rows: "12", value: "{yaml}" }
                div { class: "inst-hint",
                    "Preset YAML targets the egui AppState schema — view-only here."
                }
            },
            Some(Err(e)) => rsx! { div { class: "inst-err", "preset fetch: {e}" } },
        }
    }
}

/// ActionInstance cards sub-region.
// PARITY GAP: the egui section renders the command-palette ActionRegistry's
// recorded instances (title + #id rows, Params list, read-only state JSON,
// per-card remove). This app has no command palette / ActionRegistry yet, so
// only the empty-state hint is reproducible.
fn actions_section() -> Element {
    rsx! {
        div { class: "inst-hint",
            "No action instances yet. Press Ctrl+P to open the command palette."
        }
    }
}
