//! Tauri v2 application entrypoint. Kept in a lib so the mobile (iOS/Android)
//! harnesses can call `run()` — `#[cfg_attr(mobile, tauri::mobile_entry_point)]`
//! is what makes the same codebase boot on phones.
//!
//! The shell is a pure webview container: no IPC commands. The Dioxus frontend
//! talks HTTP straight to graph-api (start it with `just dev-up`).

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_http::init())
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
