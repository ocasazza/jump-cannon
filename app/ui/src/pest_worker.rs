//! Local Web Worker client for the Importers panel's live grammar preview.
//!
//! This is the browser-side parse sandbox: it spawns the `pest-worker` bundle
//! (built by trunk from `app/pest-worker`, see `ui/index.html`) as a real
//! [`web_sys::Worker`] and hands it one package manifest plus one sample input
//! to preview-parse. Two properties matter:
//!
//! * **No UI freeze** — the parse runs in a browser-owned thread; the webview
//!   main thread only message-passes.
//! * **Killable CPU** — [pest_vm] has no fuel counter or preemption, so a
//!   hostile grammar must never parse on the UI thread. The watchdog below is
//!   the sandbox: a parse exceeding [`TIMEOUT_MS`] is hard-killed with
//!   [`Worker::terminate`] and surfaces as a timeout error. Byte/node/edge
//!   limits still come from the package's validated `[limits]`.
//!
//! ## No hand-written JS
//!
//! Same Blob-bootstrap trick as [`crate::worker`] (the tvix Generate worker):
//! trunk emits the worker as a `--target no-modules` bundle
//! (`pest-worker.js` + `pest-worker_bg.wasm`), and the classic-worker
//! bootstrap that `importScripts` the glue is built as a Rust string and
//! spawned from a Blob URL. `Trunk.toml` sets `filehash=false`, so the bundle
//! lives at stable filenames, resolved against `document.baseURI` at spawn.
//!
//! ## Lifecycle
//!
//! One worker is spawned per preview parse and [`Worker::terminate`]d when the
//! reply arrives or the watchdog fires. Preview parses are cheap (small
//! grammars, small inputs), so the per-spawn wasm re-compile is negligible;
//! the panel re-parses on demand, not per keystroke.

#![allow(dead_code)] // native target compiles only the stub below

#[cfg(target_arch = "wasm32")]
pub use wasm::parse_in_worker;

/// Preview result of parsing a sample input with a package's pest grammar.
/// Mirrors the worker's success reply (`app/pest-worker/src/main.rs`).
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ParsePreview {
    pub nodes: usize,
    pub edges: usize,
    pub unresolved: Vec<String>,
    pub sample_ids: Vec<String>,
}

/// Native stub: the worker is a browser construct. app/ui only ever ships to
/// wasm, but a native `cargo check`/`cargo test` of the workspace must still
/// typecheck the Importers panel's call site.
#[cfg(not(target_arch = "wasm32"))]
pub async fn parse_in_worker(_manifest: String, _input: String) -> Result<ParsePreview, String> {
    Err("the pest preview worker is wasm-only".to_string())
}

#[cfg(target_arch = "wasm32")]
mod wasm {
    use std::cell::RefCell;
    use std::future::Future;
    use std::pin::Pin;
    use std::rc::Rc;
    use std::task::{Context, Poll, Waker};

    use futures::future::Either;
    use gloo_timers::future::TimeoutFuture;
    use wasm_bindgen::prelude::*;
    use wasm_bindgen::JsCast;
    use web_sys::{Blob, BlobPropertyBag, MessageEvent, Url, Worker};

    use super::ParsePreview;

    /// Readiness marker the worker posts once its handler is installed. Must
    /// match `pest_worker`'s `READY` constant.
    const READY: &str = "__pest_worker_ready__";

    /// Preview-parse CPU budget. [pest_vm] has no fuel counter, so this
    /// watchdog is the only bound on a hostile grammar's CPU: on expiry the
    /// worker thread is hard-killed with [`Worker::terminate`].
    const TIMEOUT_MS: u32 = 5_000;

    /// Worker bundle filenames (stable because `filehash=false` in
    /// `app/Trunk.toml`). Resolved against `document.baseURI` at spawn time —
    /// never hardcoded root-absolute — because the same dist is hosted both
    /// at `/` (graph-api, Tauri) and under a subpath (GitHub Pages serves it
    /// at `/jump-cannon/`, with trunk's `--public-url` rewriting the asset
    /// links). The blob-URL worker cannot resolve relative URLs itself, so
    /// the bootstrap embeds absolute ones.
    fn worker_url(name: &str) -> String {
        let base = web_sys::window()
            .and_then(|window| window.document())
            .and_then(|document| document.base_uri().ok().flatten())
            .unwrap_or_else(|| "/".to_string());
        web_sys::Url::new_with_base(name, &base)
            .map(|url| url.href())
            .unwrap_or_else(|_| format!("/{name}"))
    }

    /// The classic-worker bootstrap: load the no-modules glue, then init the
    /// wasm (which runs the worker's `#[wasm_bindgen(start)]`, installing the
    /// message handler). `importScripts` is only available in classic workers —
    /// so we spawn a classic worker (the default `Worker::new`), NOT a module
    /// worker.
    fn bootstrap_src() -> String {
        format!(
            "self.importScripts('{js}');\nwasm_bindgen('{wasm}');\n",
            js = worker_url("pest-worker.js"),
            wasm = worker_url("pest-worker_bg.wasm"),
        )
    }

    /// Preview-parse `input` against the package `manifest` in a
    /// freshly-spawned `pest-worker`. The returned future resolves off the UI
    /// thread (driven by `spawn`/`spawn_local`), so the webview stays
    /// responsive while the worker parses.
    pub async fn parse_in_worker(
        manifest: String,
        input: String,
    ) -> Result<ParsePreview, String> {
        let worker = spawn_worker()?;

        let slot = Rc::new(RefCell::new(ReplySlot::default()));
        let slot_cb = slot.clone();
        let worker_cb = worker.clone();
        let job = serde_json::json!({ "manifest": manifest, "input": input }).to_string();

        // One handler serves both the readiness ping and the result. On `READY`
        // we post the job (it would have been dropped if posted before the
        // worker's handler existed). The next message is the result JSON.
        let on_message = Closure::<dyn FnMut(MessageEvent)>::new(move |evt: MessageEvent| {
            let data = evt.data().as_string().unwrap_or_default();
            if data == READY {
                let _ = worker_cb.post_message(&JsValue::from_str(&job));
                return;
            }
            let parsed = parse_reply(&data);
            let mut s = slot_cb.borrow_mut();
            s.result = Some(parsed);
            if let Some(w) = s.waker.take() {
                w.wake();
            }
        });
        worker.set_onmessage(Some(on_message.as_ref().unchecked_ref()));

        // Await the reply or the watchdog, whichever fires first. `on_message`
        // is kept alive on the stack here until the future resolves, so it is
        // dropped (not leaked) after each parse.
        let reply = ReplyFuture { slot };
        let watchdog = TimeoutFuture::new(TIMEOUT_MS);
        futures::pin_mut!(reply);
        futures::pin_mut!(watchdog);
        let result = match futures::future::select(reply, watchdog).await {
            Either::Left((result, _)) => result,
            Either::Right(((), _)) => Err("parse timed out (grammar CPU limit)".to_string()),
        };
        // Terminate on reply AND on timeout: the kill IS the sandbox — a
        // runaway grammar burns CPU only inside a dead thread.
        worker.terminate();
        drop(on_message);
        result
    }

    /// Spawn a classic worker from the Blob bootstrap. Returns a descriptive
    /// error string (surfaced inline in the Importers panel) on any failure.
    fn spawn_worker() -> Result<Worker, String> {
        let parts = js_sys::Array::new();
        parts.push(&JsValue::from_str(&bootstrap_src()));
        let opts = BlobPropertyBag::new();
        opts.set_type("application/javascript");
        let blob = Blob::new_with_str_sequence_and_options(&parts, &opts)
            .map_err(|e| format!("worker bootstrap blob failed: {e:?}"))?;
        let url = Url::create_object_url_with_blob(&blob)
            .map_err(|e| format!("worker bootstrap URL failed: {e:?}"))?;
        let worker = Worker::new(&url);
        // The worker has loaded its own copy of the bootstrap source by the
        // time `new` returns enough to keep going; revoke the transient URL
        // regardless.
        let _ = Url::revoke_object_url(&url);
        worker.map_err(|e| format!("worker spawn failed: {e:?}"))
    }

    /// Parse the worker's reply JSON into a [`ParsePreview`].
    ///
    /// Success shape: `{"ok":true,"nodes":N,"edges":M,"unresolved":[...],
    /// "sample_ids":[...]}`. Error shape: `{"ok":false,"error":"…"}`.
    fn parse_reply(data: &str) -> Result<ParsePreview, String> {
        let v: serde_json::Value =
            serde_json::from_str(data).map_err(|e| format!("worker reply was not JSON: {e}"))?;
        if v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false) {
            serde_json::from_value(v)
                .map_err(|e| format!("worker reply missing preview fields: {e}"))
        } else {
            Err(v
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("worker parse failed")
                .to_string())
        }
    }

    /// Shared cell the worker reply lands in, with a waker so [`ReplyFuture`]
    /// is driven to completion the instant the message arrives.
    #[derive(Default)]
    struct ReplySlot {
        result: Option<Result<ParsePreview, String>>,
        waker: Option<Waker>,
    }

    /// A leaf future that resolves when the worker posts its result. Avoids
    /// pulling `futures::channel` (the workspace `futures` dep is
    /// `default-features=false` without the `channel` feature).
    struct ReplyFuture {
        slot: Rc<RefCell<ReplySlot>>,
    }

    impl Future for ReplyFuture {
        type Output = Result<ParsePreview, String>;
        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            let mut s = self.slot.borrow_mut();
            if let Some(r) = s.result.take() {
                Poll::Ready(r)
            } else {
                s.waker = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    }
}
