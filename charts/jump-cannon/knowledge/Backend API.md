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
graph snapshots so in-flight requests do not observe partial reloads.

Bulk arrays use little-endian numeric buffers; structured messages use protobuf
or JSON where appropriate. See [[Architecture]], [[Observability]], and
[[Security Model]].
