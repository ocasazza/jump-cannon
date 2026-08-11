---
doctype: runbook
area: compute
audience: [developer, operator, agent]
status: current
tags: [jump-cannon, ray, gpu]
---

# Ray GPU Sessions

The compute backend runs as a Kueue-admitted RayCluster (CPU head, GPU
graph-compute sidecar) in the compute namespace. graph-api dials the stable
compute Service; the frontend never addresses a Ray pod or GPU node directly.

Two lifecycle modes, selected by `graphCompute.session.enabled`:

- **Static (default).** The chart declares the RayCluster directly. It queues
  until [[Kueue Scheduling]] grants quota and exists until deleted or the TTL
  janitor reaps it.
- **On-demand session.** The chart renders the same RayCluster manifest into a
  template ConfigMap instead, plus bounded RBAC in the compute namespace.
  graph-api's session controller owns the CR: dispatch (selecting a cluster
  engine or the Dispatch action) creates it; park (explicit, idle timeout, or
  the hard cap) deletes it. Deleting the CR is the only supported park —
  Kueue owns `spec.suspend` while admitted and reverts user changes.

Session state (`GET /compute/session`) is derived from cluster objects, never
stored: parked, dispatching, queued, admitted, head_starting, ready, parking,
failed. The controller adopts a pre-existing CR on startup, so restarts and
external deletions (eviction, janitor) self-heal to parked. Progress and
bounded Ray head / graph-compute logs stream to the `gpu-session` group on
[[Backend API]]'s `/progress`; the cluster gallery renders the console.

The RayCluster base image is the official `rayproject/ray` build with Ray
preinstalled — never bootstrap Ray with a runtime `pip install` init
container: cluster egress MITMs PyPI TLS, and the pull-then-install path made
session starts slow and fragile.

Safety rails: an admission timeout parks a session that never gets quota; a
`kueue.x-k8s.io/max-exec-time-seconds` label caps admitted runtime; a
watchdog deletes the CR and fails the session if no Kueue Workload appears
after create; session mode refuses to run with more than one graph-api
replica.

Selecting a remote engine does not by itself give the worker a graph: the
broker only reselects, and `/compute/health` legitimately reports
`graph_revision: 0` until the UI sends a seed/solve
(`PUT /compute/initial-placement`, which loads the graph server-side). A
ready session with graph revision 0 is waiting for a seed, not broken.

Escape hatch: if a parked CR sticks terminating on the Kueue finalizer,
confirm the Workload is finished, then remove the finalizer by hand
(`kubectl -n gpu-workloads patch workload <name> --type=json
-p '[{"op":"remove","path":"/metadata/finalizers/0"}]'`). The controller
never force-removes finalizers.

Observe startup and execution through [[Backend API]] and [[Observability]].
Do not create an always-on bypass around [[Kueue Scheduling]].
