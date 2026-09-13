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
is ~12 MB. graph-api serves the dist from `--assets-dir` with **gzip**
(~12 MB → ~4 MB, compressed once per file and cached in-process) and an
**ETag**, so a reload revalidates to `304` and re-streams nothing.

`Cache-Control` is deliberately `no-cache` rather than `immutable`: Trunk
builds with `filehash = false`, so the bundle filename is stable across
deployments and an immutable cache would pin a stale build in every browser.

Serving the bundle raw made every cold load one long HTTP/2 stream. Through
the cluster proxy that showed up as `ERR_HTTP2_PING_FAILED` with a 200 status,
then `WebAssembly compilation aborted: Response body loading was aborted` — a
reset mid-body, not a server error. If it recurs on a very slow link, the
remaining lever is proxy-side (HTTP/2 keepalive-ping and stream-idle timeouts
on the gateway that fronts `jump-cannon.proxy.cluster.nixstation.internal`),
which lives outside this repository.

