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

### Transport tuning lives in the chart — where an Envoy Gateway fronts it

`routing.*` ([[Helm Deployment]], off by default) ships the gateway knobs with
the workload:

- **HTTP/2 flow control.** Envoy's per-stream initial window defaults to
  **64 KiB**; a multi-megabyte body then waits on window updates instead of
  sending bytes. `routing.clientPolicy` raises the stream window to 1 MiB and
  the connection window to 16 MiB.
- **Timeouts sized for the bundle, not the median request.** The HTTPRoute
  carries `request`/`backendRequest`, and the route-scoped
  `BackendTrafficPolicy` carries `requestTimeout`.
- **Buffer limits.** The CRD default is 32768 bytes.

Blast radius is why the two policies differ: `BackendTrafficPolicy` attaches to
this chart's own HTTPRoutes, but `ClientTrafficPolicy` can only attach to a
**Gateway** — the CRD forbids every other target and does not support
`sectionName` — so enabling it changes transport for every workload behind that
Gateway. Field paths verified against the Envoy Gateway v1.4.2 CRDs.

**These policies only do something when an Envoy Gateway terminates the
route.** They are `gateway.envoyproxy.io` resources, reconciled by Envoy
Gateway. On the nixstation deployment the front door is *not* Envoy: the
`netbird-private` Gateway has `gatewayClassName: netbird-private`
(controller `gateway.netbird.io/controller`), and TLS terminates in
`netbirdio/reverse-proxy` pods whose only configuration is `NB_PROXY_*` env —
no HTTP/2 or timeout surface at all. Enabling `routing.clientPolicy` there
would create an object nothing reads. See [[NetBird Access]].

### Measured: the NetBird path, not the gateway, was the 2026-09-13 constraint

Against the live deployment (before the compression work reached it):

| measurement | value |
|---|---|
| bundle served | 11,921,982 B, no `Content-Encoding` |
| sustained throughput | **17.3 KB/s** (2,077,926 B in 120 s, HTTP/2, 200) |
| implied download time | ~11.5 minutes |
| netbird-proxy CPU throttling | 62 of 225,496 periods (**0.027%**) — not the constraint |
| netbird peers | 10 of 11 `Connection type: Relayed`, `Relays: 1/3 Available` |

So the client had no direct WireGuard path and every byte crossed one ws relay
(`rels://pdx-nxnx-lv02.schrodinger.com:443`, which itself answers `426 Upgrade
Required` in 0.34 s — healthy). Chrome gives up on a stream that long
(`ERR_HTTP2_PING_FAILED`), which is the reported `WebAssembly compilation
aborted`.

Compression is necessary but not sufficient on that path: 3.5 MB at 17 KB/s is
still ~3.4 minutes. What compression *does* buy unconditionally is the warm
reload — `304`, zero bytes. Restoring a peer-to-peer path (or relay capacity
closer to the client) is the lever that makes a cold load viable; that lives in
the nixstation NetBird deployment, not here.
