//! Client-side error reporting: ship panel fetch failures and Rust panics to
//! the server's `POST /log/client` (graph-api directly, or the session
//! manager's per-world mount when a world is open) so deployments capture
//! what browsers actually hit — the alternative is asking users to paste
//! webview console text. Everything here is fire-and-forget: reporting must
//! never block, fail, or recurse into the UI.

use dioxus::prelude::*;

use crate::api;

/// Dedupe window: the same (level, message) pair is reported at most once
/// per this many milliseconds — polling panels would otherwise re-ship an
/// identical failure every cycle.
const DEDUPE_MS: f64 = 10_000.0;

thread_local! {
    static RECENT: std::cell::RefCell<Vec<(String, f64)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Report an error to the server. Never blocks and never returns a Result:
/// the POST outcome is dropped by design.
pub fn report(level: &str, message: impl Into<String>) {
    let message = message.into();
    let key = format!("{level}\n{message}");
    let now = js_sys::Date::now();
    let fresh = RECENT.with(|recent| {
        let mut recent = recent.borrow_mut();
        recent.retain(|(_, t)| now - t < DEDUPE_MS);
        if recent.iter().any(|(k, _)| *k == key) {
            false
        } else {
            recent.push((key, now));
            true
        }
    });
    if !fresh {
        return;
    }
    let level = level.to_string();
    spawn(async move {
        let body = serde_json::json!({
            "level": level,
            "message": message,
            "context": {
                "href": href(),
                "world": api::WORLD_BASE.read().clone(),
            },
        });
        // `api::url` already targets the world-scoped session manager when a
        // world is open; `x-user` is required on every authenticated route
        // there and ignored by standalone graph-api.
        let req = gloo_net::http::Request::post(&api::url("/log/client"))
            .header("x-user", &api::user_name())
            .json(&body);
        if let Ok(req) = req {
            let _ = req.send().await;
        }
    });
}

/// Report `err` tagged with the panel name and return the message, so error
/// arms stay one line: `Err(e) => note.set(Some(client_log::tagged("worlds", e)))`.
pub fn tagged(panel: &str, err: impl std::fmt::Display) -> String {
    let msg = err.to_string();
    report("error", format!("{panel}: {msg}"));
    msg
}

/// The page URL, via reflection (same trick as `api::page_origin`) so we
/// don't pull in another web-sys feature for one getter.
#[cfg(target_arch = "wasm32")]
fn href() -> Option<String> {
    use wasm_bindgen::JsValue;
    let win = web_sys::window()?;
    let loc = js_sys::Reflect::get(win.as_ref(), &JsValue::from_str("location")).ok()?;
    js_sys::Reflect::get(&loc, &JsValue::from_str("href"))
        .ok()?
        .as_string()
}

#[cfg(not(target_arch = "wasm32"))]
fn href() -> Option<String> {
    None
}

/// Install a panic hook that reports panics to the server, then delegates to
/// the previously installed hook so console visibility is unchanged. Called
/// once from `main` before `launch`.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown panic payload".into());
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "unknown location".into());
        report("panic", format!("{payload} @ {location}"));
        previous(info);
    }));
}
