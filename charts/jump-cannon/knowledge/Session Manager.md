---
doctype: architecture
area: development
audience: [user, developer, operator, agent]
status: current
tags: [jump-cannon, session-manager, worlds]
---

# Session Manager

The session manager hosts multi-user shared versioned worlds alongside
graph-api. graph-api keeps serving one configured source; the session manager
adds worlds that users create, commit to, branch, and merge — see
[[Worlds and Versioning]]. The Dioxus app reaches it through the **User /
Sessions** topbar switcher.

The core interface is `session_manager::WorldHost`: open/list/close worlds and
reach each world's VCS store and compute endpoint. It carries **no
session-management component** — a single local user is valid as-is. Multi-user
is the optional `SessionDirectory` capability reached via
`WorldHost::sessions() -> Option<...>`; `None` means absent, never stubbed.

Three hosts implement the same contract:

- **EmbeddedSessionManager** — single-user, one local identity, one minigraf
  store per world. Native keeps a file per world plus a `worlds.json`
  manifest; in the browser the stores are in-memory and every mutation is
  re-exported to localStorage (minigraf's IndexedDB backend cannot back a
  VcsStore). This host backs the standalone browser/Tauri app.
- **KubernetesSessionManager** — the cluster server: axum, per-world ACLs, the
  REST API below, per-world nested graph-api serving, and a `GpuBroker` that
  manages one Kueue-admitted RayCluster per world (see [[Ray GPU Sessions]]).
- **HttpSessionManager** — the wasm-clean client of that REST API; the app
  swaps to it when a session-manager URL is configured.

The **X-User trust boundary**: an authenticating gateway injects the identity
header (`x-user` by default, configurable via `userHeader` /
`JUMP_CANNON_USER_HEADER`). The server trusts the header and enforces
per-world ACLs; requests without it get 401 and only `/healthz` is exempt.
There is no OIDC flow in-app — see [[Security Model]].

REST surface:

- `/api/host`, `/api/worlds*` — host descriptor, world lifecycle, ACLs.
- `/api/worlds/:id/vcs/*` — branches, log, commits, merges, rebases,
  conflicts, resolutions, diff, op-log, snapshots.
- `/api/worlds/:id/sessions*`, `/api/worlds/:id/members` — the multi-user
  directory.
- `/api/worlds/:id/compute*` — dispatch/park/status of the per-world GPU
  session, admitted through [[Kueue Scheduling]].
- `/worlds/:name/*` — a full nested graph-api per world (the same HTTP
  contract as [[Backend API]]). VCS commits on the served `main` branch
  rebuild the served snapshot atomically.

Store selection is `sessionManager.store`: **minigraf** (default; one
`<world-slug>.graph` file per world on the PVC) or **terminusdb** (one
TerminusDB database per world; requires the chart's optional `terminusdb`
StatefulSet, whose admin password comes only from a user-created Secret).

Deploy through the chart's `sessionManager.*` values (Deployment, Service,
world-store PVC, RBAC, per-world RayCluster template). Exactly one replica is
supported, and `sessionManager.gpu` is mutually exclusive with the legacy
`graphCompute.session` mode — the chart fails the render otherwise. Review the
values in [[Helm Deployment]] and the component picture in [[Architecture]].

**Deployed** (2026-08-14): the consumer component
(`envoy-ai-gateway/.../platforms/jump-cannon.nix`) enables the manager with
the minigraf store and the per-world GPU broker, and exposes it through the
NetBird TLS proxy at
`https://jump-cannon-sessions.proxy.cluster.nixstation.internal` (the shared
proxy injects `x-user`; browsers put this URL in Settings → session manager).
The single-tenant `graphCompute.session` is retired there; per-world
dispatch creates a Kueue-held RayCluster in `gpu-workloads` until quota is
granted ("waiting for a seed, not broken" — [[Ray GPU Sessions]]). The `jump-cannon-session-manager` image publishes like the other runtime images
([[GitOps Release]]).

**Container restart hazard (fixed in graph-vcs):** minigraf's file lock
rejects a holder PID equal to the current process — but every container's
main process is PID 1 in its own PID namespace, so a stale
`<world>.graph.lock` from a previous pod used to brick the world after any
pod restart. `MinigrafStore::open` now keeps a weak-token registry of paths
it actually holds and reclaims stale same-PID/dead-PID locks; genuine
same-process double-opens and foreign live holders still fail. If a world
ever reports "locked by another process" with a LIVE different PID, that is
a real second mount — do not delete the lock blindly.
