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
Jump Cannon chart creates no network exposure by default.

`routing.enabled` is the opt-in alternative: the chart then owns its Gateway
API `HTTPRoute` (graph-api, plus the session manager when that component and
its hostnames are set) **and** the transport tuning that a ~3.5 MB WASM bundle
needs — see the delivery numbers and the blast-radius note in
[[Performance Engineering]]. Colocating the two is the point: a route whose
timeouts and HTTP/2 windows live in another repository drifts away from the
bundle it carries. Rendering fails loudly when `routing.enabled` is set without
a parent Gateway or a hostname, and when a session-manager hostname is given
without the component enabled.

Model the resource declaratively and let controller status drive readiness.
Follow [[Service Access]], [[GitOps Release]], and [[Security Model]].
