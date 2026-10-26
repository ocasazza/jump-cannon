# jump-cannon: transformation runs, live timeline, richer nodes — proposals (set A)

Phase 1 of Tree-of-Thoughts. Propose only. Grounded in a read of the files
named in the brief; every "today" claim below cites the file it came from.

## 1. Problem decomposition

**Core problem.** jump-cannon can *generate* a graph from a parameterised Nix
expression once (`crates/importer/src/tvix.rs`: `( <expr> ) { <bindings> }` →
`tvix_wasm::eval_graph` → `convert_generated_graph`). It cannot (a) express a
graph → graph **transformation** as a package, (b) **run** it repeatedly and
remember each run, (c) show the run sequence as an **animated timeline** on the
canvas, (d) show **upcoming/past** runs, or (e) show much about a node when it
is clicked.

**What exists (the bones).**
- Nix library: `graph.nix` (attrset graph, `addNode/addEdge/removeNode/merge`,
  `toGraphJSON`), `graph-combinators.nix` (`pathGen`… generators,
  `subdivideRule/hubRule/contractRule` rewrite rules, `composeOps`,
  `parallelMerge`). Rewrite rules are *already* `graph -> graph`; nothing hosts
  them. The `{ graph }` parameter-passing convention exists because of the tvix
  3-level-import closure bug (header of `graph-combinators.nix`).
- Engine: format_version=3 packages; the tvix engine binds typed variables at
  apply time; its schema is fixed and a `[schema]` body is rejected
  (`validate_package_schema`); the projection **drops node `metadata`**
  (`frontmatter: HashMap::new()`, `mtime: 0`, `content_readable: false`).
- Server: `ArcSwap<GraphSnapshot>` with a fresh `revision` per rebuild
  (`state.rs`), `WatchPlan::{Static, Filesystem, Poll{interval_ms}, Push}`
  (`data-loader`), `/progress?since=` stage log, `/node/*id` protobuf
  `NodeMeta` (title/path/folder/doctype/tags/frontmatter_json/metrics/body),
  `POST /generate` native eval. `graph-vcs` already models `Commit { ops:
  Vec<GraphOp>, timestamp_ms, message, parents }` and `Snapshot`.
- UI: wgpu canvas with `set_positions`, `update_colors`, `update_sizes`,
  `set_selected` (dim mask), hover; topology change = full remount
  (`graph_canvas::load`). A **Timeline panel already exists** — it scrubs a
  `FrameRing` of *layout positions*, not graph states. Nodes panel
  `focused_node` shows doctype/title/id/path/source + the document viewer;
  Inspector shows metrics, badge chips, leftover frontmatter grid, neighbours.
- Requester's package: `omp-auto-loop.toml` (pest, N|/E| lines, `content`
  capture → body). It is one instance of "a process emits a graph over time".

**Constraints.** Rust everywhere (Dioxus/wgpu, no JS). Packages not crates —
a new *mechanism* may be a crate, a new *source* may not. Reuse the tvix
package contract rather than a parallel one. Respect `{ graph }` param passing.
Honest progress, reduced-motion variants, one hierarchy per surface (AGENTS.md
UI rules). Wire stays `{nodes, links}` + protobuf `NodeMeta` + LE buffers.

**Subproblems.**
1. Transformation contract: what is the input, the output, the parameters.
2. Run model: identity, trigger, cadence, what is recorded, retention.
3. Timeline semantics: sequence of snapshots? diffs? one graph with time?
4. Schedule: where "upcoming" comes from when no scheduler exists.
5. Animation: what the canvas can do without a remount; what needs one.
6. Node content: a generic property/provenance model every source fills.

**Success criteria.** A user with *any* source (Obsidian vault, Kubernetes,
httpjson, pest lines, tvix generator) can: install one transform package; see
it run on a cadence; scrub past runs and see what changed; see the next run;
click any node and learn its properties, provenance, and change history —
with no source-specific Rust.

## 2. Solution space

| Dimension | Options |
|---|---|
| Where the transform runs | graph-api native (`spawn_blocking` like `TvixImporter::import`) · browser worker (`worker::eval_in_worker`, as Generate does) · both (same expr, `POST /generate`-style fallback) |
| Transform input binding | inject current graph as a Nix binding (like `eval_seed` injects `n`) · inject only a lazy view (ids + accessor builtins) · run over an op-log instead of a graph |
| Run persistence | none (in-memory ring) · graph-vcs commits · append-only journal file · reified into the graph itself |
| Schedule origin | package `WatchPlan::Poll` cadence · catalog/overlay `schedule` field · external scheduler + `Push` · deterministic "future is computable" |
| Timeline unit | snapshot revision · `Vec<GraphOp>` diff · time-valid intervals on nodes/edges |
| Animation | recolour/dim/resize in place (no remount) · remount per state · shader-side interpolation |
| Node enrichment | project Nix `metadata` → frontmatter · schema-driven "facts" section · per-node change log · provenance block |

Trade-off axes: generality vs. specificity; server load (a rebuild does metrics
+ Tantivy + facets per swap) vs. fidelity; persistence durability vs. bounded
memory; in-place animation (fast, same node set) vs. topology change (remount).

## 3. Approaches

### A1. Transform packages + run journal in graph-api (probability 0.85, complexity med)

**Summary.** A tvix package gains a `role = "transform"`: its `expr` is a
function of `{ graph, <variables> }` returning a graph; graph-api applies it as
a pipeline stage after the source import, on the source's existing `WatchPlan`
cadence, and records each application in a bounded in-memory run journal
served at `/runs`.

The package shape stays format_version=3 and `engine = "tvix"`; the only new
field is `[parser] role = "transform"` (default `"generate"`). Validation is
the same `validate_config`; `render_expression` renders
`( <expr> ) { graph = <injected>; name = <literal>; … }`. A new
`tvix_wasm::eval_transform(expr, input_json)` injects the current snapshot
(`to_graph_json` of the `VaultGraph`, metadata included) as a top-level
`let graph = builtins.fromJSON ''…''` binding — the same host-injection idiom
`eval_seed` uses for `n`, so no new closure levels. A `graph-transform.nix`
library (importable at `/jc/src/`, taking `{ graph }` like the combinators)
supplies the array-style vocabulary: `mapNodes`, `filterNodes`, `foldNodes`,
`mapEdges`, `groupBy`, `collapse`, `label`, all as thin wrappers over
`builtins.map/filter/foldl'` on `attrValues`. Laziness is Nix's own: a
`filterNodes` result is a thunk until `toGraphJSON` forces it.

The Rust side is a `crates/importer` type, not a crate: `GraphTransform {
package: ValidatedPackage, variables, source_id }` implementing a new
`data_loader::Transform` trait (`apply(&VaultGraph, &dyn ImportProgress) ->
Result<VaultGraph>`), composed by `ImportPipeline` after the importer. A
catalog source definition gains `transforms: [ { package, variables } ]`
(same overlay files `catalog.local.json` / `variables.local.json`). Each
`rebuild_snapshot` records `RunRecord { id, source_id, transform_id, trigger:
Watch|Poll|Manual, started_ms, finished_ms, revision_before, revision_after,
added/removed/changed node & edge counts, error }` into a ring of N (default
256) alongside the last K (default 8) full snapshots for scrubbing.
`GET /runs` returns past runs **and** `upcoming: [{ due_ms, transform_id }]`
derived from the `Poll` interval / filesystem rescan interval; `POST
/runs` triggers a manual run. `/runs/:id/diff` returns two LE `u32` index
buffers (added, removed) against `/graph/ids` of `revision_after`.

**Timeline UI.** A new Runs panel (past strip · now marker · upcoming strip,
honest countdown from `due_ms`) with a scrub over retained snapshots. Scrubbing
to a run whose node set matches the live one animates in place: added nodes
pulse via `update_colors` + `update_sizes`, removed nodes dim via
`set_selected`; a differing node set remounts through `graph_canvas::load`
against `/graph/*?revision=`. `prefers-reduced-motion` swaps the pulse for a
static highlight.

**Node content.** (1) `convert_generated_graph` projects Nix `metadata` into
`frontmatter` (today discarded) and allows `[schema.fields]` on tvix packages
so metadata keys become searchable/facetable. (2) `NodeMeta` proto gains
`provenance { source_id, transform_id, run_id, first_seen_run, last_changed_run,
change: added|changed|unchanged }` and `facts: [{key, value, field_type}]` —
the schema-declared fields of *whatever* importer, in declared order. (3)
`focused_node` in `nodes.rs` renders Facts, Provenance, and a per-node "changed
in runs …" list above the document viewer. Obsidian, Kubernetes, and pest
nodes all get richer with zero source-specific code.

**Key decisions.** Server-side eval; input injected as JSON; journal
in-memory and bounded; schedule derived from existing `WatchPlan`.
**Trade-offs.** No durability across restart (deliberate: bounded); full
rebuild per run (metrics + Tantivy) caps cadence at tens of seconds for large
graphs; serialising a 1M-node graph to JSON for injection is O(n) allocation
per run. **Risks.** tvix eval of a big injected graph is slow — mitigate with
the caps in `lib.rs` and a per-package `[limits] input_nodes`; the closure bug
may bite `graph-transform.nix` if it imports `graph.nix` — keep the `{ graph }`
parameter convention.

### A2. Runs as graph-vcs commits; timeline is the branch log (probability 0.82, complexity med-high)

**Summary.** Same transform package and Nix library as A1, but every run is
committed to a `graph-vcs` branch (`runs/<source_id>`) via the existing
`Commit { ops: Vec<GraphOp>, timestamp_ms, message }`; the timeline is
`vcs.log`, the diff is `commit.ops`, and the History panel already renders it.

graph-api owns a `MinigrafStore` per source (native; single file under
`VAULT_ROOT/.jump-cannon/runs/<source>.graph`). `rebuild_snapshot` diffs the
old and new `VaultGraph` into `GraphOp`s and commits with message
`transform:<id> trigger:<poll|watch|manual> vars:<…>`. The run journal *is*
the commit log, durable across restarts, and merge/rebase semantics come free
for the multi-source case (two transforms on one source = two branches; a
`merge` commit records conflicts as first-class objects, which is exactly what
the requester's "dream consolidation" wants: consolidation is a merge with
recorded conflicts, not a silent overwrite). `GET /runs` maps to
`log(branch, n)`; `GET /runs/:commit/graph` materialises a `Snapshot` and
serves it through the same `/graph/*` buffers with `x-graph-revision` set to a
commit-derived id, so the canvas needs no new wire.

**Timeline UI.** The Sessions view's History panel gains a "runs" mode; the
User view gets the same Runs panel as A1 but backed by commits. Scrub =
checkout a commit into the canvas (remount, or in-place when node sets
match). Animated DAG: the *commit graph itself* (parents edges) is renderable
as a small secondary graph in the Runs panel using the same wgpu pipeline —
useful when transforms fork/merge.

**Upcoming runs.** From `WatchPlan` as in A1, plus a per-source
`schedule.local.json` overlay (`interval_ms` or a fixed `at: [ISO…]` list) so
an operator can declare future runs without a cron system. A `Scheduled`
progress event is emitted when a run is queued so the Progress panel shows it.

**Node content.** As A1, plus `NodeMeta.history: [{commit_id, op, ts}]` from
walking the log for that node id (bounded to last 32) — the Inspector shows
"this node was added in run 12, relabelled in run 15".

**Trade-offs.** Durable and mergeable, but heavier: a commit per run means
a `Snapshot` clone per run; wasm32 has no `MinigrafStore` file backend (browser
worlds already snapshot to localStorage, so client-only graphs get a bounded
ring instead). **Risks.** Coupling the User view to VCS internals; log walks
for per-node history are O(runs × ops) without an index.

### A3. Instrument the existing rebuild: structured run events on `/progress` (probability 0.80, complexity low)

**Summary.** No new persistence and no VCS: make every snapshot rebuild — by
any importer, transform or not — a first-class *run* in the progress event
log, carrying node-scoped payloads, and let the existing Progress + Timeline
panels and the canvas animate from that stream.

`progress.rs` gains three events: `run_started { run_id, source_id, trigger,
transform_id?, due_ms? }`, `run_touched { run_id, added: [idx], removed:
[idx], changed: [idx] }` (indices against the *new* revision's `/graph/ids`),
and `run_finished { run_id, revision, counts, error? }`. Because `/progress`
is per-source and already polled every ~250 ms by the app, "live" comes free.
The transform itself is A1's package + `eval_transform` stage — this approach
differs only in *what is recorded and where*: nothing beyond the last-N event
window. "Upcoming" is a `run_scheduled { run_id, due_ms }` event emitted by
`spawn_poll` / the filesystem rescan timer one tick ahead, so the UI shows a
real due time and never invents one.

**Timeline UI.** The existing Timeline panel gets a second track: below the
layout-frame scrub, a run track of markers folded from the stream; clicking a
marker replays `run_touched` as a colour pulse (`update_colors`) and a dim of
removed nodes (`set_selected`) — an *animation of the delta*, not a checkout
of the past state (the past state is not retained). The Progress panel's
`TaskRow` fold in `main.rs` groups stages under their run.

**Node content.** `NodeMeta.frontmatter` projection fix + schema-driven Facts
as in A1; per-node history is client-side: the app keeps the last N
`run_touched` events and shows "touched in runs …" for the selected node.

**Trade-offs.** Smallest change; every source benefits immediately (an
Obsidian save burst becomes a visible run). But you cannot scrub *to* a past
graph, only see what a run did; history is lost on reload. **Risks.**
`run_touched` for a 100k-node delta bloats the JSON progress log — cap and
elide beyond 10k indices with an explicit "truncated" flag.

### A4. Time inside the graph: bitemporal nodes and reified runs (probability 0.08, complexity med)

**Summary.** Abandon "sequence of snapshots". A run does not replace the
graph; it *appends* to one graph in which every node and edge carries
`valid_from`/`valid_to` run indices, and the transformation, its runs, and
their effects are themselves nodes and edges (`transform → run → node`
provenance edges, PROV-O style).

The transform stage emits `{ nodes, links }` where unchanged nodes keep
`valid_from`, removed nodes get `valid_to = run`, new nodes get `valid_from =
run`; the Rust stage adds `run:<id>` and `transform:<id>` nodes with `produced`
edges. The timeline is then a **filter** — `valid_from <= t < valid_to` —
evaluated by the existing `/search/matches` set primitive and applied through
`set_selected` with no remount, ever. Scrubbing is instantaneous and the
"animated DAG" is literally the provenance subgraph lighting up as `t`
advances. Upcoming runs are `run` nodes with `status = scheduled` emitted
ahead of time by the scheduler stage.

**Node content.** Falls out for free: clicking a node shows its `valid_from`
/`valid_to` facts, and its neighbours *include* the run and transform that
produced it — the "what's going on" question is answered by the graph.

**Trade-offs.** Radically generic and uses only existing facets/filters, but
the graph grows monotonically (retention = a compaction transform, which is
itself a run), metrics (PageRank, Louvain) are polluted by provenance nodes
unless the metric pass learns to exclude a `provenance` tag, and layout shows
dead nodes unless filtered. **Why low probability.** It changes what "the
graph" means for every consumer (metrics, layout, search) and needs a
compaction story before it is safe on long-running sources.

### A5. Lazy stream evaluator: the timeline is an infinite lazy list in Nix (probability 0.06, complexity high)

**Summary.** A transform package's `expr` returns not a graph but a lazy
**stream** of graphs — `unfold step g0` — and the host forces element *k*;
past, present, and *future* runs are the same thing, so "upcoming runs" are
literally precomputed states when the transform is deterministic.

`graph-transform.nix` provides `unfold : (g -> g) -> g -> stream` (a Y-built
lazy cons list, `{ head; tail; }` thunks) plus `take`, `drop`, `zipWith`, and
`nth`, mirroring the array-like vocabulary the brief asks for. Rust gains
`tvix_wasm::eval_stream(expr, k, fuel)` which wraps the expression as `nth k
(<expr> { … })` so only the *k*-th state is forced; states 0..k are memoised
server-side in a bounded LRU. A "run" is `(stream_id, k, wall_clock)`; the
scheduler is a clock that increments *k* on cadence; scrubbing forward past
`now` is allowed and clearly labelled "predicted". Impure inputs (a vault
reload) enter as a *second* stream zipped with the transform, so the design
degrades to A1 semantics when the input is not predictable.

**Trade-offs.** Beautiful fit for the "lazy parameterised iterator" framing;
predicted futures are a genuinely novel UX. But tvix has no fuel/step limit
(`lib.rs` caps only output), so a divergent `step` hangs the blocking thread;
the closure bug is likelier with Y-built thunks across imports; every state is
a full graph, so memory is states × graph. **Why low probability.** Needs an
evaluator feature (fuel / cancellation) tvix does not expose at the pinned
rev, and most real transforms are impure.

### A6. No scheduler: external timers + `Push`, runs applied by a CLI (probability 0.07, complexity low-med)

**Summary.** jump-cannon deliberately does not grow a scheduler. A
`graph-api run <package> --var k=v` subcommand applies a transform package to
the live source and pushes the result through the existing `WatchPlan::Push`
trigger; systemd timers, cron, or a Kubernetes CronJob own cadence; graph-api
*reads* upcoming runs from the scheduler it is told about.

The catalog source definition gains `scheduler: { kind: systemd-timer | cron
| k8s-cronjob, ref }`; `GET /runs` derives `upcoming` from `systemctl
list-timers --output=json`, a cron expression's next fires, or the CronJob's
`.status.nextScheduleTime` via the already-allowlisted Kubernetes client —
never from a guess. Past runs are the CLI's append to `runs.jsonl` next to the
overlay files. The transform contract, Nix library, `eval_transform`, and the
node-content work are exactly A1's; only ownership of *time* moves outside
the process, which is how ops users already run everything else.

**Trade-offs.** Zero in-process timers, restart-safe, and honest about
"upcoming" (it is the real scheduler's answer). But three scheduler adapters
is a maintenance tax, dev on macOS has no systemd, and the requester's
"periodic dream consolidation" wants one-click cadence from the UI, which this
only gets by writing a timer unit. **Why low probability.** Contradicts the
brief's expectation of a UI-driven schedule; best as a *later* adapter on top
of A1's `/runs` contract, not the first design.

## 4. Diversity and overfitting check

| | Transform runs | Run store | Timeline unit | Schedule | Animation |
|---|---|---|---|---|---|
| A1 | server, injected graph | in-memory ring | revision | WatchPlan | recolour/dim, remount on topology change |
| A2 | server | graph-vcs commits | commit | WatchPlan + overlay | checkout + commit DAG |
| A3 | server | none (event window) | progress run | scheduled event | delta replay only |
| A4 | server, appends | the graph itself | validity filter | run nodes | filter, no remount |
| A5 | server, lazy stream | LRU of states | stream index | clock on k | predicted futures |
| A6 | CLI + Push | jsonl | CLI record | external scheduler | as A1 |

A1–A3 are conventional (pipeline stage, VCS, instrumentation); A4–A6 move
where time lives (into the graph, into laziness, out of the process). No two
share both store and timeline unit.

**Overfitting.** The requester's omp loop is a pest source that appends lines;
none of the six special-cases it. What each gives a non-requester:
- An Obsidian user gets "runs" for save bursts (A3 immediately), a periodic
  "cluster and label my notes" transform (A1/A2), and per-note change history.
- A Kubernetes operator gets a `collapse-by-namespace` transform and sees
  every poll as a run with added/removed pods pulsing (A1/A3).
- An httpjson tenant (Hindsight memory bank) gets scheduled consolidation with
  recorded conflicts (A2) and provenance on every memory node.
- A generator user (`generate-clusters.toml`) gets A5's predicted futures for
  free because generators are pure.

**Node-content work is approach-independent** and should land first: project
Nix `metadata` → `frontmatter` (today dropped in `convert_generated_graph`),
allow `[schema.fields]` on tvix packages, add schema-driven `facts` and a
`provenance` block to `NodeMeta`, and render both in `focused_node`. That
alone answers "click a node, see what's going on" for every source, with no
dependency on which run/timeline design wins.

**Judge's pruning hints.** A1 is the minimal complete design; A2 is A1 with
durability and merge semantics at the cost of VCS coupling; A3 is the cheapest
first slice and composes with A1 or A2 (same events, richer store). A4 is
worth stealing one idea from — reified `run`/`transform` nodes as provenance
neighbours — even if bitemporality is rejected.
