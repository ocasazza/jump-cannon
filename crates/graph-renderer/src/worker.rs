//! Local Web Worker client for the `LocalWorker` generate backend.
//!
//! This is the OFFLINE non-freeze path: it spawns the `tvix-worker` bundle
//! (built by trunk from `crates/tvix-worker`, see `assets/index.html`) as a
//! real [`web_sys::Worker`] and hands it a Nix expression to evaluate. Because
//! the worker is a second wasm instance in a browser-owned thread, the egui
//! thread never blocks on `eval_graph` — unlike the `Inline` backend, which on
//! wasm runs the synchronous eval right on the paint thread.
//!
//! Unlike the `Server` backend it needs no reachable graph-api, so it is the
//! sensible `Auto` fallback when offline.
//!
//! ## No hand-written JS
//!
//! Trunk emits the worker as a `--target no-modules` bundle (`tvix-worker.js` +
//! `tvix-worker_bg.wasm`). A classic worker needs a one-line bootstrap that
//! `importScripts` that glue and calls its init. Rather than add a standalone
//! `.js` file (which the Rust-only rule forbids beyond the wasm loader shim),
//! we build that bootstrap as a Rust string and spawn the worker from a Blob
//! URL — the only "logic" in it is "load the wasm", same category as the
//! trunk-generated loader. `Trunk.toml` sets `filehash=false`, so the bundle
//! lives at the stable URL [`WORKER_JS_URL`].
//!
//! ## Lifecycle
//!
//! One worker is spawned per evaluation and [`Worker::terminate`]d when the
//! reply arrives. Re-fetching the bundle is HTTP-cached; the per-eval cost is a
//! wasm re-compile (tens of ms), which is negligible next to the multi-second
//! evals this path exists to keep off the paint thread. A persistent worker
//! pool is a possible future optimization (it would need a ready-handshake
//! queue for concurrent jobs; the Generate panel only runs one at a time).

#![cfg(target_arch = "wasm32")]

use std::cell::RefCell;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};

use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{Blob, BlobPropertyBag, MessageEvent, Url, Worker};

use tvix_wasm::GeneratedGraph;

/// Stable URL of the trunk-emitted worker glue (`filehash=false` in
/// `Trunk.toml`; `public_url="/assets/"`).
const WORKER_JS_URL: &str = "/assets/tvix-worker.js";
/// Stable URL of the worker's wasm module.
const WORKER_WASM_URL: &str = "/assets/tvix-worker_bg.wasm";

/// Readiness marker the worker posts once its handler is installed. Must match
/// `tvix_worker`'s `READY` constant.
const READY: &str = "__tvix_worker_ready__";

/// The classic-worker bootstrap: load the no-modules glue, then init the wasm
/// (which runs `#[wasm_bindgen(start)] main`, installing the message handler).
/// `importScripts` is only available in classic workers — so we spawn a classic
/// worker (the default `Worker::new`), NOT a module worker.
fn bootstrap_src() -> String {
    format!(
        "self.importScripts('{js}');\nwasm_bindgen('{wasm}');\n",
        js = WORKER_JS_URL,
        wasm = WORKER_WASM_URL,
    )
}

/// Evaluate `expr` in a freshly-spawned `tvix-worker`, returning the parsed
/// graph. The returned future resolves off the egui thread (driven by
/// `spawn_local` via `BackgroundJob::spawn_future`), so the paint thread stays
/// responsive while the worker evaluates.
pub async fn eval_in_worker(expr: String) -> Result<GeneratedGraph, String> {
    let worker = spawn_worker()?;

    let slot = Rc::new(RefCell::new(ReplySlot::default()));
    let slot_cb = slot.clone();
    let worker_cb = worker.clone();
    let expr_cb = expr.clone();

    // One handler serves both the readiness ping and the result. On `READY` we
    // post the expression (it would have been dropped if posted before the
    // worker's handler existed). The next message is the result JSON.
    let on_message = Closure::<dyn FnMut(MessageEvent)>::new(move |evt: MessageEvent| {
        let data = evt.data().as_string().unwrap_or_default();
        if data == READY {
            let _ = worker_cb.post_message(&JsValue::from_str(&expr_cb));
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

    // Await the reply. `on_message` is kept alive on the stack here until the
    // future resolves, so it is dropped (not leaked) after each eval.
    let result = ReplyFuture { slot }.await;
    worker.terminate();
    drop(on_message);
    result
}

/// Spawn a classic worker from the Blob bootstrap. Returns a descriptive
/// error string (surfaced inline in the Generate panel) on any failure.
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
    // The worker has loaded its own copy of the bootstrap source by the time
    // `new` returns enough to keep going; revoke the transient URL regardless.
    let _ = Url::revoke_object_url(&url);
    worker.map_err(|e| format!("worker spawn failed: {e:?}"))
}

/// Parse the worker's reply JSON into a graph.
///
/// Success shape: `{"ok":true,"graph":{"nodes":[...],"links":[...]}}` — the
/// embedded `graph` is the canonical `toGraphJSON` wire, so it flows through
/// the same [`tvix_wasm::parse_graph_json`] the Server backend uses (identical
/// promotion path, no shape drift). Error shape: `{"ok":false,"error":"…"}`.
fn parse_reply(data: &str) -> Result<GeneratedGraph, String> {
    let v: serde_json::Value = serde_json::from_str(data)
        .map_err(|e| format!("worker reply was not JSON: {e}"))?;
    if v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false) {
        let graph = v
            .get("graph")
            .ok_or_else(|| "worker reply missing `graph`".to_string())?;
        tvix_wasm::parse_graph_json(&graph.to_string())
    } else {
        Err(v
            .get("error")
            .and_then(|e| e.as_str())
            .unwrap_or("worker evaluation failed")
            .to_string())
    }
}

/// Shared cell the worker reply lands in, with a waker so [`ReplyFuture`] is
/// driven to completion the instant the message arrives.
#[derive(Default)]
struct ReplySlot {
    result: Option<Result<GeneratedGraph, String>>,
    waker: Option<Waker>,
}

/// A leaf future that resolves when the worker posts its result. Avoids pulling
/// `futures::channel` (the workspace `futures` dep is `default-features=false`
/// without the `channel` feature).
struct ReplyFuture {
    slot: Rc<RefCell<ReplySlot>>,
}

impl Future for ReplyFuture {
    type Output = Result<GeneratedGraph, String>;
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
