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
`GET /search` returns bounded protobuf IDs; `GET /search/rich` returns JSON
scores and schema-approved snippets. `GET /search/matches` is the set-valued
primitive for [[Filter Builder]]: it returns every match as raw little-endian
`u32` dense indices plus `X-Graph-Revision`, bounded by the exact snapshot's
node count rather than a ranked UI limit. Invalid or unavailable
field-qualified queries return HTTP 400. graph-api builds this index directly
from validated importer `SearchDocument` records and never falls back to
title-only matching.

`GET /importers` returns the active descriptor plus a bounded, sanitized list
of configured source instances. Its activation mode is `helm_rollout`; the API
does not expose a source-selection or run mutation. graph-api rejects an
unknown selection, a selected kind that differs from the importer actually
started, and unsafe filesystem profiles during startup.

Bulk arrays use little-endian numeric buffers; structured messages use protobuf
or JSON where appropriate. See [[Architecture]], [[Observability]], and
[[Security Model]]. Importer contracts are documented in [[Importer Runtime]].

The `/compute/*` family fronts the optional remote layout worker
([[Ray GPU Sessions]]). `/compute/health` and `/compute/engines` degrade
gracefully when the worker is absent; their wire shapes are frozen contracts
with the frontend. `GET /compute/session` reports the on-demand GPU session
lifecycle as derived state with the broker status embedded, and
`PUT /compute/session` requests dispatch or park; both return soft JSON
envelopes, and the pair reports `{"enabled": false}` where no cluster is
available (local development). Session progress and bounded worker logs emit
to the `gpu-session` group on `/progress`.

`POST /log/client` is the client-error intake: the Dioxus/Tauri app ships
panel fetch errors and wasm panics here (fire-and-forget, deduped), and the
server logs them under the `jump_cannon::client` tracing target (204; 422 on
malformed payloads; messages truncate at 4096 chars). The session manager's
per-world mounts inherit the route, so world-scoped failures land in the
session-manager pod's logs. When a user reports a UI failure, grep the pod
logs for `jump_cannon::client` before asking for console text.
