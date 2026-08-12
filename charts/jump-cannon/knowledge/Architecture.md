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
with wgpu. The legacy Obsidian-only `vault-search` binary remains independently
runnable but is not part of graph-api's request or reload path.

Alongside the single-source graph-api, the optional session manager hosts
multi-user shared versioned worlds: the same Dioxus app switches to its
Sessions view, and each world's served branch is rebuilt through the same
atomic snapshot path. Keep it in [[Session Manager]] and its versioning model
in [[Worlds and Versioning]].

Keep source loading in [[Importer Runtime]], HTTP contracts in [[Backend API]],
presentation in [[Frontend]], and scheduled or remote work in [[Compute]].
