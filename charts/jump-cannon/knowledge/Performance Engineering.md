---
doctype: runbook
area: quality
audience: [developer, operator, agent]
status: current
tags: [jump-cannon, performance]
---

# Performance Engineering

Measure one variable at a time and retain the baseline, graph size, hardware,
and configuration with every result. Separate load time, layout convergence,
frame latency, API latency, and remote-session startup rather than combining
them into one score.

Weekly cluster results flow through [[Scheduled Tests]] to [[Observability]].
GPU-backed runs follow [[Kueue Scheduling]] and [[Ray GPU Sessions]].

Merge-time Metal Criterion runs publish their native reports as immutable Hydra
build products. Compare retained Hydra results directly; benchmark derivations
must not manage credentials, upload into a mutable dashboard store, or depend on
an out-of-band format-conversion script.

## Frontend bundle delivery

Load time starts with one large artifact: the release `jump-cannon-ui_bg.wasm`
is ~12 MB raw. graph-api serves the dist from `--assets-dir` compressed once
per file and cached in-process, preferring the smallest encoding the client
accepts:

| encoding | wire bytes | note |
|---|---|---|
| brotli (q5) | 3.5 MB | what every current browser gets |
| gzip (level 6) | 4.15 MB | fallback, and for `br;q=0` |
| identity | 12.0 MB | clients that send no `Accept-Encoding` |

Every response carries an `ETag` over the content, so a reload revalidates to
`304` with an empty body. `Cache-Control` is deliberately `no-cache` rather
than `immutable`: Trunk builds with `filehash = false`, so the bundle filename
is stable across deployments and an immutable cache would pin a stale build in
every browser.

Serving the bundle raw made every cold load one long HTTP/2 stream. Through the
cluster proxy that surfaced as `ERR_HTTP2_PING_FAILED` with a 200 status, then
`WebAssembly compilation aborted: Response body loading was aborted` — a reset
mid-body, not a server error.

### Transport tuning lives in the chart

The second half of that failure is gateway-side, and the knobs now ship with
the workload (`routing.*` in [[Helm Deployment]], off by default):

- **HTTP/2 flow control.** Envoy's per-stream initial window defaults to
  **64 KiB**. A multi-megabyte body then spends its time waiting for window
  updates rather than sending bytes, which is exactly what a high-latency link
  cannot afford. `routing.clientPolicy` raises the stream window to 1 MiB and
  the connection window to 16 MiB.
- **Timeouts sized for the bundle, not the median request.** The HTTPRoute
  carries `request`/`backendRequest` timeouts and the route-scoped
  `BackendTrafficPolicy` carries `requestTimeout`; a 3.5 MB body on a 1 Mbps
  link is ~28 s of streaming, so a default-ish 15 s route timeout resets it.
- **Buffer limits.** The CRD default is 32768 bytes; a multi-megabyte body
  streams through that in hundreds of refills.

Blast radius is why the two policies differ: `BackendTrafficPolicy` attaches to
this chart's own HTTPRoutes, but `ClientTrafficPolicy` can only attach to a
**Gateway** — the CRD forbids every other target and does not support
`sectionName` — so enabling it changes transport for every workload behind that
Gateway. It is opt-in for exactly that reason. Field paths were verified against
the Envoy Gateway v1.4.2 CRDs.
