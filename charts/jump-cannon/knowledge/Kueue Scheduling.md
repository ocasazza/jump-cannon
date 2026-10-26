---
doctype: runbook
area: compute
audience: [operator, agent]
status: current
tags: [jump-cannon, kueue, gpu]
---

# Kueue Scheduling

Kueue is the admission boundary for scarce cluster GPU capacity. GPU session
and performance Jobs select the environment's LocalQueue and workload priority.
A zero GPU quota safely leaves work queued until an operator grants capacity.

Preserve on-demand semantics: request resources, wait for admission, run, and
release them. When the environment grants a small standing GPU envelope,
[[GPU Sessions]] become self-service: a session admits on dispatch and
releases its quota when its Job is deleted on park; preemption re-suspends the
Job and re-queues its Workload. The envelope, not the controller, bounds
consumption. The [[Session Manager]]'s per-world
Jobs are admitted into that same envelope: several worlds may hold GPU
sessions concurrently only while their sum fits the standing quota; beyond it,
further dispatches queue. See [[GPU Sessions]], [[Scheduled Tests]], and
[[Large Jobs and Training]].
