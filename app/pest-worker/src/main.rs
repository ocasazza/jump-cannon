//! `pest-worker` — a dedicated Web Worker that runs `crates/importer`
//! pest-engine preview parses off the webview's main (UI) thread.
//!
//! ## Why this exists
//!
//! [pest_vm] has no parser fuel counter, deadline, or preemption mechanism:
//! a hostile or pathological grammar can burn CPU for as long as it likes
//! inside `parse_input`. Run on the UI thread that freezes the webview for
//! the duration (no paint, no input). This worker is the sandbox answer: a
//! second wasm instance, in a real OS thread the browser owns, that does
//! nothing but validate one package and parse one sample input. The UI
//! thread stays fully responsive, and the spawner's watchdog
//! ([`crate::pest_worker::parse_in_worker`] in app/ui) hard-kills the thread
//! with `Worker::terminate` when a parse exceeds its time budget.
//!
//! ## Protocol (plain strings, both directions)
//!
//! * **ready** — once its handler is installed the worker posts [`READY`]; the
//!   spawner waits for that before sending (a message posted before the async
//!   wasm init finished would be dropped).
//! * **in** — a `MessageEvent` whose `data` is a JSON job
//!   `{"manifest":"<package toml>","input":"<sample text>"}`.
//! * **out** — a JSON string from [`evaluate`]:
//!   `{"ok":true,"nodes":N,"edges":M,"unresolved":[...],"sample_ids":[...]}`
//!   on success (`sample_ids` = the first 20 node ids, for the preview pane),
//!   or `{"ok":false,"error":"…"}` on any manifest-validation, grammar-compile,
//!   or parse failure.
//!
//! ## Build
//!
//! Built by trunk from `app/ui/index.html` via
//! `<link data-trunk rel="rust" data-type="worker" data-bin="pest-worker" …>`.
//! Trunk emits `pest-worker.js` + `pest-worker_bg.wasm` into the same `dist/`;
//! `app/Trunk.toml` sets `filehash=false` so the spawner can reference the
//! stable URL `/pest-worker.js` at runtime.

/// Readiness marker the worker posts once its message handler is installed.
/// Must stay in sync with the spawner's `READY` (app/ui/src/pest_worker.rs).
pub const READY: &str = "__pest_worker_ready__";

/// Validate one package manifest and preview-parse one sample input with it,
/// returning the reply-JSON wire.
///
/// Target-agnostic (no JS plumbing) so the success/error shaping is unit-tested
/// natively — the wasm message handler in [`worker`] is a thin wrapper that
/// decodes the job JSON, calls this, and posts the result back.
pub fn evaluate(manifest: &str, input: &str) -> String {
    match preview(manifest, input) {
        Ok(reply) => reply.to_string(),
        Err(err) => serde_json::json!({ "ok": false, "error": err }).to_string(),
    }
}

/// The validated-package + parse pipeline behind [`evaluate`]. Every failure
/// class — TOML syntax, unsupported `format_version`, unknown engine, limit
/// violations, pest grammar compile errors (pest_meta's messages carry
/// line/column), capture-rule binding errors, and input parse errors — lands
/// here as the `Err` string the `{"ok":false,"error":…}` reply reports.
fn preview(manifest: &str, input: &str) -> Result<serde_json::Value, String> {
    let package = importer::ValidatedPackage::from_toml(manifest).map_err(|e| e.to_string())?;
    let result = package.parse_input(input).map_err(|e| e.to_string())?;
    let sample_ids: Vec<&String> = result.graph.nodes.keys().take(20).collect();
    Ok(serde_json::json!({
        "ok": true,
        "nodes": result.graph.nodes.len(),
        "edges": result.graph.edges.len(),
        "unresolved": result.unresolved,
        "sample_ids": sample_ids,
    }))
}

// The JS plumbing is wasm-only. A native `cargo build` of this bin (without the
// `wasm` feature) compiles to an empty `main` so `cargo check --workspace`
// stays green on the host without dragging in the wasm-only entry point.
#[cfg(all(target_arch = "wasm32", feature = "wasm"))]
mod worker {
    use wasm_bindgen::prelude::*;
    use wasm_bindgen::JsCast;
    use web_sys::{DedicatedWorkerGlobalScope, MessageEvent};

    /// Job message from the spawner: the package TOML and the sample input to
    /// preview-parse with it.
    #[derive(serde::Deserialize)]
    struct Job {
        manifest: String,
        input: String,
    }

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
            let data = evt.data().as_string().unwrap_or_default();
            let reply = match serde_json::from_str::<Job>(&data) {
                Ok(job) => crate::evaluate(&job.manifest, &job.input),
                Err(err) => serde_json::json!({
                    "ok": false,
                    "error": format!("invalid job message: {err}"),
                })
                .to_string(),
            };
            // Best-effort post back; if the channel is gone the main thread has
            // already torn the worker down, nothing to do.
            let _ = out.post_message(&JsValue::from_str(&reply));
        });
        scope.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
        // Keep the closure alive for the worker's lifetime.
        on_message.forget();

        // Handshake: announce readiness now that the handler is installed. The
        // spawner buffers the job until it sees this, because a message posted
        // before `set_onmessage` ran (wasm init is async) would be dropped.
        let _ = scope.post_message(&JsValue::from_str(crate::READY));
    }
}

fn main() {
    // On native (and on wasm without the `wasm` feature) this is a no-op shell;
    // the real entry is the `#[wasm_bindgen(start)]` in `worker::worker_start`.
}

#[cfg(test)]
mod tests {
    use super::evaluate;

    /// Line-graph package: `N|id|title|kind|tags|k=v;…` node records and
    /// `E|source|target` edge records, one per line. Mirrors
    /// `crates/importer/examples/line-graph.importer.toml`.
    const LINE_GRAPH: &str = r##"
format_version = 3

[metadata]
id = "example.line-graph"
name = "Line graph"
version = "1.0.0"
description = "Example runtime grammar for line-oriented node and edge records"

[parser]
engine = "pest"
root_rule = "document"
grammar = '''
document = { SOI ~ (record ~ NEWLINE?)* ~ EOI }
record = _{ node | edge }
node = { "N|" ~ node_id ~ "|" ~ title ~ "|" ~ kind ~ "|" ~ tags ~ "|" ~ properties }
node_id = @{ field }
title = @{ field }
kind = @{ field }
tags = _{ (tag ~ ("," ~ tag)*)? }
tag = @{ atom }
properties = _{ (property ~ (";" ~ property)*)? }
property = { key ~ "=" ~ value }
key = @{ atom }
value = @{ atom }
edge = { "E|" ~ source ~ "|" ~ target }
source = @{ field }
target = @{ field }
field = _{ (!("|" | NEWLINE) ~ ANY)+ }
atom = _{ (!("," | ";" | "=" | "|" | NEWLINE) ~ ANY)+ }
'''

[parser.captures]
node = "node"
id = "node_id"
title = "title"
kind = "kind"
tag = "tag"
property = "property"
key = "key"
value = "value"
edge = "edge"
source = "source"
target = "target"
"##;

    fn err_text(reply: &str) -> String {
        let v: serde_json::Value = serde_json::from_str(reply).unwrap();
        assert_eq!(v["ok"], serde_json::Value::Bool(false), "reply: {reply}");
        v["error"]
            .as_str()
            .unwrap_or_else(|| panic!("error is a string: {reply}"))
            .to_string()
    }

    #[test]
    fn evaluate_ok_counts_and_samples() {
        let input = "N|n1|Alpha|service|web|owner=team-a\nN|n2|Beta|db||zone=us\nE|n1|n2\nE|n1|ghost\n";
        let reply = evaluate(LINE_GRAPH, input);
        let v: serde_json::Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(v["ok"], serde_json::Value::Bool(true), "reply: {reply}");
        assert_eq!(v["nodes"], 2, "two N| records");
        assert_eq!(v["edges"], 1, "the edge to 'ghost' is not added");
        assert_eq!(
            v["unresolved"].as_array().unwrap().len(),
            1,
            "the missing endpoint is reported: {reply}"
        );
        let sample_ids = v["sample_ids"].as_array().unwrap();
        assert_eq!(sample_ids.len(), 2, "both node ids sampled");
        assert!(sample_ids.iter().all(|id| !id.as_str().unwrap().is_empty()));
    }

    #[test]
    fn evaluate_broken_toml_is_soft_error() {
        let error = err_text(&evaluate("format_version = 3\n[metadata\n", ""));
        assert!(error.contains("invalid importer TOML"), "error: {error}");
    }

    #[test]
    fn evaluate_bad_grammar_is_soft_error_with_location() {
        // Unbalanced brace: pest_meta reports line/column in its message.
        let broken = LINE_GRAPH.replace(
            "document = { SOI ~ (record ~ NEWLINE?)* ~ EOI }",
            "document = { SOI ~ (record ~ NEWLINE?)*",
        );
        let error = err_text(&evaluate(&broken, ""));
        assert!(error.contains("invalid Pest grammar"), "error: {error}");
        assert!(
            error.chars().any(|c| c.is_ascii_digit()),
            "pest_meta error carries a line/column: {error}"
        );
    }

    #[test]
    fn evaluate_missing_capture_rule_is_soft_error() {
        let broken = LINE_GRAPH.replace("id = \"node_id\"", "id = \"nope\"");
        let error = err_text(&evaluate(&broken, ""));
        assert!(
            error.contains("configured id rule 'nope' does not exist in the grammar"),
            "error: {error}"
        );
    }

    #[test]
    fn evaluate_non_matching_input_is_soft_error() {
        let error = err_text(&evaluate(LINE_GRAPH, "this is not a record"));
        assert!(
            error.contains("input did not match root rule 'document'"),
            "error: {error}"
        );
    }
}
