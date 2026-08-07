---
doctype: runbook
area: compute
audience: [developer, operator, agent]
status: current
tags: [jump-cannon, ray, gpu]
---

# Ray GPU Sessions

The chart can declare a RayCluster with a CPU head and GPU worker. graph-api
connects to the compute service; the frontend never addresses a Ray pod or GPU
node directly.

Use workload selectors, bounded resources, TTL, and the existing queue policy.
Do not create an always-on bypass around [[Kueue Scheduling]]. Observe startup
and execution through [[Backend API]] and [[Observability]].
