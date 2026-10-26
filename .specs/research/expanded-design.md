# Expanded Design — Proposal A: Transform‑Package + Run‑Ledger

Status: design, ready for implementation. Supersedes the high-level sketch in
`.specs/research/proposals.a.md` by filling every gap the pruning evaluation
flagged (CK‑010 … CK‑013, and the D1/D4/D5/D6 improvements). Nothing here adds a
new crate, a new engine tag, or a new scheduler — the feature is assembled from
knobs that already exist in the workspace.

## 0. Vocabulary & scope guardrails

- **No omp/agent/session vocabulary in core types.** The domain nouns are
  `Transform`, `Run`, `RunLedger`, `RunPlan`, `RunDelta`, `Provenance`. These
  describe graph transformations, not autonomous-agent loops. (The `Worlds`
  session machinery in `crates/session-manager` is unrelated and untouched.)
- **No new engine.** Transforms ride the existing `tvix` engine
  (`crates/importer/src/tvix.rs`, `ENGINE = "tvix"`). The package declares
  `input = "graph"` and `role = "transform"`; both are new keys on the *existing*
  tvix manifest, not a new `ParserConfig` variant beyond the one `Tvix` arm that
  already exists.
- **No new crate.** All Rust lands in `crates/vault-data`, `crates/importer`,
  `crates/graph-api`, and `app/ui`. The Nix combinators land in the existing
  `crates/tvix-wasm/src/nix/` tree.
- **Dual-target.** Everything compiles for native *and* `wasm32-unknown-unknown`.
  Manifest validation, variable resolution, and Nix-literal rendering stay
  wasm-clean (no `native` feature); evaluation (`tvix_wasm::eval_graph`) and
  graph projection stay behind the `native` feature exactly as the tvix engine
  already splits them.

---

## 1. Concrete types

All of the following are plain Rust; none introduce `async` into the trait
surface (`apply` is synchronous and CPU-bound, run on a blocking thread by the
host, mirroring `tvix_wasm::eval_graph`).

### 1.1 Identity newtypes (in `crates/vault-data/src/node.rs`)

`RunId` lives in `vault-data` because `NodeMeta::Provenance` must reference it and
`vault-data` is the lowest crate that both `importer` and `graph-api` depend on.
This is the only way to let node provenance and the run ledger share one id type
without a new crate.

```rust
/// Monotonic run identity, assigned by the RunLedger when a run is created.
/// Serialized as a bare integer; `Display` renders `run-00000042` for the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RunId(pub u64);

/// A canonical graph node id (`VaultNode::id`), e.g. `tvix:k8s:pod/web-7f…`.
/// Alias, not a newtype, so it stays assignment-compatible with existing APIs
/// that pass node ids as `String` (search, inspector, graph-vcs).
pub type NodeId = String;
```

### 1.2 Value — typed variable binding (in `crates/importer/src/transform.rs`, re-exported)

The `tvix` engine already models variable *types* with `TvixVarType`
(String/Integer/Number/Boolean) and resolves supplied strings to Nix literals.
`Value` is the resolved, typed counterpart the `Transform` trait receives, so a
transform author never re-parses strings:

```rust
/// A resolved, typed variable binding. One-to-one with `tvix::TvixVarType`.
/// Rendered to a Nix literal by `tvix::render_expression` at apply time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Value {
    String(String),
    Integer(i64),
    Number(f64),
    Boolean(bool),
    /// The current graph, bound when the package declares `input = "graph"`.
    /// Never supplied by an administrator; injected by the host (see §2.2).
    Graph(vault_data::VaultGraph),
}
```

### 1.3 Transform trait + descriptor (in `crates/importer/src/transform.rs`)

```rust
use std::collections::BTreeMap;
use vault_data::VaultGraph as Graph;

/// A graph → graph transformation. One `tvix` package = one `Transform`.
/// `apply` is synchronous and CPU-bound; hosts run it on a blocking thread.
pub trait Transform: Send + Sync {
    /// Static identity + the variable schema an administrator binds against.
    fn descriptor(&self) -> TransformDescriptor;

    /// Produce a new graph from the current one plus bound variables.
    /// `bound` always contains every declared variable (defaults applied),
    /// and — for `input = "graph"` packages — the injected `Value::Graph`.
    fn apply(
        &self,
        graph: &Graph,
        bound: &BTreeMap<String, Value>,
    ) -> Result<Graph, TransformError>;
}

/// Static, serializable description surfaced at `GET /runs` and in the Runs panel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransformDescriptor {
    /// Package id (`manifest.metadata.id`), e.g. "windowed-cluster".
    pub id: String,
    pub display_name: String,
    pub description: String,
    /// Declared variables (name → type/default/bounds), projected from the
    /// package's `[parser].variables`. Excludes the injected `graph` variable.
    pub variable_schema: Vec<TransformVariable>,
    /// Optional topos id to switch to when this transform's output snapshot
    /// becomes the active graph. `None` means the UI stays in its current topos
    /// (the default graph-exploration topos). When set, the UI looks up the id
    /// in its local `ToposRegistry` and switches the workspace panel set +
    /// layout. Declared in the package TOML as `topos_id` (see §10).
    pub topos_id: Option<String>,
    /// Optional Nix expression template that, when evaluated with the
    /// transform's bound variables + output graph, produces a JSON blob the
    /// target topos consumes as its initial state. Served at
    /// `GET /runs/:id/topos-state`. `None` means the topos boots with its
    /// built-in default state.
    pub topos_state_template: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransformVariable {
    pub name: String,
    pub var_type: String,   // mirrors TvixVarType::as_str()
    pub default: Option<String>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub description: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum TransformError {
    #[error("evaluation failed: {0}")]
    Eval(String),
    #[error("transform produced a malformed graph: {0}")]
    Projection(String),
    #[error("cap exceeded: {detail}")]
    CapExceeded { detail: String },
    #[error("missing or out-of-bounds variable: {0}")]
    Variable(String),
}
```

### 1.4 Run, RunDelta, RunStatus (in `crates/graph-api/src/runs.rs`)

`Run` matches the spec's `score_4` anchor exactly: id, transform id, the bound
variables, the input/output snapshot revisions, status, and the computed delta.
Revisions are the same `u64` topology revision the snapshot already carries
(`x-graph-revision`), so a run's input/output are addressable and the UI's
existing revision guard (§4) can admit the output.

```rust
use std::collections::BTreeMap;
use vault_data::{NodeId, RunId};
use importer::transform::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Run {
    pub id: RunId,
    /// Package id of the transform that produced this run.
    pub transform_id: String,
    /// Variables bound for this run (administrator values + defaults).
    /// The injected `graph` binding is NOT persisted here (it is the input
    /// snapshot, addressed by `input_revision`).
    pub bound: BTreeMap<String, Value>,
    /// Topology revision of the snapshot fed in. `None` for a browser-owned
    /// (unaddressable) input graph.
    pub input_revision: Option<u64>,
    /// Topology revision of the snapshot published on success. `None` while
    /// pending/running or on failure.
    pub output_revision: Option<u64>,
    pub status: RunStatus,
    /// Node-level diff between input and output. Empty until the run succeeds.
    pub delta: RunDelta,
    /// Wall-clock start / finish (epoch millis), for the timeline animation.
    pub started_at: i64,
    pub finished_at: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RunDelta {
    pub added: Vec<NodeId>,
    pub removed: Vec<NodeId>,
    pub changed: Vec<NodeId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RunStatus {
    Pending,
    Running,
    Succeeded,
    Failed { reason: String },
}
```

`RunDelta` is computed by diffing the input and output `VaultGraph` by node id:
a node id present only in the output is `added`; present only in the input is
`removed`; present in both but with a changed content hash (see `Provenance`,
§2.1) is `changed`. Edges are not diffed at node granularity — the UI patches
the edge buffer wholesale only when `added`+`removed` is non-empty (§4).

### 1.5 RunPlan — scheduling, integrated with WatchPlan (in `crates/graph-api/src/runs.rs`)

`RunPlan` is **not** a new scheduler. It is a declaration a transform package
carries; the host maps a non-`Manual` plan onto the *existing* `WatchPlan` tick
machinery in `crates/graph-api/src/watcher.rs` — the same `tokio::time::interval`
loop that already drives `WatchPlan::Poll` importers (watcher.rs `spawn_poll`).

```rust
use std::time::Duration;

/// How often a transform re-runs. Declared in the package's `[parser]` table
/// (`schedule = "manual" | "every:1h" | "daily:09:00"`), parsed at validate time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunPlan {
    /// Runs only on `POST /runs/trigger`. The default.
    Manual,
    /// Runs on a fixed cadence. Mapped to `WatchPlan::Poll { interval_ms }`.
    Every(Duration),
    /// Runs once per day at a wall-clock time. Driven by the same interval
    /// loop, which wakes each minute and fires when the local clock crosses
    /// `at` (no cron dependency, no second timer thread).
    Daily(TimeOfDay),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeOfDay {
    pub hour: u8,   // 0..=23
    pub minute: u8, // 0..=59
}
```

Mapping (in `watcher.rs`, alongside the existing `match descriptor.watch`):

| `RunPlan`         | Driver                                                              |
|-------------------|--------------------------------------------------------------------|
| `Manual`          | no timer; only `POST /runs/trigger`                                 |
| `Every(d)`        | reuse `spawn_poll(state, d.as_millis(), gate)`; each tick triggers one run |
| `Daily(tod)`      | reuse the poll loop at a 60 s interval; fire only when the local clock first crosses `tod` this day |

A transform run shares the `RescanGate` discipline already in `watcher.rs`: a run
in flight suppresses a racing scheduled tick (`RescanDecision::SkipInFlight`), so
a transform slower than its cadence can never pile up.

### 1.6 RunLedger — bounded, JSONL-backed ring (in `crates/graph-api/src/runs.rs`)

```rust
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Mutex;

/// A bounded ring of the most recent runs, mirrored to a JSONL file so the
/// timeline survives a restart. Default capacity 256; overflow drops the
/// oldest entry (and rewrites the file by streaming the retained tail).
pub struct RunLedger {
    inner: Mutex<RunLedgerInner>,
    path: PathBuf,
    capacity: usize,
    next_id: std::sync::atomic::AtomicU64,
}

struct RunLedgerInner {
    entries: VecDeque<Run>,   // newest at back
}

impl RunLedger {
    pub const DEFAULT_CAPACITY: usize = 256;

    /// Open (or create) the ledger. File lives under the importer packages dir
    /// (see §2.6). On open, the last `capacity` lines are replayed into memory;
    /// `next_id` seeds from `max(id)+1`.
    pub fn open(path: PathBuf, capacity: usize) -> std::io::Result<Self> { /* … */ }

    /// Allocate the next RunId and append a `Pending` run. Appends one JSONL line.
    pub fn create(&self, transform_id: String, bound: BTreeMap<String, Value>,
                  input_revision: Option<u64>) -> RunId { /* … */ }

    /// Transition an existing run (Running → Succeeded/Failed), fill delta +
    /// output_revision + finished_at, and append the updated line. Readers see
    /// the newest line per id; compaction on overflow keeps the file bounded.
    pub fn update(&self, id: RunId, status: RunStatus, delta: RunDelta,
                  output_revision: Option<u64>) { /* … */ }

    /// Newest-first snapshot for `GET /runs`.
    pub fn list(&self) -> Vec<Run> { /* … */ }

    /// One run by id for `GET /runs/:id/diff`.
    pub fn get(&self, id: RunId) -> Option<Run> { /* … */ }
}
```

JSONL append is one line per state transition; `list`/`get` reduce by id
(last-writer-wins). On ring overflow the file is compacted by streaming only the
retained `capacity` runs' latest lines back out — bounded disk even under a tight
cadence (addresses the "disk-space exhaustion" risk in the proposal).

---

## 2. Concrete file paths and symbols

### 2.1 `crates/vault-data/src/node.rs` — provenance on `NodeMeta`

Add a `Provenance` struct and embed it in `NodeMeta` behind `#[serde(default)]`
so existing snapshots deserialize unchanged.

```rust
/// Run-level provenance: which transform runs first produced, last touched,
/// and changed this node's content. Written by the host after each run's diff.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Provenance {
    pub first_seen_run: Option<RunId>,
    pub last_seen_run: Option<RunId>,
    pub changed_in: Vec<RunId>,
    pub first_seen_at: i64, // epoch millis
    pub last_seen_at: i64,  // epoch millis
    /// 64-bit content hash of the node's canonical fields; drives the `changed`
    /// classification in `RunDelta`. 0 until the first run touches the node.
    #[serde(default)]
    pub content_hash: u64,
}
```

Add to `NodeMeta` (after `content_writable`):

```rust
    /// Transformation-run provenance. Empty for importer-produced nodes that
    /// have never passed through a transform.
    #[serde(default)]
    pub provenance: Provenance,
```

New symbols: `vault_data::Provenance`, `vault_data::RunId`, `vault_data::NodeId`,
`NodeMeta::provenance`.

### 2.2 `crates/importer/src/tvix.rs` — `role = "transform"` + `input = "graph"` + `Transform` impl

- **Manifest keys.** Extend `TvixParserToml` (tvix.rs ~line 150) with
  `role: Option<String>` (`"source"` default, or `"transform"`) and
  `schedule: Option<String>` (parsed into `RunPlan`). Extend `TvixVariableToml`
  handling so a variable declared `type = "graph"` (new `TvixVarType::Graph`
  arm, tvix.rs ~line 68) is recognized as the injected input, not an
  administrator-supplied value.
- **`validate` / `validate_config`** (tvix.rs 179 / 247): when `role =
  "transform"`, require exactly one `type = "graph"` variable named `graph`,
  require it has *no* default and *no* bounds, and reject it from the
  administrator-bindable set. A `source`-role package rejects any `graph`
  variable (keeps importers graph-free).
- **`resolve_variables`** (tvix.rs 317, and the dispatch in
  `lib.rs` 511): skip the `graph` variable during administrator resolution
  (it has no supplied string); everything else resolves unchanged. A new
  `inject_graph(resolved, graph)` helper renders the live graph to the Nix
  literal the expression expects — a `toGraphJSON`-shaped attrset
  (`{ nodes = [...]; links = [...]; }`) produced by reusing the inverse of
  `convert_generated_graph` (tvix.rs 516).
- **`Transform` impl.** `TvixImporter` (tvix.rs 655) gains
  `impl importer::transform::Transform for TvixTransform`, where `TvixTransform`
  wraps a validated transform package. `descriptor()` projects
  `TransformDescriptor` from the config's variables (excluding `graph`).
  `apply(graph, bound)` renders `( <expr> ) { graph = <injected>; <bindings> }`
  via `render_expression` (tvix.rs 362), calls `tvix_wasm::eval_graph`
  (native-only), and projects the result with `convert_generated_graph`,
  mapping any cap overflow to `TransformError::CapExceeded` (§7).

New symbols: `tvix::TvixVarType::Graph`, `tvix::TvixTransform`,
`tvix::inject_graph`, `TvixParserToml::{role, schedule}`.

### 2.3 `crates/importer/src/lib.rs` — package dispatch (no new engine tag)

- Reuse the existing `ParserConfig::Tvix` arm (lib.rs 166). The *role* is a
  field on `TvixEngineConfig`, not a new enum variant — the engine dispatch at
  lib.rs 383‑387 (`tvix::ENGINE => tvix::validate`) is unchanged.
- Add `ValidatedPackage::role()` and `ValidatedPackage::run_plan()` accessors
  (next to `engine_kind()` at lib.rs 410) that read the tvix config's
  `role`/`schedule`.
- Add `ValidatedPackage::into_transform(graph_binding_name) ->
  Result<Box<dyn Transform>, ImportError>` next to `build_tvix_importer`'s call
  path, returning a `TvixTransform` for `role = "transform"` packages and
  `WrongEngine`-style error otherwise.
- `pub mod transform;` added to `crates/importer/src/lib.rs`, re-exporting
  `Transform`, `TransformDescriptor`, `TransformVariable`, `TransformError`,
  `Value`.

Explicitly: **no new `ParserConfig` variant, no new `ENGINE` constant.** A
transform *is* a `tvix` package distinguished by `role` + `input = "graph"`.

### 2.4 `crates/tvix-wasm/src/nix/graph-transform.nix` — new combinator file

A sibling of `graph-combinators.nix`, following the exact `{ graph }`-parameter
pattern (§5). It is the transform author's standard library: graph→graph
combinators (filter, group, contract, relabel, densify) that operate on the
`{ nodes = [...]; links = [...]; }` attrset.

```nix
# graph-transform.nix — graph → graph transformation combinators.
#
# IMPORTANT: like graph-combinators.nix, this file NEVER imports graph.nix.
# tvix-eval breaks closures captured across 3+ import levels, so graph ops are
# passed in as the `graph` parameter. A transform package calls:
#
#   let gt = import /jc/src/graph-transform.nix { graph = import /jc/src/graph.nix {}; };
#   in gt.windowByLabel { inherit graph; namespace = "prod"; }
#
{ graph }:
let
  # … filter / groupBy / contract / relabel / density helpers …
in {
  filterNodes   = pred: g: /* … */;
  groupByLabel  = { g, labelKey }: /* … */;
  contract      = { g, groups }: /* … */;   # collapse each group to one node
  density       = group: /* edges / possible-edges within group */;
}
```

### 2.5 `crates/graph-api/src/server.rs` — three routes

Three new endpoints, each justified by a gap in the existing surface:

| Endpoint               | Why no existing endpoint fits                                                                               |
|------------------------|-------------------------------------------------------------------------------------------------------------|
| `GET /runs`            | The `/progress` stream reports transient reload stages, not archived run records. `/node/*` surfaces nodes, not the run log. `/importers` lists importers+transforms but not their history. A `/runs` endpoint is the run-log query surface. |
| `GET /runs/:id/diff`   | `/graph/schema` and `/node/*id` describe current topology. No existing endpoint surfaces a historical per-node diff between two snapshots. The diff is per-run by design — tying it to a run id rather than revision-pair avoids the caller reconstructing which revisions correspond. |
| `POST /runs/trigger`   | `POST /generate` is the tvix expression eval path but is importer-scoped — it produces a fresh graph from scratch, not from the current importer graph. A transform needs the current graph injected as input plus the run-log side-effects (ledger append, snapshot swap, provenance update). |

Add to `api_routes()` (server.rs 54‑121), next to the `/graph/*` block:

```rust
        .route("/runs", get(runs_list))
        .route("/runs/:id/diff", get(run_diff))
        .route("/runs/:id/topos-state", get(run_topos_state))
        .route("/runs/trigger", axum::routing::post(runs_trigger))
```

Handlers (new, in `server.rs`):

- `runs_list(selection: SourceSelection) -> Json<Vec<Run>>` — newest-first from
  `state.inner.run_ledger.list()`. Read handler ⇒ `SourceSelection`.
- `run_diff(selection, Path(id): Path<u64>) -> impl IntoResponse` — returns the
  run's `RunDelta` plus input/output revisions, `404` if unknown. Sets
  `x-graph-revision` to `output_revision` via `insert_graph_revision` so the UI
  can gate the diff against the current canvas.
- `run_topos_state(selection, Path(id): Path<u64>) -> impl IntoResponse` —
  returns the evaluated `topos_state` JSON blob for a succeeded run, or `404`
  if the run is unknown / no topos was declared. Read handler ⇒
  `SourceSelection`.
- `runs_trigger(_: DefaultSource, Json(req): Json<TriggerReq>)` — write/compute
  handler ⇒ `DefaultSource` (rejects non-default sources with 400, matching
  `generate_post`). Looks up the transform package, resolves variables,
  injects the current snapshot graph, runs `Transform::apply` on a blocking
  thread, publishes the output snapshot (new `ArcSwap` swap, revision bumped),
  records the run, and returns `200` with the `RunId` — or `422` on
  `TransformError::CapExceeded` (§7).

`TriggerReq { transform_id: String, variables: BTreeMap<String, String> }`.

### 2.6 `crates/graph-api/src/state.rs` — `RunLedger` in `AppStateInner`

Add a field to `AppStateInner` (state.rs 218), **separate from** the
`ArcSwap<GraphSnapshot>` so a run's write path never blocks snapshot readers:

```rust
    /// Chronological transform-run log. Bounded ring (default 256) persisted
    /// as JSONL under `JUMP_CANNON_IMPORTER_PACKAGES_DIR`. Independent of
    /// `snapshot`: appending a run never contends with graph readers.
    pub run_ledger: Arc<RunLedger>,
```

Thread it through both constructors (`new`, `new_with_importer_catalog`,
state.rs 253/275): open the ledger at
`{packages_dir}/runs.jsonl` (resolved from `JUMP_CANNON_IMPORTER_PACKAGES_DIR`,
falling back to `{vault_root}/.jump-cannon/runs.jsonl`) with capacity
`RunLedger::DEFAULT_CAPACITY`, overridable via `JC_RUN_LEDGER_CAP`.

Publishing an output snapshot reuses the exact `GraphSnapshot::build` +
`snapshot.store` path the watcher already uses, so revision bumping, derived
caches, and the `x-graph-revision` header are all unchanged.

### 2.7 `app/ui/src/panels/runs.rs` — new Runs panel (Dioxus)

New module following the existing panel conventions (`branches.rs`,
`gpu_sessions.rs`):

- `use panel_kit::loading::{loading_store, LoadingGate};`
- `let store = loading_store("runs", "loading runs…");` — `begin()` on first
  fetch, `succeed()`/`fail()` on resolution; the workspace `GlobalLoadingBar`
  aggregates this pending state automatically.
- `use_future` polls `GET /runs`; after `Ready`, refreshes update silently (no
  gate flash per poll), matching `branches.rs`.
- Timeline of runs newest-first: each row shows transform display name, bound
  variables, status badge, and `added/removed/changed` counts from `RunDelta`.
- Upcoming scheduled runs animate a countdown; the animation respects
  `prefers-reduced-motion` (read via the same media-query helper the Timeline
  panel already uses) — reduced-motion falls back to a static "next run in m:ss"
  label. Progress during a run uses a measured `ProgressBar` (fraction = nodes
  processed / total) and an explicit indeterminate spinner only when the
  transform cannot report progress (never a fabricated percentage).
- Failed runs render a **red error badge** showing `RunStatus::Failed.reason`
  (§8). A `POST /runs/trigger` button exists for `Manual` transforms.
- Styles live under a `/* runs panel */` anchor block in `app/ui/assets/app.css`.
- **Topos switch on run completion.** When a run succeeds and its
  `TransformDescriptor.topos_id` is `Some(id)`, the Runs panel fetches
  `GET /runs/:id/topos-state`, looks up `id` in the app's `ToposRegistry`
  (§10), and calls `workspace.switch_spec(&entry.spec)`. The switch:
  - Persists the current workspace layout under the old spec's key.
  - Activates the target spec's panel registry (hiding panels not in it,
    revealing topos-specific panels).
  - Applies the spec's default layout (or restores a previously-saved layout
    for this spec id).
  - Injects the `ToposState` from `entry.topos_state` into the app's
    `Ctx.topos` signal, which `panel_body()` and `graph_canvas.rs` read to
    drive visual encoding (node color/size, force params).
  - Panel-kit handles the 300 ms crossfade animation internally;
    `prefers-reduced-motion` skips it.
  - If `topos_id` is `None`, the app stays in the default graph-exploration
    topos with no state change — the graph canvas just patches its buffers
    normally per §4.

### 2.8 `app/ui/src/panels/inspector.rs` — provenance display

Add a "Provenance" section rendering the selected node's `NodeMeta::provenance`:
`first_seen_run` / `last_seen_run` as clickable `RunId` chips (open the Runs
panel filtered to that run), `changed_in` as a count with expand, and
`first_seen_at` / `last_seen_at` as `m:ss`-formatted timestamps. Hidden entirely
when `provenance == Provenance::default()` (importer-only node, never
transformed).

### 2.9 `app/ui/src/main.rs` — `Panel::Runs`

- Add `Runs` to the `Panel` enum (main.rs 172, after `GpuSessions`/`Importers`).
- Add `Panel::Runs => "Runs"` to the display-name match (main.rs 203‑221).
- Add `Panel::Runs` to the tray catalog (`app/ui/src/workspace/catalog.rs` 32).
- Dispatch `Panel::Runs` to `panels::runs::Runs {}` in the panel render match.
- `pub mod runs;` in `app/ui/src/panels/mod.rs` (after `importers`).
- Optionally a `Section::Runs` → `Panel::Runs` entry in `palette.rs` for the
  command palette.

### 2.10 `charts/jump-cannon/knowledge/` — knowledge map (see §9)

---

## 3. Scheduling model (RunPlan ⟶ WatchPlan tick loop)

The pruning evaluation flagged "a new scheduler" as unnecessary complexity. There
is none. `RunPlan` is declarative; the *driver* is the existing watcher:

1. At startup, `graph-api` enumerates transform packages (role = transform) and,
   for each with a non-`Manual` `RunPlan`, registers a driver through the same
   code path `watcher.rs` uses for `WatchPlan::Poll` (`spawn_poll`,
   watcher.rs 272‑278). `Every(d)` ⇒ `interval_ms = d.as_millis()`;
   `Daily(tod)` ⇒ a 60 s interval whose tick fires a run only on the first
   minute the local clock crosses `tod` that day.
2. Each tick that decides to run calls the same internal
   `trigger_run(transform_id, defaults)` the `/runs/trigger` handler uses.
3. The `RescanGate` already in `watcher.rs` enforces single-flight: a scheduled
   tick that races an in-flight run is dropped (`RescanDecision::SkipInFlight`),
   never queued — a transform slower than its cadence cannot accumulate backlog.
4. `Manual` transforms register no driver; they run only via
   `POST /runs/trigger`.

This reuses the tick loop, the single-flight gate, and the `ProgressLog` wiring
with zero new timer infrastructure.

---

## 4. Incremental GPU buffer patching

The admission check already exists and is reused verbatim: the revision guard in
`app/ui/src/graph_canvas.rs::load` (graph_canvas.rs 53‑106) refuses to mount
buffers whose `x-graph-revision` disagrees with `init.graph_revision`. A run's
output snapshot bumps that revision, so the UI only ever patches buffers that
belong to one consistent topology.

Per-run patching (new path in `graph_canvas.rs`, alongside `load`):

1. When a run completes, the Runs panel (or the boot poll) fetches
   `GET /runs/:id/diff` → `RunDelta` + `output_revision`.
2. The canvas compares `output_revision` against its mounted `graph_revision`
   (the `GraphData::graph_revision` field, graph_canvas.rs 25). If they match
   the *previous* revision and the delta's `added`/`removed` are both empty
   (pure `changed`), it takes the **patch path**; otherwise it falls back to the
   full `load()` rebuild (topology changed ⇒ CSR/edge buffers must be rebuilt).
3. **Patch path:** map each `changed` `NodeId` to its buffer index via the
   existing `GraphData::id_to_idx` (graph_canvas.rs 32), then write only those
   contiguous-where-possible ranges into the live GPU buffers
   (`render::Scene::{positions, colors, sizes}`) through a new
   `render::patch_nodes(&[(u32, NodeAttrs)])` that issues `queue.write_buffer`
   on the sub-ranges — **no full `mount_canvas` rebuild, no re-seed, no
   re-upload of untouched nodes.** The GPU force sim keeps running across the
   patch; only the changed nodes' colors/sizes (and any server-supplied content
   fields) are overwritten, so the layout does not reset.
4. Changed-node indices are coalesced into ascending runs before upload to
   minimize `write_buffer` calls (one call per contiguous index run).

This is the D4/D5 improvement: deltas drive a surgical buffer patch, not a
full-graph re-fetch, keeping large graphs (≫100k nodes) at interactive frame
rates during a transform.

### Reduced-motion variant

The animated DAG transitions (countdown to next scheduled run, node
appearance/disappearance/changed tweening into the patched buffer) respect
`prefers-reduced-motion`. The Runs panel reads the media query via the same
helper the Timeline panel already uses (`panel_kit::prefers_reduced_motion`).
When `prefers-reduced-motion` is active:

- Countdown animation → static "next run in m:ss" label (as described in §2.7).
- Node appearance → snap-to-final position in one frame, no opacity/movement
  tween. The buffer is written identically; the animation function is a no-op.
- Node disappearance → instant removal from the buffer, no fade-out.
- Node change → instant write with a brief (~200 ms) highlight color flash
  marking the changed nodes, falling back to their final color with zero
  interpolation.

The non-animated equivalent conveys the **same state** — the changed/delta is
visible via the instant highlight (no motion sickness), and the Runs panel
timeline lists every change explicitly. A user navigating via keyboard/AT gets
the same `aria-live` announcements in both modes.

---

## 5. Nix closure workaround (explicit)

tvix-eval breaks closures captured across **3 or more** levels of file imports
(documented at the top of `graph-combinators.nix`). Therefore:

> **Transforms receive `graph` as an explicit function parameter and NEVER
> `import graph.nix` directly.** `graph-transform.nix` is declared `{ graph }:`
> and a transform package passes the graph library in at the single top import
> level: `import /jc/src/graph-transform.nix { graph = import /jc/src/graph.nix {}; }`.

The injected *data* graph (`input = "graph"`) is likewise passed as the `graph`
binding in the rendered expression `( <expr> ) { graph = <literal>; … }`, so the
transform expression is a plain function of `graph` + its declared variables and
never reaches a deep import chain. This keeps every transform within the 2-level
import ceiling tvix can evaluate correctly.

---

## 6. Non‑omp worked example — Kubernetes cluster re‑clustering

A deliberately non-agent domain, demonstrating the full path end to end.

**Input graph** (imported by any existing importer, e.g. a json-engine package
reading a cluster dump):
- Nodes: pods, deployments, and nodes. `meta.frontmatter` carries
  `namespace`, `labels`, `kind`.
- Edges: owner-references (deployment → replicaset → pod) and scheduling edges
  (pod → node).

**Transform package** `windowed-cluster` — a tvix package,
`role = "transform"`, `input = "graph"`, expression built on
`graph-transform.nix`:

```toml
# windowed-cluster.toml  (format_version = 3, engine = "tvix")
[metadata]
id = "windowed-cluster"
name = "Windowed Cluster"
[parser]
engine = "tvix"
role = "transform"
schedule = "every:1h"
expr = '''
  { graph, window_minutes, density_threshold }:
  let gt = import /jc/src/graph-transform.nix {
             graph = import /jc/src/graph.nix {};
           };
      groups = gt.groupByLabel { g = graph; labelKey = "namespace+app"; };
      hot = builtins.filter
              (grp: gt.density grp >= density_threshold) groups;
  in gt.contract { g = graph; groups = hot; }
'''
[[parser.variables]]
name = "graph"
type = "graph"            # injected; no default, no bounds
[[parser.variables]]
name = "window_minutes"
type = "integer"
default = "60"
min = 1
max = 1440
[[parser.variables]]
name = "density_threshold"
type = "number"
default = "0.7"
min = 0.0
max = 1.0
```

**Binding:** `window_minutes = 60`, `density_threshold = 0.7`.
**Schedule:** `Every(Duration::from_secs(3600))` (`schedule = "every:1h"`),
driven by the reused poll tick (§3).

**Result:** each hour the transform groups nodes by `namespace + app` label,
computes intra-group edge density, and **contracts** every group whose density
≥ 0.7 into a single hot-spot cluster node, re-emitting a consolidated graph. The
`RunDelta` reports the collapsed pod ids as `removed`, the new cluster nodes as
`added`, and surviving boundary nodes whose edges were rewired as `changed`.
Because topology changed (`added`/`removed` non-empty), the canvas takes the
full-rebuild branch of §4; a pure metadata refresh (e.g. density recomputed with
no contraction) would take the surgical patch path instead. The Runs panel
timeline shows one entry per hour; the Inspector's provenance section shows each
cluster node's `first_seen_run` and the pods' `last_seen_run`.

---

## 7. Cap handling

Caps mirror the tvix engine's existing limits: **1,000,000 nodes / 4,000,000
edges**, overridable with `JC_MAX_NODES` / `JC_MAX_EDGES` (read once at startup,
same pattern as other `JC_*` env overrides).

- `Transform::apply` checks the projected graph against the resolved caps inside
  `convert_generated_graph`'s projection and returns
  `TransformError::CapExceeded { detail: "1,000,000 nodes exceeded (got 1,048,576)" }`.
- `runs_trigger` maps `CapExceeded` to **HTTP 422** (not 500 — the input was
  valid, the output is too large), records the run as
  `RunStatus::Failed { reason }` with the cap detail, and **does not** swap the
  snapshot (the previous graph stays live).
- The Runs panel shows that entry with a **red error badge** carrying the cap
  reason. No crash, no silent truncation (CK‑009 / the proposal's stated risk).

---

## 8. Empty / failure UX

When a run fails (eval error, projection error, cap exceeded) or yields an empty
graph:

- **Canvas unchanged.** The snapshot `ArcSwap` is only stored on success, so the
  canvas keeps rendering the previous graph state. The revision never advances on
  failure, so the UI's revision guard (§4) simply never admits a new buffer —
  there is nothing to retract.
- **Ledger entry marked failed.** The run is recorded `RunStatus::Failed {
  reason }` (or, for an empty-but-valid output, `Succeeded` with an all-`removed`
  `RunDelta` and a visible "0 nodes" count).
- **Red badge.** The Runs panel renders the failed entry with a red error badge
  showing `reason` verbatim; the `loading_store` resolves via `fail()` so the
  `GlobalLoadingBar` clears rather than spinning forever.
- An empty *successful* output is still published (a transform may legitimately
  clear the graph), but the panel surfaces a "0 nodes" warning chip so it is
  never mistaken for a hang.

---

## 9. Knowledge map update

Create `charts/jump-cannon/knowledge/Transformation Runs.md` as a new page in the
knowledge map, and wire it in:

- **New page** `Transformation Runs.md` covering: the Transform package shape
  (`role = "transform"`, `input = "graph"`), the `Run`/`RunLedger` model,
  `RunPlan` scheduling via the reused watcher tick, the three `/runs` endpoints,
  the Runs panel, provenance on nodes, incremental GPU patching, cap handling,
  and the Nix closure workaround.
- **Cross-links** added to the adjacent existing pages:
  - `Import and Generate.md` — a "Transforms vs. sources" note pointing here.
  - `Importer Runtime.md` — note that transforms reuse the tvix engine +
    package format (no new engine/crate).
  - `Backend API.md` — add `GET /runs`, `GET /runs/:id/diff`,
    `POST /runs/trigger` to the endpoint list.
  - `Frontend.md` — add the Runs panel to the panel inventory.
  - `Start Here.md` and `Maintaining This Map.md` — register the new page per
    the map's own maintenance convention.

---

## 10. Domain-specific topos — importer-package-owned UI regime

A **topos** is the complete UI/UX binding for one graph domain: which panels are
visible, how node/edge fields map to visual channels (color, size, opacity),
force-sim tuning, and the default workspace layout. It is declared in the
importer package that defines the data schema — the same TOML that already
declares `[schema.fields]` and `[parser]`. A topos is NOT a panel-kit abstraction;
panel-kit provides only the workspace shell that *consumes* a `ToposState` blob.

### 10.1 Model

Every importer package already owns the graph schema (§2.3). The topos extends
that ownership to the visual contract:

```
importer package TOML
├── [schema.fields]      — what fields exist on nodes (already implemented)
├── [parser]             — tvix/pest/json engine config (already implemented)
└── [topos]              — how those fields map to the screen (new)
    ├── id, display_name
    ├── visual.node_color, node_size, edge_width  — field → visual channel
    ├── force             — repulsion_strength, cluster_by, gravity
    ├── panels            — active/available panel names
    └── layout.default    — panel-kit SavedLayoutV2 JSON
```

The app ships with all panel implementations pre-compiled (Rust + Dioxus, no
runtime code loading). A topos selects *which subset* is active and supplies the
per-domain visual encoding. New panel implementations are Rust changes; new
topos that remix existing panels are pure TOML configuration.

### 10.2 Declaration (package TOML)

```toml
[topos]
id = "agentic-knowledge"
display_name = "Agentic Knowledge Graph"

[topos.visual.node_color]
field = "status"
mapping = { active = "#4CAF50", idle = "#9E9E9E", error = "#F44336" }

[topos.visual.node_size]
field = "depth"
range = [4.0, 24.0]

[topos.visual.edge_width]
field = "call_count"
range = [0.5, 4.0]

[topos.force]
repulsion_strength = 0.3
cluster_by = "parent_agent"
gravity = 0.05

[topos.panels]
default = ["Graph", "AgentStatus", "RunLog", "Inspector"]
available = ["Graph", "Nodes", "AgentStatus", "RunLog", "Inspector", "Settings"]

[topos.layout.default]
# inline panel-kit SavedLayoutV2 JSON
```

When `[topos]` is absent, the graph-exploration topos is the implicit default:
all standard panels, categorical color palette, no force-param overrides.

### 10.3 tvix-computed topos state

The visual encoding is not always static. A topos may need to *compute* its
panel-kit configuration from the graph — e.g., agent counts for a status panel,
or field-distribution statistics for a color ramp. This is done in Nix, in the
same expression that produces the graph:

```nix
{ graph, window_minutes, density_threshold }:
let
  clustered = clusterByWindow graph window_minutes density_threshold;
in {
  graph = clustered;
  toposState = {
    panels = {
      agentStatus = {
        active = countByField clustered.nodes "status" "active";
        idle = countByField clustered.nodes "status" "idle";
        error = countByField clustered.nodes "status" "error";
      };
    };
    visual = {
      nodeColor = mapField "status" {
        active = "#4CAF50";
        idle = "#9E9E9E";
        error = "#F44336";
      };
    };
    force = {
      repulsionStrength = 0.3;
    };
  };
}
```

The `toposState` output is the runtime panel-kit configuration — a JSON blob
the app reads to drive visual encoding, panel state, and force-sim parameters.
It is served at `GET /runs/:id/topos-state` (§2.5) and injected into the app's
`Ctx.topos_state` signal on topos switch (§2.7).

### 10.4 App-side: ToposRegistry + topos switch

New module `app/ui/src/workspace/topos.rs`:

```rust
use panel_kit::WorkspaceSpec;

/// Registry of known topos ids → workspace spec + visual encoding.
/// Populated at boot from:
/// 1. Built-in topos (the default graph-exploration topos, always present).
/// 2. Importer packages that declare `[topos]` (parsed from GET /importers).
/// 3. The active transform's output topos_state (applied on run completion).
pub struct ToposRegistry {
    entries: HashMap<String, ToposEntry>,
}

/// One entry in the registry. The `spec` drives panel-kit's `switch_spec()`;
/// the `topos_state` drives graph visual encoding (field→color/size mappings,
/// force-sim params, per-panel data blobs).
pub struct ToposEntry {
    pub spec: WorkspaceSpec<Panel>,
    pub topos_state: ToposState,
}

/// Live topos state driving visual encoding + panel data. Injected into Ctx
/// by the topos-switch path in runs.rs (§2.7). Read by panel_body(), the
/// graph canvas, and any topos-aware panel.
pub struct ToposState {
    pub topos_id: String,
    pub visual: VisualEncoding,
    pub force: ForceParams,
    pub panel_data: serde_json::Value,  // per-panel opaque blobs
}

pub struct VisualEncoding {
    pub node_color: ColorMapping,
    pub node_size: SizeMapping,
    pub edge_width: SizeMapping,
}

pub enum ColorMapping {
    Categorical(HashMap<String, String>),  // field_value → #rrggbb
    PaletteDefault,
}

pub struct SizeMapping {
    pub field: String,
    pub min: f32,
    pub max: f32,
}

pub struct ForceParams {
    pub repulsion_strength: f32,
    pub cluster_by: Option<String>,
    pub gravity: f32,
}
```

`Ctx` gains a new signal:

```rust
    pub topos: Signal<Option<ToposState>>,
```

`graph_canvas.rs::load` reads `ctx.topos` to override the default categorical
palette and force params. `panel_body()` reads it to gate panel visibility
(hiding panels not in the active spec's registry). `workspace.switch_spec(&entry.spec)`
handles layout persistence keyed by the spec's id; `Ctx.topos` is set to
`entry.topos_state` after the switch.

### 10.5 Panel-kit surface (cross-repo) — upstream `switch_spec`

A topos is, structurally, a named `WorkspaceSpec<K>` — the immutable app
definition panel-kit already models (panel registry, default layout, theme,
chrome, persistence policy). The topos concept maps 1:1 onto the existing
`WorkspaceSpec` type; no new panel-kit type is needed.

What panel-kit *does* need is the ability to switch specs at runtime:

```rust
// In panel-kit (crates/panel-kit-core or the web crate, TBD by libraryness
// implementation):

impl Workspace<K: PanelKind> {
    /// Activate a different workspace spec, persisting the current layout
    /// under the old spec's key and applying the new spec's panel set +
    /// default layout (or a previously-saved layout for `spec.id`).
    ///
    /// - Saves the current layout keyed by `{storage_root}/{old_spec_id}`.
    /// - Hides panels not in the new spec's registry, reveals spec panels.
    /// - Applies the spec's default layout, or restores a saved layout for
    ///   `{storage_root}/{new_spec_id}` if one exists.
    /// - Animates the transition over 300 ms; `prefers-reduced-motion`
    ///   skips the animation.
    pub fn switch_spec(&mut self, spec: &WorkspaceSpec<K>);
}
```

This is the only API addition in panel-kit. It is generic: any consumer with
multiple workspace specs (e.g., an IDE switching between "editor" and "debug"
modes) can use it. Panel-kit does not know about topos, graphs, transforms,
or visual encoding — it knows about workspace specs and layout persistence.

The layout persistence layer (already `LayoutStore` in core) gains internal
key namespacing: `{storage_root}/{spec_id}` instead of a single flat key, so
each spec's saved layout is independent. Existing single-spec consumers see
no change; the `storage_root` is their existing `storage_key`.

What stays in jump-cannon:

- **`ToposRegistry`** (`app/ui/src/workspace/topos.rs`) — populated at boot
  from built-in topos + importer packages that declare `[topos]`. Maps
  `topos_id: String → WorkspaceSpec<Panel> + ToposState` (the spec drives
  panel-kit; the state drives graph visual encoding).
- **`ToposState`, `VisualEncoding`, `ColorMapping`, `SizeMapping`,
  `ForceParams`** — graph-domain types for field→color/size mappings and
  force-sim tuning. Injected into `Ctx.topos: Signal<Option<ToposState>>`
  on topos switch.
- **Topos-switch wiring in `runs.rs`** (§2.7): on run completion with
  `topos_id = Some(id)`, fetch `GET /runs/:id/topos-state`, look up the
  `WorkspaceSpec<Panel>` from `ToposRegistry`, call
  `workspace.switch_spec(&spec)`, inject the `ToposState` into `Ctx`.

The `ToposEntry` struct from the original design is deleted — it was just a
`WorkspaceSpec<Panel>` plus `ToposState` under another name. The registry
holds both directly.

### 10.6 Worked example: agentic-knowledge topos

The omp-auto-loop transform's importer package declares `[topos] id =
"agentic-knowledge"`. Its tvix expression computes `toposState` with live
agent-status counts. When the transform runs and succeeds:

1. The Runs panel fetches `GET /runs/42/topos-state` → the computed
   `ToposState` JSON.
2. It looks up `"agentic-knowledge"` in `ToposRegistry`, gets the
   `ToposEntry { spec, topos_state }`.
3. It calls `workspace.switch_spec(&entry.spec)` — panel-kit switches
   the panel set and layout.
4. The default Graph + Nodes + Inspector + Settings panel set is replaced
   by Graph + AgentStatus + RunLog + Inspector + Settings.
5. The AgentStatus panel reads `toposState.panels.agentStatus` and renders
   active/idle/error counts.
6. The graph canvas reads `toposState.visual` and colors nodes by `status`
   field (green active, grey idle, red error) instead of the categorical
   palette.
6. Force-sim uses `repulsionStrength = 0.3` and clusters by `parent_agent`.

When the user switches back to a non-agentic source (e.g., selecting the K8s
importer), the topos reverts to the default graph-exploration regime.

### 10.7 Non-transform topos activation

A topos does not require a transform. Any importer package that declares
`[topos]` activates its topos when the user selects that importer as the active
source. The importer's tvix expression can also output a `toposState` alongside
`graph`, served at the existing `GET /init` response as a new optional field.
This means a static importer (Obsidian vault, K8s cluster) can ship a topos
with no transform involved — the topos is a property of the package, not of
the run system.

| Pruning gap | Addressed in |
|-------------|--------------|
| CK‑010 concrete `Run` struct / score_4 anchor | §1.4 (`Run`, `RunDelta`, `RunStatus`) |
| CK‑011 Nix closure workaround | §2.4, §5 (`graph-transform.nix { graph }`) |
| CK‑012 concrete file paths & symbols | §2 (every file + symbol named) |
| CK‑013 scheduling model (RunPlan, no new scheduler) | §1.5, §3 (RunPlan ⟶ WatchPlan tick) |
| D1 non-omp worked example | §6 (Kubernetes re-clustering) |
| D4 incremental GPU buffer patching | §4 (`render::patch_nodes`, delta-driven) |
| D5 node-content provenance on `NodeMeta` | §2.1 (`Provenance` + `content_hash`) |
| D6 knowledge map update | §9 (`Transformation Runs.md`) |
| Cap handling (CK‑009) | §7 (422, red badge, no truncation) |
| Empty/failure UX | §8 (previous state retained, red badge) |
| No new crate / no new engine | §0, §2.3 (reuses tvix engine + package format) |
| No omp/agent/session vocabulary in core types | §0 (domain nouns only) |
