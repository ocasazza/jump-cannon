---
doctype: runbook
area: operations
audience: [operator, agent]
status: current
tags: [jump-cannon, gitops, flux]
---

# GitOps Release

The release sequence is source commit, Hydra image and chart builds, secured
artifact publication, consumer chart lock update, rendered manifest review,
Flux reconciliation, and live workload verification. Each step needs its own
evidence; a source build does not prove the cluster updated.

Environment-owned resources include [[NetBird Access]] and GPU admission under
[[Kueue Scheduling]]. Confirm the deployed result in [[Observability]].
