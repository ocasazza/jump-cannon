//! Instances panel — Dioxus port of crates/graph-renderer/src/ui/sections/instances.rs.
//!
//! The egui section is the AppState round-trip surface: state timeline
//! (snapshot ring + per-entry Restore), share link codec, YAML export/import
//! with file download/upload + dev-server presets (`GET /configs`), the
//! two-step reset, and the read-only ActionInstance cards. The round-trip
//! machinery itself (the typed `AppState`, the codec, the ring, the boot
//! hooks) lives in `crate::appstate`; this panel is its UI.
//!
//! Panel-local state lives in `GlobalSignal`s inside this module (not on
//! `crate::Ctx`) so each panel file is self-contained.

use dioxus::prelude::*;
use gloo_storage::{LocalStorage, Storage};
use serde::{Deserialize, Serialize};
use wasm_bindgen::{JsCast, JsValue};

use crate::api::{configs, err, url, ConfigEntry};
use crate::appstate;
use crate::Ctx;

/// localStorage key for the panel's persisted settings.
const PREFS_KEY: &str = "jc_instances_v1";

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

static PREFS: GlobalSignal<Prefs> = Signal::global(Prefs::restore);
static IMPORT_BUF: GlobalSignal<String> = Signal::global(String::new);
static IMPORT_ERR: GlobalSignal<Option<String>> = Signal::global(|| None);
static RESET_ARMED: GlobalSignal<bool> = Signal::global(|| false);
static SHARE_BUF: GlobalSignal<String> = Signal::global(String::new);
static SHARE_IMPORT_BUF: GlobalSignal<String> = Signal::global(String::new);
static SHARE_ERR: GlobalSignal<Option<String>> = Signal::global(|| None);
static PRESETS: GlobalSignal<PresetList> = Signal::global(|| PresetList::Idle);
static PRESET_VIEW: GlobalSignal<Option<PresetView>> = Signal::global(|| None);

// --- browser plumbing --------------------------------------------------------------
// Reflect-based instead of typed web_sys calls: the crate's web-sys feature
// set (Window/Document/Element/…) is frozen with Cargo.toml, and Location/
// Clipboard/HtmlElement are not in it.

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

/// One-shot lazy init: preset list fetch + last-viewed preset restore. (The
/// auto-snapshot ticker is `appstate::ensure_init`'s job — armed from every
/// panel, not just this one, so attribution-stamped diffs land even before
/// this panel first opens.)
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
}

// --- panel ---------------------------------------------------------------------

pub fn panel(_ctx: Ctx) -> Element {
    appstate::ensure_init();
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
        let ring = appstate::SNAPSHOTS.read();
        let rows: Vec<(usize, appstate::StateSnapshot)> =
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
                onclick: move |_| appstate::clear_timeline(),
                "Clear timeline"
            }
        }
        if entries.is_empty() {
            div { class: "inst-hint", "Timeline empty." }
        } else {
            div { class: "inst-timeline",
                for (idx, entry) in entries {
                    div { key: "{entry.timestamp_ms}-{idx}", class: "inst-row",
                        span { class: "inst-ts", { appstate::format_timestamp_ms(entry.timestamp_ms) } }
                        span { class: "inst-src", "{entry.source}" }
                        span { class: "inst-flex" }
                        button {
                            class: "btn inst-small",
                            // Restore stamps a `restore @ <orig_timestamp>` entry
                            // before the reload — the restore itself becomes a
                            // timeline event the user can undo (egui contract).
                            onclick: move |_| {
                                if let Err(e) = appstate::restore_snapshot(idx) {
                                    *IMPORT_ERR.write() = Some(e);
                                }
                            },
                            "Restore"
                        }
                    }
                }
            }
        }
    }
}

/// Shareable hash / link sub-region — port of `share_link_panel`. The codec
/// (compact JSON → DEFLATE → base64url, `#s=<hash>`) lives in `appstate`.
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
                title: "Encode the entire UI state into a short hash and copy a shareable link",
                onclick: move |_| {
                    match appstate::encode_share(&appstate::capture()) {
                        Ok(hash) => {
                            let link = page_origin()
                                .map(|o| format!("{o}/#{}={hash}", appstate::FRAGMENT_KEY))
                                .unwrap_or(hash);
                            copy_to_clipboard(&link);
                            *SHARE_BUF.write() = link;
                        }
                        Err(e) => *SHARE_BUF.write() = format!("encode error: {e}"),
                    }
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
                    match appstate::decode_share(&pasted) {
                        Ok(state) => {
                            *SHARE_ERR.write() = None;
                            appstate::apply(&state, "load share link");
                        }
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

/// Import / Export YAML sub-region — port of `yaml_io_panel`. The full
/// `AppState` round-trips through `appstate::{export_yaml, import_str}`
/// (the egui `export_state_yaml` / `import_state_yaml` pair; JSON also
/// accepted on import).
fn io_section() -> Element {
    // Live mirror of the current state: re-serialized every render the panel
    // is open (signal reads inside `capture` subscribe this scope), so what
    // you see / copy / download is never a stale snapshot — the egui
    // `yaml_export_buffer` contract.
    let live = appstate::export_yaml();
    let live_copy = live.clone();
    let live_dl = live.clone();
    let import = IMPORT_BUF.read().clone();
    let has_import = !import.trim().is_empty();
    let import_err = IMPORT_ERR.read().clone();
    let armed = *RESET_ARMED.read();
    rsx! {
        div { class: "inst-subhead", "Import / Export YAML" }
        div { class: "inst-row",
            span { class: "inst-live", "Live state" }
            button {
                class: "btn",
                title: "Copy the current state as YAML",
                onclick: move |_| copy_to_clipboard(&live_copy),
                "Copy"
            }
            button {
                class: "btn",
                title: "Download the entire current app state as a .yaml file",
                onclick: move |_| {
                    if let Err(e) = download_text(
                        "jump-cannon-appstate.yaml",
                        "application/yaml",
                        &live_dl,
                    ) {
                        *IMPORT_ERR.write() = Some(format!("download: {e}"));
                    }
                },
                "⬇ File"
            }
        }
        textarea { class: "inst-code", readonly: true, rows: "12", value: "{live}" }

        div { class: "inst-subhead", "Paste YAML to import" }
        textarea {
            class: "inst-code",
            rows: "12",
            placeholder: "Paste an AppState YAML (or JSON) document here, then click Load.",
            value: "{import}",
            oninput: move |e| *IMPORT_BUF.write() = e.value(),
        }
        div { class: "inst-row",
            button {
                class: "btn",
                disabled: !has_import,
                onclick: move |_| {
                    let text = IMPORT_BUF.peek().clone();
                    match appstate::import_str(&text) {
                        Ok(state) => {
                            *IMPORT_ERR.write() = None;
                            // Full replacement — every setting, the egui
                            // contract. The ring survives (sessionStorage)
                            // and the import stamps its own entry.
                            appstate::apply(&state, "import yaml");
                        }
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
                title: "Pick an exported .yaml state file and load the full app state from it",
                "⬆ Upload .yaml"
                input {
                    r#type: "file",
                    accept: ".yaml,.yml,.json,application/yaml,application/json",
                    style: "display:none",
                    onchange: move |evt| {
                        if let Some(engine) = evt.files() {
                            spawn(async move {
                                for name in engine.files() {
                                    match engine.read_file_to_string(&name).await {
                                        Some(text) => match appstate::import_str(&text) {
                                            Ok(state) => appstate::apply(&state, "import yaml"),
                                            Err(e) => {
                                                *IMPORT_ERR.write() =
                                                    Some(format!("upload parse: {e}"));
                                            }
                                        },
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
            // Two-step reset: first click arms, second commits — a full
            // default-state apply (egui `*state = AppState::default()`),
            // stamped in the timeline so the user can roll back.
            button {
                class: if armed { "btn inst-danger armed" } else { "btn inst-danger" },
                onclick: move |_| {
                    if *RESET_ARMED.peek() {
                        appstate::apply(&appstate::AppState::default(), "reset to defaults");
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

/// Viewer for the selected preset's YAML, with a Load button that applies it
/// to the live state through the same path as the paste / upload imports
/// (the egui `apply_imported_yaml` shared by paste, upload, and presets —
/// shared fields carry over; see the schema PARITY GAP on
/// `appstate::AppState` for the egui-era generate/seed sub-trees).
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
                div { class: "inst-row",
                    button {
                        class: "btn",
                        title: "Apply this preset to the live app state",
                        onclick: move |_| {
                            let Some(v) = PRESET_VIEW.peek().clone() else { return };
                            let Some(Ok(yaml)) = v.content else { return };
                            match appstate::import_str(&yaml) {
                                Ok(state) => {
                                    *IMPORT_ERR.write() = None;
                                    appstate::apply(&state, "import yaml");
                                }
                                Err(e) => {
                                    *IMPORT_ERR.write() = Some(format!("preset parse: {e}"));
                                }
                            }
                        },
                        "Load preset"
                    }
                }
                textarea { class: "inst-code", readonly: true, rows: "12", value: "{yaml}" }
            },
            Some(Err(e)) => rsx! { div { class: "inst-err", "preset fetch: {e}" } },
        }
    }
}

/// ActionInstance cards sub-region — the command-palette ActionRegistry's
/// recorded executions (egui sections/instances.rs::show_actions): title +
/// #id row, Params list, read-only state JSON, per-card ✕ remove.
fn actions_section() -> Element {
    let insts = crate::palette::instances_snapshot();
    if insts.is_empty() {
        return rsx! {
            div { class: "inst-hint",
                "No action instances yet. Press Ctrl+P to open the command palette."
            }
        };
    }
    rsx! {
        for (idx, inst) in insts.iter().enumerate() {
            div { key: "{inst.id}", class: "inst-action-card",
                {
                    let title = crate::palette::action_title(&inst.action_id)
                        .unwrap_or_else(|| inst.action_id.clone());
                    let id = inst.id;
                    rsx! {
                        div { class: "inst-action-head",
                            span { class: "inst-action-title", "{title}" }
                            span { class: "inst-action-id", "#{id}" }
                            button { class: "inst-action-x",
                                onclick: move |_| crate::palette::remove_instance(id),
                                "✕"
                            }
                        }
                    }
                }
                if !inst.params.is_empty() {
                    div { class: "inst-sub", "Params" }
                    for (k, v) in inst.params.iter() {
                        div { key: "{k}", class: "inst-action-param",
                            { format!("{k}: {}", param_value_display(v)) }
                        }
                    }
                }
                if !inst.state.is_null() {
                    div { class: "inst-sub", "State" }
                    textarea { class: "inst-code", readonly: true, rows: "3",
                        value: serde_json::to_string_pretty(&inst.state)
                            .unwrap_or_else(|_| inst.state.to_string()),
                    }
                }
                if idx + 1 < insts.len() { hr { class: "inst-sep" } }
            }
        }
    }
}

/// egui sections/instances.rs::param_value_display.
fn param_value_display(v: &crate::palette::ParamValue) -> String {
    use crate::palette::ParamValue as P;
    match v {
        P::String(s) => s.clone(),
        P::Number(n) => format!("{n}"),
        P::Boolean(b) => b.to_string(),
        P::Selected(xs) => xs.join(", "),
    }
}
