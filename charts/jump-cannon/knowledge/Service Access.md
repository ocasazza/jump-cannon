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

Keep names and routing declarative in the environment repository. Do not add
host-local DNS repair loops or embed IP addresses in the frontend. See
[[NetBird Access]], [[Security Model]], and [[Troubleshooting]].
