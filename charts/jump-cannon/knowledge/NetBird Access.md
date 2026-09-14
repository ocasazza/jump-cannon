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

The proxy in front of Jump Cannon is `netbirdio/reverse-proxy`, reconciled from
a `netbird-private` Gateway by the netbird-operator — **not** Envoy Gateway. It
exposes no HTTP/2, timeout, or buffer configuration, so the chart's
`routing.backendPolicy` / `routing.clientPolicy` (both `gateway.envoyproxy.io`
resources) are inert on this path and should stay off here. They exist for a
deployment whose route is terminated by Envoy Gateway.

When a cold load of the frontend bundle aborts on this path, measure the link
before tuning anything: 2026-09-13 saw 17.3 KB/s with 10 of 11 peers relayed,
which no origin-side compression can rescue ([[Performance Engineering]]).
