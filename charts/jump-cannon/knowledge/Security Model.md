---
doctype: runbook
area: security
audience: [developer, operator, agent]
status: current
tags: [jump-cannon, security]
---

# Security Model

The chart runs non-root, drops Linux capabilities, disables service-account
token automount by default, and keeps packaged documentation in ConfigMaps.
Never put credentials or sensitive source data in Helm values or this graph.

Importer access is explicit and least-privilege. Network exposure is
environment-owned through [[Service Access]]. GPU work is admitted through
[[Kueue Scheduling]]. Review these boundaries in [[Helm Deployment]].
