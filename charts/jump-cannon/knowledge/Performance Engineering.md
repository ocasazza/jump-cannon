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
