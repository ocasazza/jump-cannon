---
doctype: runbook
area: operations
audience: [user, operator, agent]
status: current
tags: [jump-cannon, networking]
---

# Service Access

Clients connect through a stable service name, not a Kubernetes ClusterIP. The
cluster Service is an internal implementation detail; Gateway and NetBird
resources provide the supported access path.

The interactive browser route must terminate TLS. Graph rendering uses WebGPU,
which browsers expose only in a secure context; opening the internal HTTP
Service by a non-loopback hostname leaves Nodes available but produces a clear
Graph-panel remediation instead of attempting an unsupported backend.

Keep names and routing declarative in the environment repository. Do not add
host-local DNS repair loops or embed IP addresses in the frontend. See
[[NetBird Access]], [[Security Model]], and [[Troubleshooting]].
