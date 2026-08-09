---
doctype: runbook
area: compute
audience: [operator, agent]
status: current
tags: [jump-cannon, kueue, gpu]
---

# Kueue Scheduling

Kueue is the admission boundary for scarce cluster GPU capacity. Ray sessions
and performance Jobs select the environment's LocalQueue and workload priority.
A zero GPU quota safely leaves work queued until an operator grants capacity.

Preserve on-demand semantics: request resources, wait for admission, run, and
release them. When the environment grants a small standing GPU envelope,
[[Ray GPU Sessions]] become self-service: a session admits on dispatch and
releases its quota when its RayCluster is deleted on park. The envelope, not
the controller, bounds consumption. See [[Ray GPU Sessions]], [[Scheduled Tests]], and
[[Large Jobs and Training]].
