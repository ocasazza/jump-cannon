---
doctype: runbook
area: compute
audience: [developer, operator, agent]
status: current
tags: [jump-cannon, gpu]
---

# GPU Sessions

The compute backend runs as a Kueue-admitted `batch/v1` Job in the compute
namespace: one pod with a single `graph-compute` container (the Rust gRPC
layout server) that requests the GPU. graph-api dials a stable ClusterIP
compute Service in front of the Job pod; the frontend never addresses a pod or
GPU node directly.

Every Job is created suspended and carries the Kueue queue, priority-class, and
`kueue.x-k8s.io/max-exec-time-seconds` labels. Kueue owns `spec.suspend`:
admission unsuspends the Job, preemption re-suspends it and re-queues its
Workload. graph-api never touches `spec.suspend`.

Two lifecycle modes, selected by `graphCompute.session.enabled`:

- **Static (default).** The chart declares the Job and its Service directly.
  The Job queues until [[Kueue Scheduling]] grants quota and runs until
  deleted or until the max-exec-time cap deactivates it.
- **On-demand session.** The chart renders the Job manifest into a template
  ConfigMap instead, plus bounded RBAC in the compute namespace. graph-api's
  session controller owns the Job and its Service: dispatch (selecting a
  cluster engine or the Dispatch action) creates the Service and the Job from
  the template; park (explicit, idle timeout, or the hard cap) deletes the
  Job with background propagation, then the Service. Deleting the Job is the
  only supported park — its pod dies with it and the quota is released.

A third mode ships with the [[Session Manager]]: when `sessionManager.gpu` is
enabled, its broker owns **one Kueue-admitted Job (and Service) per world**
from the chart's `__world__`-placeholder template ConfigMap, with the same
dispatch/park lifecycle, idle auto-park, and hard caps. All per-world Jobs
draw from the same standing LocalQueue envelope — the envelope, not the
broker, bounds consumption. This mode is mutually exclusive with
`graphCompute.session.enabled`; the chart fails the render if both are set.

Session state (`GET /compute/session`) is derived from the Job, its pod, and
its Kueue Workload (the one owned by the Job), never stored:

| State | Meaning |
|---|---|
| `parked` | No Job exists. |
| `dispatching` | Desired running; the Service and Job are being created. |
| `queued` | Job created and its Workload lacks quota (or Kueue has not created the Workload yet), or the Workload was evicted for a requeueable reason (for example `Preempted`) and waits again. |
| `admitted` | Quota reserved and admitted; the Job is unsuspended and the pod is scheduling. |
| `starting` | Job pod running; waiting for the worker to become healthy. |
| `ready` | Worker healthy and serving. |
| `parking` | Service and Job are being deleted. |
| `failed` | Admission, start, runtime, or crash-loop failure; see below. |

The controller adopts a pre-existing Job on startup, so restarts and external
deletions self-heal to parked. Progress and bounded `graph-compute` logs from
the Job pod stream to the `gpu-session` group on [[Backend API]]'s
`/progress`; the cluster gallery renders the console.

Preemption shows as `queued` again, then `admitted → starting → ready` after
re-admission; the fresh worker starts at graph revision 0 (see the seed note
below). Kueue deactivation — the max-exec-time label exceeded, or the Workload
`spec.active` set to false — fails the session and auto-parks it.

Safety rails:

- an admission timeout, measured from when the Workload last lacked quota,
  fails a session that never gets quota;
- the `kueue.x-k8s.io/max-exec-time-seconds` label is a Kueue-enforced hard cap
  on admitted runtime, and the controller enforces its own hard session cap and
  idle auto-park;
- a watchdog deletes the Job and fails the session if no Kueue Workload appears
  within 60 s of create;
- a start timeout fails a session that stays admitted or starting too long;
- crash-loop detection (CrashLoopBackOff or restart count) fails the session,
  then auto-parks after a 60 s log-grace window so the logs can be read;
- the Job `backoffLimit` bounds pod restarts as a backstop;
- session mode refuses to run with more than one graph-api replica.

Selecting a remote engine does not by itself give the worker a graph: the
broker only reselects, and `/compute/health` legitimately reports
`graph_revision: 0` until the UI sends a seed/solve
(`PUT /compute/initial-placement`, which loads the graph server-side). A
ready session with graph revision 0 is waiting for a seed, not broken.

Escape hatch: if a deleted Job or its Workload sticks terminating on a Kueue
finalizer, confirm the Workload is finished, then remove the finalizer by hand
(`kubectl -n gpu-workloads patch workload <name> --type=json
-p '[{"op":"remove","path":"/metadata/finalizers/0"}]'`). The controller
never force-removes finalizers.

Observe startup and execution through [[Backend API]] and [[Observability]].
Do not create an always-on bypass around [[Kueue Scheduling]].
