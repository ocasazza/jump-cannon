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
metadata. They never expose credentials. Filesystem source instances declare
the exact claim, mount, input path, and read-only state. The Lavender OKF
profile may read only the shared OKF repository claim; it must never mount
Lavender lake or state data. The okf-sync CronJob authenticates to GitHub with
a read-only deploy key scoped to the `schrodinger/lavender-okf` repository;
the private key lives only in a user-created Secret (mounted mode `0400`,
never in values), so the sync job can pull but never push.

When `importers.runtimeSwitchGroup` is set, a viewer may select any runnable
catalog source at runtime from **Settings > Importers** instead of waiting for
a Helm rollout. graph-api gates the switch on the proxy-injected
`x-netbird-groups` header containing the configured group, and every
configured filesystem source is already mounted read-only in the pod. Writes,
generation, and GPU compute remain bound to the deployment-selected source.
Known limitation: the direct L3 NetworkResource route bypasses the proxy, so
the group header is forgeable by any NetBird client on that route — the gate
deters casual access, not a determined insider.

The [[Session Manager]] adds an identity boundary: an authenticating gateway
injects the `x-user` header (name configurable) and the server trusts it —
requests without it get 401 and only `/healthz` is exempt. Never expose the
session manager without that gateway; the header is trivially forgeable
otherwise. Enforcement is per-world ACLs on writes: any authenticated user may
read any world today (coarse-reader limitation; reader enforcement is a
follow-up), while ACL-listed writers commit, merge, rebase, and mutate ACLs.
The optional TerminusDB backend takes its admin password only from a
user-created Secret, never from Helm values.
