//! `tvix-worker` — a dedicated Web Worker that evaluates Nix graph expressions
//! off the browser's main (egui) thread.
//!
//! ## Why this exists
//!
//! `tvix_wasm::eval_graph` is a single synchronous call that can take a
//! noticeable wall-clock chunk for a large graph. Run on the egui thread it
//! freezes the tab for the duration (no paint, no input). The `Server`
//! execution backend already avoids that by evaluating in graph-api over async
//! HTTP — but that needs a reachable server. This worker is the OFFLINE
//! non-freeze: a second wasm instance, in a real OS thread the browser owns,
//! that does nothing but `eval_graph`. The egui thread stays fully responsive
//! and talks to it by message-passing only — no SharedArrayBuffer, no
//! COOP/COEP, no atomics build.
//!
//! ## Protocol (plain strings, both directions)
//!
//! * **ready** — once its handler is installed the worker posts [`READY`]; the
//!   spawner waits for that before sending (a message posted before the async
//!   wasm init finished would be dropped).
//! * **in** — a `MessageEvent` whose `data` is the Nix expression to evaluate.
//! * **out** — a JSON string from [`evaluate`]:
//!   `{"ok":true,"graph":{"nodes":[...],"links":[...]}}` on success (the
//!   canonical `toGraphJSON` wire that [`tvix_wasm::parse_graph_json`] accepts,
//!   so the renderer flows it into the SAME promotion path as a server eval),
//!   or `{"ok":false,"error":"…"}` on an eval error.
//!
//! ## Build
//!
//! Built by trunk from `assets/index.html` via
//! `<link data-trunk rel="rust" data-type="worker" data-bin="tvix-worker" …>`.
//! Trunk emits `tvix-worker.js` + `tvix-worker_bg.wasm` into the same `dist/`;
//! `Trunk.toml` sets `filehash=false` so the renderer can reference the stable
//! URL `/assets/tvix-worker.js` at runtime.

/// Readiness marker the worker posts once its message handler is installed.
/// Must stay in sync with the renderer's `crate::worker::READY`.
pub const READY: &str = "__tvix_worker_ready__";

/// Evaluate one expression to the reply-JSON wire.
///
/// Target-agnostic (no JS plumbing) so the success/error shaping is unit-tested
/// natively — the wasm message handler in [`worker`] is a thin wrapper that
/// calls this and posts the result back.
pub fn evaluate(expr: &str) -> String {
    match tvix_wasm::eval_graph(expr) {
        Ok(graph) => {
            // `to_graph_json` is already a `{nodes,links}` JSON string; splice
            // it in as a raw value rather than re-stringifying.
            format!(
                "{{\"ok\":true,\"graph\":{}}}",
                tvix_wasm::to_graph_json(&graph)
            )
        }
        Err(err) => serde_json::json!({ "ok": false, "error": err }).to_string(),
    }
}

// The JS plumbing is wasm-only. A native `cargo build` of this bin (without the
// `wasm` feature) compiles to an empty `main` so `cargo check --workspace`
// stays green on the host without dragging in the wasm-only entry point.
#[cfg(all(target_arch = "wasm32", feature = "wasm"))]
mod worker {
    use wasm_bindgen::prelude::*;
    use wasm_bindgen::JsCast;
    use web_sys::{DedicatedWorkerGlobalScope, MessageEvent};

    /// Worker entry point. `#[wasm_bindgen(start)]` runs this automatically when
    /// the worker's wasm module boots, so there is no hand-written JS driving
    /// it — trunk's generated worker glue loads the module and this installs the
    /// message handler from Rust. Named `worker_start` (not `main`) so it does
    /// not collide with the bin crate's own `main` entry symbol at link time.
    #[wasm_bindgen(start)]
    pub fn worker_start() {
        console_error_panic_hook::set_once();

        // `self` inside a dedicated worker is the DedicatedWorkerGlobalScope.
        let scope: DedicatedWorkerGlobalScope = js_sys::global().unchecked_into();
        let out = scope.clone();

        let on_message = Closure::<dyn FnMut(MessageEvent)>::new(move |evt: MessageEvent| {
            let expr = evt.data().as_string().unwrap_or_default();
            let reply = crate::evaluate(&expr);
            // Best-effort post back; if the channel is gone the main thread has
            // already torn the worker down, nothing to do.
            let _ = out.post_message(&JsValue::from_str(&reply));
        });
        scope.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
        // Keep the closure alive for the worker's lifetime.
        on_message.forget();

        // Handshake: announce readiness now that the handler is installed. The
        // spawner buffers the expression until it sees this, because a message
        // posted before `set_onmessage` ran (wasm init is async) would be
        // dropped. The marker is shared with `crate::worker::READY` (renderer).
        let _ = scope.post_message(&JsValue::from_str(crate::READY));
    }
}

fn main() {
    // On native (and on wasm without the `wasm` feature) this is a no-op shell;
    // the real entry is the `#[wasm_bindgen(start)]` in `worker::main`.
}

#[cfg(test)]
mod tests {
    use super::evaluate;

    /// A star graph authored against the embedded tvix graph library, emitting
    /// `toGraphJSON`'s `{ nodes, links }` shape.
    const STAR_EXPR: &str = r#"
        let
          g  = import /jc/src/graph.nix {};
          gc = import /jc/src/graph-combinators.nix { graph = g; };
        in g.toGraphJSON (gc.starGen { nodes = 5; prefix = "n"; })
    "#;

    #[test]
    fn evaluate_ok_wraps_a_parseable_graph() {
        let reply = evaluate(STAR_EXPR);
        assert!(reply.starts_with("{\"ok\":true,"), "reply: {reply}");

        // The embedded `graph` must be the canonical wire the renderer parses
        // back via the SAME path the Server backend uses — verify it round-trips
        // through `parse_graph_json` (the exact call `crate::worker::parse_reply`
        // makes on the renderer side).
        let v: serde_json::Value = serde_json::from_str(&reply).unwrap();
        let graph = v.get("graph").expect("reply has graph").to_string();
        let parsed = tvix_wasm::parse_graph_json(&graph).expect("graph parses");
        // starGen{nodes=5}: center n0 + 4 spokes n1..n4 → 5 nodes, 4 edges.
        assert_eq!(parsed.nodes.len(), 5, "star of 5 has 5 nodes");
        assert_eq!(parsed.edges.len(), 4, "star of 5 has 4 edges");
    }

    #[test]
    fn evaluate_err_is_soft_json() {
        // A syntactically broken expression must NOT panic — it must come back
        // as `{"ok":false,"error":…}` so the renderer can surface it inline.
        let reply = evaluate("this is not (valid nix");
        let v: serde_json::Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(v["ok"], serde_json::Value::Bool(false), "reply: {reply}");
        assert!(v["error"].is_string(), "error is a string: {reply}");
    }
}
