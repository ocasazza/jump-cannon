---
doctype: guide
area: compute
audience: [developer, operator, agent]
status: current
tags: [jump-cannon, compute]
---

# Compute

graph-api is the only frontend-facing compute boundary. Local layouts run in
the browser. Cluster-hosted GPU sessions use [[Ray GPU Sessions]] admitted by
[[Kueue Scheduling]].

Batch expansion and future training belong in [[Large Jobs and Training]], not
inside the UI. Measure session startup and workload behavior through
[[Performance Engineering]] and [[Observability]].
