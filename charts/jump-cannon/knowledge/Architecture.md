---
doctype: architecture
area: development
audience: [developer, agent]
status: current
tags: [jump-cannon, architecture]
---

# Architecture

Markdown or another configured source is loaded into `vault-data::Graph`.
The importer also supplies a mandatory discovery schema and one validated
search document per node. graph-api owns the active atomic snapshot, metrics,
in-process search index, schema-driven facets, editing, progress, and the
optional compute broker. The Dioxus WASM client fetches topology and renders it
with wgpu.

Keep source loading in [[Importer Runtime]], HTTP contracts in [[Backend API]],
presentation in [[Frontend]], and scheduled or remote work in [[Compute]].
