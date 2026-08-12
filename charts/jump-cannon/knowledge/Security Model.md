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
On-demand GPU session mode (see [[Ray GPU Sessions]]) is the one exception:
the controller manages the session RayCluster through the in-cluster API, so
session mode forces token automount on with the bound gpu-session Role.
Never put credentials or sensitive source data in Helm values or this graph.

Importer access is explicit and least-privilege. Network exposure is
environment-owned through [[Service Access]]. GPU work is admitted through
[[Kueue Scheduling]]. Review these boundaries in [[Helm Deployment]].

`GET /importers` and **Settings > Importers** expose only sanitized deployment
metadata. They never expose credentials or add an unauthenticated Apply, Run,
or Activate path. Filesystem source instances declare the exact claim, mount,
input path, and read-only state. The Lavender OKF profile may read only the
shared OKF repository claim; it must never mount Lavender lake or state data.

The [[Session Manager]] adds an identity boundary: an authenticating gateway
injects the `x-user` header (name configurable) and the server trusts it —
requests without it get 401 and only `/healthz` is exempt. Never expose the
session manager without that gateway; the header is trivially forgeable
otherwise. Enforcement is per-world ACLs on writes: any authenticated user may
read any world today (coarse-reader limitation; reader enforcement is a
follow-up), while ACL-listed writers commit, merge, rebase, and mutate ACLs.
The optional TerminusDB backend takes its admin password only from a
user-created Secret, never from Helm values.
