//! File download / upload + server-preset fetch for the Instances-page YAML
//! import/export feature.
//!
//! WASM uses the browser: anchor-triggered Blob download, a transient
//! `<input type=file>` + `FileReader` for upload, and `fetch` for dev-server
//! presets (`GET /configs/<name>`). Native falls back to plain filesystem
//! writes (there's no `rfd` dep, so no native file dialog — the dev app is the
//! WASM build). Async results (uploaded text, fetched presets) land in small
//! static slots that the panel drains each frame.

// ============================ Download (export) =============================

/// Trigger a download of `contents` as `filename`. Returns a user-facing
/// confirmation string (the path on native, a note on WASM).
#[cfg(target_arch = "wasm32")]
pub fn download_text(filename: &str, mime: &str, contents: &str) -> Result<String, String> {
    use wasm_bindgen::JsCast;
    let win = web_sys::window().ok_or("no window")?;
    let doc = win.document().ok_or("no document")?;

    let parts = js_sys::Array::new();
    parts.push(&wasm_bindgen::JsValue::from_str(contents));
    let opts = web_sys::BlobPropertyBag::new();
    opts.set_type(mime);
    let blob = web_sys::Blob::new_with_str_sequence_and_options(&parts, &opts)
        .map_err(|e| format!("blob: {e:?}"))?;
    let url =
        web_sys::Url::create_object_url_with_blob(&blob).map_err(|e| format!("url: {e:?}"))?;

    let anchor = doc
        .create_element("a")
        .map_err(|e| format!("create anchor: {e:?}"))?
        .dyn_into::<web_sys::HtmlAnchorElement>()
        .map_err(|_| "not an anchor element".to_string())?;
    anchor.set_href(&url);
    anchor.set_download(filename);
    anchor.click();
    let _ = web_sys::Url::revoke_object_url(&url);
    Ok(format!("downloaded {filename}"))
}

#[cfg(not(target_arch = "wasm32"))]
pub fn download_text(filename: &str, _mime: &str, contents: &str) -> Result<String, String> {
    let path = std::env::current_dir()
        .map_err(|e| e.to_string())?
        .join(filename);
    std::fs::write(&path, contents).map_err(|e| e.to_string())?;
    Ok(path.display().to_string())
}

// ============================ Upload (import) ===============================

#[cfg(target_arch = "wasm32")]
static UPLOAD_SLOT: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Open a browser file picker; the chosen file's text lands in the upload slot
/// (drain with [`take_upload`]). `accept` is an HTML accept filter, e.g. ".yaml".
#[cfg(target_arch = "wasm32")]
pub fn open_upload(accept: &str) {
    use wasm_bindgen::closure::Closure;
    use wasm_bindgen::JsCast;

    let Some(doc) = web_sys::window().and_then(|w| w.document()) else {
        return;
    };
    let Ok(el) = doc.create_element("input") else {
        return;
    };
    let Ok(input) = el.dyn_into::<web_sys::HtmlInputElement>() else {
        return;
    };
    input.set_type("file");
    input.set_accept(accept);

    let input_cb = input.clone();
    let onchange = Closure::<dyn FnMut()>::new(move || {
        let Some(file) = input_cb.files().and_then(|f| f.get(0)) else {
            return;
        };
        let Ok(reader) = web_sys::FileReader::new() else {
            return;
        };
        let reader_cb = reader.clone();
        let onload = Closure::<dyn FnMut()>::new(move || {
            if let Some(text) = reader_cb.result().ok().and_then(|v| v.as_string()) {
                if let Ok(mut slot) = UPLOAD_SLOT.lock() {
                    *slot = Some(text);
                }
            }
        });
        reader.set_onload(Some(onload.as_ref().unchecked_ref()));
        onload.forget();
        let _ = reader.read_as_text(&file);
    });
    input.set_onchange(Some(onchange.as_ref().unchecked_ref()));
    onchange.forget();
    input.click();
}

/// Take the most recently uploaded file's text, if any.
#[cfg(target_arch = "wasm32")]
pub fn take_upload() -> Option<String> {
    UPLOAD_SLOT.lock().ok().and_then(|mut s| s.take())
}

#[cfg(not(target_arch = "wasm32"))]
pub fn open_upload(_accept: &str) {}
#[cfg(not(target_arch = "wasm32"))]
pub fn take_upload() -> Option<String> {
    None
}

// ====================== Server preset configs (fetch) ======================

#[cfg(target_arch = "wasm32")]
static PRESET_SLOT: std::sync::Mutex<Option<Result<String, String>>> = std::sync::Mutex::new(None);

/// Async `GET /configs/<name>` (relative to the page origin); the YAML body (or
/// an error) lands in the preset slot — drain with [`take_preset`].
#[cfg(target_arch = "wasm32")]
pub fn fetch_preset(name: &str) {
    let name = name.to_string();
    wasm_bindgen_futures::spawn_local(async move {
        let url = format!("/configs/{name}");
        let result = match gloo_net::http::Request::get(&url).send().await {
            Ok(resp) if resp.ok() => resp.text().await.map_err(|e| e.to_string()),
            Ok(resp) => Err(format!("HTTP {}", resp.status())),
            Err(e) => Err(e.to_string()),
        };
        if let Ok(mut slot) = PRESET_SLOT.lock() {
            *slot = Some(result);
        }
    });
}

#[cfg(target_arch = "wasm32")]
pub fn take_preset() -> Option<Result<String, String>> {
    PRESET_SLOT.lock().ok().and_then(|mut s| s.take())
}

#[cfg(not(target_arch = "wasm32"))]
pub fn fetch_preset(_name: &str) {}
#[cfg(not(target_arch = "wasm32"))]
pub fn take_preset() -> Option<Result<String, String>> {
    None
}

/// The dev-server preset names the Instances page offers. Matches the files in
/// `crates/graph-renderer/configs/` served by graph-api at `/configs/<name>`.
pub const PRESET_NAMES: &[&str] = &["default", "showcase-gpu", "lavender"];
