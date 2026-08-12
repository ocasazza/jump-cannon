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
[[Kueue Scheduling]]. When the [[Session Manager]] is enabled it brokers the
same kind of Kueue-admitted RayCluster per world, sharing the standing GPU
envelope and mutually exclusive with the legacy single session mode.

Production compute is declared by [[Helm Deployment]] and the consuming
Kubernetes environment. The repository's Docker Compose output is only a local
development convenience; do not add a parallel repository-level infrastructure
stack.

Batch expansion and future training belong in [[Large Jobs and Training]], not
inside the UI. Measure session startup and workload behavior through
[[Performance Engineering]] and [[Observability]].
