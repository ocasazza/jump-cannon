---
doctype: architecture
area: development
audience: [developer, operator, agent]
status: current
tags: [jump-cannon, backend, axum]
---

# Backend API

`crates/graph-api` is the axum boundary for topology, metadata, search, editing,
generation, progress, and optional remote layout. It atomically swaps complete
graph snapshots so in-flight requests do not observe partial reloads. Each
snapshot contains the graph, importer discovery schema, in-process Tantivy
index, schema-driven facet summary, metrics, and binary caches for one revision.

`GET /graph/schema` returns that revision, the active source identity/version,
and its typed searchable, facetable, snippet, edge, media, and content contract.
`GET /search` returns protobuf IDs; `GET /search/rich` returns JSON scores and
schema-approved snippets. Invalid or unavailable field-qualified queries return
HTTP 400. graph-api builds this index directly from validated importer
`SearchDocument` records; it does not spawn `vault-search` and never falls back
to title-only matching.

`GET /importers` returns the active descriptor plus a bounded, sanitized list
of configured source instances. Its activation mode is `helm_rollout`; the API
does not expose a source-selection or run mutation. graph-api rejects an
unknown selection, a selected kind that differs from the importer actually
started, and unsafe filesystem profiles during startup.

Bulk arrays use little-endian numeric buffers; structured messages use protobuf
or JSON where appropriate. See [[Architecture]], [[Observability]], and
[[Security Model]]. Importer contracts are documented in [[Importer Runtime]].
