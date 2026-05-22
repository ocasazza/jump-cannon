//! AppState persistence backend.
//!
//! * **Native** (`#[cfg(not(target_arch = "wasm32"))]`): no-op stubs.
//!   `App::save` / `App::new` still go through `eframe::Storage` (the
//!   established platform-dirs JSON blob). This module's `load`/`save`
//!   simply read/return defaults on native — callers in `app.rs` use
//!   the eframe path as the source of truth.
//! * **WASM** (`#[cfg(target_arch = "wasm32")]`): JSON blob in browser
//!   `sessionStorage`. A tab reload preserves layout/panel/filter/window
//!   state; a brand-new tab starts at `AppState::default()`. We also
//!   keep mirroring to `eframe::Storage` so existing test harnesses
//!   that introspect that key keep working, but the source of truth
//!   on WASM is sessionStorage.
//!
//! A `beforeunload` listener flushes the most-recently-serialized JSON
//! one more time when the user reloads / closes the tab. The closure
//! reads from a `Mutex<Option<String>>` updated by every `save()` call,
//! so it never contends with the egui paint thread's locks.
//!
//! Migration: if no sessionStorage entry exists but the legacy
//! `eframe::Storage` blob does, we read the eframe blob, deserialize,
//! immediately write it back to sessionStorage, and return it. Users
//! don't lose state across the cutover.

use crate::ui::state::{migrate_layout_state, AppState, STORAGE_KEY};

// -----------------------------------------------------------------------------
// Native
// -----------------------------------------------------------------------------

#[cfg(not(target_arch = "wasm32"))]
pub fn load_from_eframe(storage: Option<&dyn eframe::Storage>) -> AppState {
    let Some(s) = storage else { return AppState::default() };
    let Some(raw) = s.get_string(STORAGE_KEY) else { return AppState::default() };
    deserialize_with_migration(&raw).unwrap_or_default()
}

#[cfg(not(target_arch = "wasm32"))]
pub fn save_to_eframe(storage: &mut dyn eframe::Storage, state: &AppState) {
    if let Ok(json) = serde_json::to_string(state) {
        storage.set_string(STORAGE_KEY, json);
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub fn install_beforeunload_hook() {
    // No-op on native.
}

// -----------------------------------------------------------------------------
// WASM
// -----------------------------------------------------------------------------

#[cfg(target_arch = "wasm32")]
use std::sync::Mutex;

#[cfg(target_arch = "wasm32")]
static LAST_SERIALIZED: Mutex<Option<String>> = Mutex::new(None);

/// On WASM, called from `App::new` BEFORE `eframe::Storage` is consulted.
/// Returns the AppState recovered from sessionStorage, with a one-shot
/// migration that pulls the legacy eframe blob over on first load.
#[cfg(target_arch = "wasm32")]
pub fn load_from_eframe(storage: Option<&dyn eframe::Storage>) -> AppState {
    // 1) sessionStorage is the source of truth.
    match session_storage_get(STORAGE_KEY) {
        Ok(Some(raw)) => match deserialize_with_migration(&raw) {
            Some(state) => return state,
            None => {
                log::warn!(
                    "[persist] sessionStorage[{STORAGE_KEY}] failed to deserialize; \
                     clearing and falling back to default"
                );
                let _ = session_storage_remove(STORAGE_KEY);
            }
        },
        Ok(None) => { /* fall through to migration */ }
        Err(e) => {
            log::warn!("[persist] sessionStorage read failed: {e}");
        }
    }

    // 2) Migration: read legacy eframe blob if present, hand it to
    //    sessionStorage immediately so subsequent reloads are sticky.
    if let Some(s) = storage {
        if let Some(raw) = s.get_string(STORAGE_KEY) {
            if let Some(state) = deserialize_with_migration(&raw) {
                if let Ok(json) = serde_json::to_string(&state) {
                    let _ = session_storage_set(STORAGE_KEY, &json);
                    *LAST_SERIALIZED.lock().unwrap() = Some(json);
                }
                return state;
            }
        }
    }

    AppState::default()
}

#[cfg(target_arch = "wasm32")]
pub fn save_to_eframe(storage: &mut dyn eframe::Storage, state: &AppState) {
    let Ok(json) = serde_json::to_string(state) else { return };
    // sessionStorage = source of truth on WASM.
    if let Err(e) = session_storage_set(STORAGE_KEY, &json) {
        log::warn!("[persist] sessionStorage write failed: {e}");
    }
    *LAST_SERIALIZED.lock().unwrap() = Some(json.clone());
    // Mirror to eframe storage for compatibility (test harnesses, etc).
    storage.set_string(STORAGE_KEY, json);
}

/// Install a one-shot `beforeunload` window listener that re-flushes the
/// last serialized AppState JSON to sessionStorage. Idempotent — once
/// installed, additional calls are no-ops. The closure only reads the
/// `LAST_SERIALIZED` mutex (updated by the regular `save_to_eframe`
/// path), so it never contends with egui's paint-time locks.
#[cfg(target_arch = "wasm32")]
pub fn install_beforeunload_hook() {
    use std::sync::atomic::{AtomicBool, Ordering};
    static INSTALLED: AtomicBool = AtomicBool::new(false);
    if INSTALLED.swap(true, Ordering::SeqCst) {
        return;
    }

    use wasm_bindgen::closure::Closure;
    use wasm_bindgen::JsCast;

    let Some(window) = web_sys::window() else {
        log::warn!("[persist] no window — beforeunload hook skipped");
        return;
    };

    let cb = Closure::<dyn Fn(web_sys::Event)>::new(move |_e: web_sys::Event| {
        if let Some(json) = LAST_SERIALIZED.lock().ok().and_then(|g| g.clone()) {
            if let Err(e) = session_storage_set(STORAGE_KEY, &json) {
                log::warn!("[persist] beforeunload flush failed: {e}");
            }
        }
    });

    if let Err(e) = window.add_event_listener_with_callback(
        "beforeunload",
        cb.as_ref().unchecked_ref(),
    ) {
        log::warn!("[persist] add_event_listener_with_callback failed: {e:?}");
    }
    // Leak the closure: it must live for the page lifetime.
    cb.forget();
}

#[cfg(target_arch = "wasm32")]
fn session_storage_get(key: &str) -> Result<Option<String>, String> {
    let window = web_sys::window().ok_or_else(|| "no window".to_string())?;
    let storage = window
        .session_storage()
        .map_err(|e| format!("{e:?}"))?
        .ok_or_else(|| "no sessionStorage".to_string())?;
    storage.get_item(key).map_err(|e| format!("{e:?}"))
}

#[cfg(target_arch = "wasm32")]
fn session_storage_set(key: &str, val: &str) -> Result<(), String> {
    let window = web_sys::window().ok_or_else(|| "no window".to_string())?;
    let storage = window
        .session_storage()
        .map_err(|e| format!("{e:?}"))?
        .ok_or_else(|| "no sessionStorage".to_string())?;
    storage.set_item(key, val).map_err(|e| format!("{e:?}"))
}

#[cfg(target_arch = "wasm32")]
fn session_storage_remove(key: &str) -> Result<(), String> {
    let window = web_sys::window().ok_or_else(|| "no window".to_string())?;
    let storage = window
        .session_storage()
        .map_err(|e| format!("{e:?}"))?
        .ok_or_else(|| "no sessionStorage".to_string())?;
    storage.remove_item(key).map_err(|e| format!("{e:?}"))
}

// -----------------------------------------------------------------------------
// Shared
// -----------------------------------------------------------------------------

/// Try to parse a raw JSON blob into an `AppState`, applying the
/// pre-refactor `LayoutState` migration. Returns `None` on any failure;
/// callers fall back to `AppState::default()`.
fn deserialize_with_migration(raw: &str) -> Option<AppState> {
    let mut value: serde_json::Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(e) => {
            log::warn!("[persist] AppState JSON parse failed: {e}");
            return None;
        }
    };
    migrate_layout_state(&mut value);
    match serde_json::from_value::<AppState>(value) {
        Ok(s) => Some(s),
        Err(e) => {
            log::warn!("[persist] AppState typed decode failed: {e}");
            None
        }
    }
}
