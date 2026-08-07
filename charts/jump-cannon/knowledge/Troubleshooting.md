---
doctype: runbook
area: operations
audience: [developer, operator, agent]
status: current
tags: [jump-cannon, troubleshooting]
---

# Troubleshooting

Check the layers in order: stable service-name resolution, Gateway or NetBird
routing, graph-api health, asset delivery, graph counts and IDs, browser console,
WebGPU canvas, compute admission, then dashboard ingestion.

For search or filtering, fetch `GET /graph/schema` and confirm its
`graph_revision`, source identity, declared query keys, and facetable fields.
An importer that omits a required schema or emits a missing, duplicate,
undeclared, or mistyped search document now fails the rebuild; the old complete
snapshot stays live. A field-qualified query absent from the active schema is a
client error (HTTP 400), not a reason to fall back to title matching. Do not
diagnose graph-api by looking for a `vault-search` child process: active search
is built in-process with the graph snapshot.

Use direct evidence from [[Service Access]], [[Backend API]], [[Browser Regression]],
[[Kueue Scheduling]], and [[Observability]]. Fix the declarative owner instead
of adding host-local repair scripts.
