---
doctype: runbook
area: operations
audience: [operator, agent]
status: current
tags: [jump-cannon, netbird, networking]
---

# NetBird Access

The consuming Envoy AI Gateway deployment declares the NetBird
`NetworkResource` and routes a stable service name to Jump Cannon. The portable
Jump Cannon chart intentionally does not create environment-specific network
exposure.

Model the resource declaratively and let controller status drive readiness.
Follow [[Service Access]], [[GitOps Release]], and [[Security Model]].
