# Session manager plan and handoff

Status: all milestones implemented and **deployed** (2026-08-14). The cluster
runs the session manager (`jump-cannon-session-manager` image, minigraf
store, per-world GPU broker; single-tenant `graphCompute.session` retired)
behind the NetBird proxy at
`https://jump-cannon-sessions.proxy.cluster.nixstation.internal`. Worlds
publish namespaced node IDs (`world:<world-slug>:<local>`) under discovery
schema v2. Client errors ship to `POST /log/client` (pod logs, target
`jump_cannon::client`).

This is the living handoff for the k8s-native session manager arc: shared
versioned worlds, Jujutsu-inspired graph versioning over minigraf (embedded)
and TerminusDB (cluster), Kueue-shared per-world GPU, and a multi-view UI.

## Decisions (locked with the user)

- Both stores behind one `graph_vcs::VcsStore` abstraction: **minigraf**
  (embedded, native + WASM) and **TerminusDB** (server, cluster).
- VCS model is ours, jj-inspired: stable `ChangeId`s across rebase, op-log,
  first-class recorded conflicts (non-blocking), snapshot-based 3-way merge.
- The core abstract interface — `session_manager::WorldHost` — carries **no
  session-management component**. Multi-user session management is the
  optional `SessionDirectory` sub-trait reached via
  `WorldHost::sessions() -> Option<&dyn SessionDirectory>` (capability via
  `Option`, never stubbed). Single-user mode: one fixed local identity, one
  implicit session.
- Auth: the OIDC gateway injects `X-User`; the session-manager server trusts
  the header (name configurable via `JUMP_CANNON_USER_HEADER`) and enforces
  per-world ACLs. No OIDC flow in-app.
- Two deployment targets are first-class: standalone browser/Tauri
  (wgpu + minigraf, no backend GPU) and multi-GPU cluster (TerminusDB +
  Kueue-shared RayClusters for extremely large graphs).

## What exists now

| Piece | Where | State |
|---|---|---|
| `VcsStore` + jj model + `MinigrafStore` | `crates/graph-vcs` | done, 15 tests |
| `WorldHost`/`SessionDirectory` + `EmbeddedSessionManager` + conformance suite | `crates/session-manager` | done, 5 tests |
| graph-api split (`api_router`/`router`), `build_world_state`, `WatchPlan::Push` wired | `crates/graph-api` | done, behavior unchanged |
| `KubernetesSessionManager` server (`server` feature): auth middleware, VCS/session REST `/api/*`, per-world nested graph serving `/worlds/:name/*` (RwLock + tower oneshot mux), `WorldImporter` | `crates/session-manager` | done, 12 tests incl. HTTP conformance |
| `HttpSessionManager` client (wasm-clean, reqwest) | `crates/session-manager/src/http.rs` | done |
| `GpuBroker`: per-world RayClusters via wrapped `gpu_session::GpuSessionHandle`s, per-world compute Services, activity touch | `crates/session-manager/src/gpu_broker.rs` | done, disabled without template env |
| Helm: `sessionManager.*` values, Deployment/Service/PVC/RBAC/templates, `ci/session-manager.yaml`, mutual-exclusion `fail` vs `graphCompute.session` | `charts/jump-cannon` | done, helm template/lint green |
| App views: `AppView::{User,Sessions}` topbar switcher, per-view workspaces (`jc_layout_v9` / `jc_sessions_layout_v1`), `Ctx.host: Arc<dyn WorldHost>`, world-aware `api::url` via `WORLD_BASE`. **View switching mounts in place** (persisted `jc_view` + `ctx.view.set`): the views own separate workspaces, servers, and world hosts; the distinct `UserWorkspaceView`/`SessionsWorkspaceView` component types keep the switch a clean unmount/mount of hook scopes. The open world persists as `jc_world` and re-mounts on return to Sessions. `window.__jc_boot` still stamps boot for the browser suite. | `app/ui/src/main.rs`, `api.rs` | done |
| Sessions panels: Worlds, History, Branches, Merge, GPU Sessions. Settings also rides the Sessions dock (it carries the session-manager URL + x-user identity); palette jump-to-section commands are filtered at boot to panels the active view's workspace holds, and the new panels gate their empty states behind a first-fetch Spinner like the older panels. | `app/ui/src/panels/{worlds,history,branches,merge,gpu_sessions}.rs`, `palette.rs` | done |
| M7: world export/import (`WorldExport` v1, `MinigrafStore::restore_commit/restore_branch`) | `crates/session-manager/src/export.rs`, `crates/graph-vcs/src/minigraf_store.rs` | done, 5 tests |
| M7: wasm embedded persistence — localStorage export-snapshot per commit (`open_persistent`), NOT minigraf IndexedDB (see M7 note below) | `crates/session-manager/src/embedded.rs` | done |
| M7: embedded-world canvas (`rematerialize_embedded` → client `GraphData`), Worlds-panel commit editor (add node/edge, delete selected), export download / import upload | `app/ui/src/{main,graph_canvas}.rs`, `panels/worlds.rs` | done |

## M7 persistence finding (why localStorage, not IndexedDB)

minigraf 1.2.3's `browser` feature does contain a real IndexedDB backend,
but it is reachable only through `minigraf::browser::BrowserDb` — a
`#[wasm_bindgen]` JS-facing façade with `Rc<RefCell>` internals (not
`Send`/`Sync`, which `VcsStore: Send + Sync` requires), a Promise-based
string-Datalog API, and no multi-command transactions (`VcsStore` writes
bundle several commands into one `begin_write` transaction). `Minigraf`
itself has no public constructor over a custom storage backend, so the
IndexedDB pages cannot back a `VcsStore` without forking minigraf. The
shipped substitution: the embedded host keeps in-memory stores and writes
one `WorldExport` JSON per world to localStorage after every successful
mutation (`PersistingStore` hook), replayed on boot — the same format as
user-facing export/import. Best-effort (quota errors swallowed), full
history fidelity.

Follow-up fixes (2026-08-14), both caught by the browser suite's
world-persistence pass — the first time embedded world creation was
exercised in a real browser:

- Bare `minigraf` panics on wasm32: its transaction IDs call
  `SystemTime::now()` unless the `browser` feature switches them to
  `js_sys::Date::now()`. graph-vcs now enables `minigraf/browser` on
  wasm32 targets only (the IndexedDB façade remains unused; the clock is
  what we need). Without it, creating a world in the browser aborted the
  app on the first commit.
- Replayed worlds boot CLOSED (`open_persistent`), so the boot-restore's
  rematerialize hit `require_open`; `rematerialize_embedded` now
  re-opens a world the host still knows (and refuses to silently
  re-create a genuinely unknown one).
- The gpu_force pipelines created zero-sized storage buffers for an
  empty graph, flooding wgpu bind-group validation errors; vec-backed
  buffers now pad to one dummy element (`nonempty_f32`/`nonempty_u32`
  in gpu_force.rs).

## Key seams for the next session

- World graph serving: `POST /api/worlds/:id/vcs/*` mutations fire the
  world's push trigger → graph-api's Push driver rebuilds the served
  snapshot. The served branch is always `main` (`WorldImporter`).
- `HttpSessionManager::connect(url, UserIdentity)` fetches `/api/host`;
  `multi_user` decides whether `sessions()` is `Some`. The UI hides
  session/membership UI when not multi-user.
- GPU: `JUMP_CANNON_SM_GPU_TEMPLATE` + `_COMPUTE_NAMESPACE` etc. arm the
  broker; chart mounts a `__world__`-placeholder RayCluster template.
  Compute URL pattern: `http://<release>-<world>.<ns>.svc:50051`.
- panel-kit "views" (`use_views`, dynamic storage keys) is an upstream PR to
  the panel-kit repo — the app currently uses two static workspaces, which is
  functionally equivalent for two views.

## Remaining milestones

- **M6 TerminusStore**: `graph-vcs` backend mapping our commit/branch ops
  onto TerminusDB's native branch/merge/rebase (thin reqwest client or the
  community Rust client); optional `terminusdb` StatefulSet in the chart
  (`terminusdb.enabled=false` default, admin Secret); docker-gated parity
  tests against the `VcsStore` contract. `sessionManager.store` values key
  already validates `minigraf|terminusdb` (only minigraf wired).
- ~~**M7 browser-alone polish**~~ **done** (see the M7 table rows + the
  persistence finding above): world export/import (JSON `WorldExport`
  download/upload rather than raw `.graph` files — the export carries full
  history, not just head state), localStorage persistence for embedded
  wasm worlds, embedded-world canvas + commit editor.
- **M8 closeout**: knowledge notes (`Session Manager.md`,
  `Worlds and Versioning.md`; update `Start Here.md`, `Architecture.md`,
  `Compute.md`, `Kueue Scheduling.md`, `Ray GPU Sessions.md`,
  `Security Model.md`), `docs/importer-architecture.md` Phase-3 checkboxes
  (Push watch plan landed), browser-rust Sessions-view boot check, final
  gates (`cargo check --workspace --tests`, `just app-check`,
  `just test browser-rust`), then `git pull --rebase && git push`.

## Known limitations (documented in code)

- ~~**In-place view switching panics.**~~ **Resolved (2026-08-14).** The
  two Dioxus issues behind the reload-based switch: (1) a rules-of-hooks
  violation — `Workspace::render_with_header` calls panel bodies inline, so
  a second workspace must mount under a distinct component type (fix kept:
  `UserWorkspaceView`/`SessionsWorkspaceView` + `ViewWs` prop wrapper);
  (2) the `GlobalSignal::write() → AlreadyBorrowed` panic (empty
  `borrowed_at`, untracked `peek()` guard) did **not** reproduce when the
  switch was reduced to a bare `ctx.view.set(view)` and driven by the
  Rust browser suite (Chromium, embedded host, default layouts): the
  Sessions workspace mounted cleanly, including a maximized-panel
  round-trip. The panic therefore lived in the *extra synchronous work*
  the old in-place attempt did inside the click handler (world-base
  detach + full graph invalidation + reload writing dozens of
  GlobalSignals on the same stack as the pending flush), not in mounting
  the second workspace itself. The shipped `switch_view` keeps the
  handler minimal: set the view signal, detach/re-attach `WORLD_BASE`,
  and let the ordinary spawned `reload_graph`/`rematerialize_embedded`
  paths re-base the graph. The browser suite's step 11 now covers
  in-place switching (Sessions mount, User return, maximized-panel
  round-trip) against the shared console-error gate, which would catch a
  panic-hook firing.
- Frontmatter is not projected into world search documents (fixed world
  schema); values remain visible via `/node/*id` `frontmatter_json`.
- Read ACLs are coarse: any authenticated user reads any world; writers are
  gated. Reader enforcement is a follow-up.
- Rebase replays merge commits in range as single-parent (first-parent).
- Retired worlds' GPU reconcile tasks have no shutdown signal (one detached
  parked-state task per retired world until process exit).
- Distinct world slugs sanitizing to the same DNS-1123 cluster name share a
  cluster (`my.world` vs `my-world`).
- The open world is persisted: `jc_world` localStorage (written by
  `open_world_in_view`, cleared by `clear_world_base`) is restored by the
  first run of the host-rebuild effect in `App` when booting into the
  Sessions view, and re-mounted by `switch_view` on returning to Sessions —
  for both embedded and HTTP session-manager hosts. The server-side open set
  is in-memory: a session-manager restart closes every world, and the
  Worlds panel's Open button re-opens it (`POST /api/worlds` is an idempotent
  reopen; `WorldExists` is treated as already-open success).
