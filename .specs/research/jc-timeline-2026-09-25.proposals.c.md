# jump-cannon Transformation Timeline: Six High-Level Approaches (set C)

**Date:** 2026-09-25
**Scope:** Design options only. Nothing here is implemented.
**Subject:** `~/Repositories/ocasazza/jump-cannon` (Rust workspace). Feature lands upstream.

## Source-grounded current state

- **Graph library.** `graph.nix` is attrsets `{ nodes.<id> = {id,type,metadata}; edges.<id> = {id,source,target,directed,metadata}; }` with `addNode/addEdge/removeNode/removeEdge`, queries, `merge`, `fromEdgeList`, and `toGraphJSON` which flattens `metadata` into the node/link records (`graph.nix:16-149`). `random`/`clustered` build in O(n) via one `listToAttrs` (`graph.nix:174-261`). `graph-combinators.nix` takes `{ graph }` as a parameter and inlines `Y` because closures across 3+ import levels break in tvix (`graph-combinators.nix:3-12,25-26`); it already has rewrite rules (`subdivideRule`, `hubRule`, `reverseEdgesRule`, `contractRule`) and `composeOps`/`parallelMerge` (`graph-combinators.nix:162-259`).
- **Evaluator.** `tvix_wasm::eval_graph` wraps the expr in `deepSeq` + `toJSON` (`lib.rs:268-298`), caps output at 1M nodes / 4M edges (`lib.rs:43-44`). `RawNode` ignores extra fields, so **Nix-side node metadata is dropped on the wire** (`lib.rs:233-246`). `eval_seed` shows the host-injects-a-binding pattern (`let n = <n>;`, `lib.rs:402-450`).
- **tvix engine.** A `format_version = 3` package whose `[parser] expr` is a function of typed `[[parser.variables]]`; the engine renders `( <expr> ) { name = <literal>; … }` and evaluates it (`tvix.rs:13-20,354-371`). `convert_generated_graph` projects each node to `NodeMeta { title = id, tags = [type], frontmatter = {}, doctype = "generated", content_readable = false }` (`tvix.rs:512-566`) — the second place node metadata is lost. Watch plan is `Static` (`tvix.rs:629`). Package schema additions are rejected (`tvix.rs:290-307`).
- **pest engine.** Properties become string frontmatter, tags retain order, the optional `content` capture becomes a node body (`pest.rs:3-11,370-373`).
- **graph-api.** One `ArcSwap<GraphSnapshot>` with a revision counter surfaced as `x-graph-revision` (`server.rs:30,42-47`). Change drivers: filesystem debounce, `Poll { interval_ms }`, `Push`, `Static` (`watcher.rs:1-14`, `data-loader/src/lib.rs:960-970`). `/progress?since=` is an append-only stage log (`server.rs:1522-1549`). `/node/*id` returns `proto::NodeMeta` (identity, tags, `frontmatter_json`, metrics, lazy body) (`server.rs:1753-1824`, `graph.proto:19-37`). `POST /generate` evaluates Nix server-side and returns `{nodes, links}` (`server.rs:678-774`). The catalog binds a tvix package + variables per source instance (`importer_catalog.rs:107-118`) and exposes per-request parameter pickers (`importer_catalog.rs:120-136`).
- **graph-vcs.** jj-style `Commit { id, change_id, parents, … }` over `Snapshot { nodes: BTreeMap<NodeId, VaultNode>, edges }` with recorded conflicts (`graph-vcs/src/model.rs:77`, `merge.rs:26`); the History panel already lists commits per branch (`panels/history.rs:1-3`).
- **UI.** `Ctx` bundles `graph`, `graph_session {epoch, graph_revision}`, `selected`, `meta` (`main.rs:670-704`). The canvas is wgpu; the only runtime mutation seams are `mount_canvas`, `set_selected_node`, `set_search_highlights`, and `GraphPipelines::set_positions` (`render/mod.rs:175,512,520`, `render/pipelines.rs:1743`). The existing **Timeline panel scrubs simulation position frames** in a delta-compressed `FrameRing` (`panels/timeline.rs:1-27`) — it is a layout scrubber, not a graph-state timeline. The Nodes focus view renders kind, title, id, path, provenance chips, then the Document viewer; for metadata-only nodes that is nearly empty (`panels/nodes.rs:777-815`). The Inspector renders metrics, badges, frontmatter grid, and neighbours (`panels/inspector.rs:4-21`).
- **Conventions.** Rust everywhere; importers are packages not crates; styling in `app.css`; honest progress via `panel_kit::loading`; `prefers-reduced-motion`; wire = protobuf/raw buffers (`AGENTS.md:5-9,34-78,158-161,194-200`).

## Step 1: Problem decomposition

### Core problem

jump-cannon can *generate* a graph from a parameterised Nix function once, but it cannot *transform an existing graph* with one, cannot *re-apply* that transformation on a cadence, keeps no record of *what each application changed*, and shows a clicked node as almost nothing when the source is not markdown. The feature must add a first-class, package-declared **transformation** that is applied over time, a **run record** that makes each application visible (past, in-flight, upcoming), an **animated view of the graph moving between run states**, and a **generic node detail surface** that is rich for every importer.

### Constraints

1. Reuse `engine = "tvix"` semantics: expr is a function of declared variables; admin binds values at apply time; no parallel mechanism.
2. Nix library style: attrset graphs, `{ graph }` parameter passing, no closure across 3+ import levels, no impure builtins, O(n) construction patterns.
3. Wire stays `{nodes, links}` via `toGraphJSON` / `parse_graph_json`; node shape `id/type/metadata`.
4. Rust everywhere; no new crate per source; packages carry mechanism, the catalog carries binding.
5. Must serve an Obsidian vault user, a Kubernetes-cluster user, an httpjson API user, and a synthetic-generator user as well as the agent-loop requester.
6. Honest progress, reduced-motion variants, and the existing loading-store discipline in the UI.

### Subproblems

- **T1 Transform contract.** What a transformation package receives (graph? variables? time?), what it returns, and how the Rust side names it (the "generic Rust transformation struct").
- **T2 Functional library.** `map/filter/reduce/fold/zip` over nodes and edges, lazy where Nix is lazy, in the `{ graph }`-parameter style.
- **T3 Run identity and record.** What a run is, what is persisted per run (inputs, output revision, diff summary, duration, error), how runs compose into a timeline.
- **T4 Scheduling.** Where "upcoming" comes from in a tool with no scheduler: interval, cron-like, push, or derived.
- **T5 Animation.** Rendering the transition between two graph states on the wgpu canvas with the mutation seams that exist.
- **T6 Node content.** Generic metadata every node must carry so any source is explorable: properties, provenance, run lineage, neighbourhood, body-or-not.
- **T7 Metadata fidelity.** Stop dropping Nix `metadata` at `RawNode` and at `convert_generated_graph`.

### Success criteria

- A transform is one TOML package; binding it to a source and a cadence is catalog/config, not Rust.
- The same transform package applies to a vault graph, a Kubernetes graph, and a generated graph without edits.
- Every run has a stable id, inputs, output revision, diff counts (added/removed/changed nodes and edges), timing, and a typed outcome; a failed run never publishes a partial graph.
- The UI lists past runs with outcomes, the in-flight run with measured progress, and the next scheduled run with a time; scrubbing between two runs animates the canvas.
- Clicking any node shows: identity, type/tags, every property as key/value, provenance (source, run that last touched it, first-seen), degree and neighbours, and body when readable — for every importer.
- Bounded memory and disk for run history; `prefers-reduced-motion` disables tweening.

## Step 2: Solution-space map

| Dimension | Options |
|---|---|
| Where the transform evaluates | graph-api native (blocking thread, like `/generate`) · browser wasm (like the Generate panel) · graph-compute worker · both with one Rust struct |
| Transform input | fresh generation (today) · current snapshot injected as `graph` · snapshot + wall-clock `t` · previous run's output (fold) |
| Run persistence | in-memory ring · JSONL beside the packages dir · graph-vcs commits · nodes in the graph itself · nothing (pure replay) |
| Schedule origin | `WatchPlan::Poll` interval · cron field on the catalog binding · Kubernetes CronJob/Kueue · Nix-computed sequence of `t` · manual only |
| Timeline model | sequence of graph revisions · commit DAG · run log with diffs · continuous function of `t` |
| Animation | tween positions/colors between states via `set_positions` · add/remove nodes via remount · highlight sets only |
| Node enrichment | widen `NodeMeta` (properties + provenance) · derive from graph edges to run nodes · per-node commit blame |

Trade-off axes: **generality vs. specificity** (graph-as-input vs. agent-log viewer), **purity vs. history** (pure Nix over deterministic inputs replays for free but live sources are not deterministic), **server vs. client** (server keeps one truth and serves many viewers; client needs no backend change), **reuse of graph-vcs** (rich history, but worlds are a separate host today), **wire growth** (protobuf `NodeMeta` is shared with session-manager and the browser).

## Step 3: Six approaches

### Approach 1 — Transform packages as an importer decorator with a run ledger (high probability)

**Summary.** Add `input = "graph"` to the tvix engine so a package's expr is a function of `{ graph, <variables> }`; a `GraphTransform` in `crates/importer` wraps any upstream `Importer`, applies the chain on the upstream's cadence or its own, and graph-api records each application in a bounded run ledger served at `/runs`.

A transform package is a normal `format_version = 3` tvix package with `[parser] input = "graph"`. `render_expression` gains one more binding: the current snapshot's graph rendered as a Nix attrset literal (or, at scale, served through the VFS as `/jc/input/graph.json` and read with `builtins.fromJSON`, keeping `render_expression` O(1)). The new `crates/importer/src/transform.rs` defines `GraphTransform { package, variables, schedule }` with `apply(&VaultGraph) -> Result<TransformOutcome>` where `TransformOutcome { graph, diff: GraphDiff, stats }`, and `TransformedImporter<I: Importer>` composing it with an upstream importer through the existing `Importer` trait (`descriptor` merges capabilities; `import` runs upstream then `apply`). The catalog's source definition gains `transforms: [{ package, variables, schedule }]` so a chain is configuration, not code. A new Nix file `graph-iter.nix { graph }:` provides `mapNodes/filterNodes/foldNodes/mapEdges/filterEdges/foldEdges/groupBy/pipe`, all `listToAttrs`-based and taking `{ graph }` like `graph-combinators.nix`.

A run is `Run { id, transform_id, started, finished, input_revision, output_revision, outcome: Ok{added,removed,changed}|Failed{msg}|Skipped{Unchanged}, duration }`. graph-api keeps a ring of the last N runs in `AppState` and appends one JSONL line to `<packages_dir>/runs.local.jsonl` (same overlay convention as `catalog.local.json`, `importer_catalog.rs:225`). Scheduling reuses `WatchPlan::Poll { interval_ms }` and adds one optional `schedule = "every 5m" | "cron …"` on the binding; `next_run_at` is computed from it and served. `/runs` (JSON) lists past + in-flight + next; `/runs/:id/diff` serves the diff as dense node indices against the output revision. The UI adds a **Runs panel** (past/in-flight/upcoming, outcome, counts, error) and extends the Graph panel with a **revision scrubber**: selecting a run fetches that run's diff and tweens colors/alpha via `set_search_highlights` (added = highlight, removed = ghost) and positions via `set_positions`; reduced-motion snaps. Node content is fixed at the root: `RawNode` keeps `metadata` as `serde_json::Map`, `convert_generated_graph` maps it into `frontmatter`, and `NodeMeta` gains `provenance { source_id, first_seen_revision, last_run_id }`; the Nodes focus view gains a generic **Facts** section (every frontmatter key/value, tags, degree, neighbours) above the Document viewer so metadata-only nodes are no longer blank.

- **Key decisions:** transform = tvix package with `input = "graph"`; Rust struct is an `Importer` decorator; ledger is JSONL + ring; schedule is an interval/cron on the binding; diff is server-computed.
- **Trade-offs:** widest reuse of existing seams; graph literal injection is the scalability wall (mitigated by VFS JSON); protobuf change to `NodeMeta` touches session-manager and app proto regen.
- **Probability:** 0.85. **Complexity:** medium-high.
- **Risks:** tvix eval time on a 100k-node input; `deepSeq` forces the whole output so laziness is only inside the expression; cron parsing scope creep — keep to `every <duration>` plus fixed clock times.
- **Non-requester benefit:** an Obsidian user writes "label every note with its folder depth and connect orphans to an inbox node" once and sees it run after every save; a Kubernetes user contracts ReplicaSet→Pod edges nightly.

### Approach 2 — Runs as graph-vcs commits; timeline = branch log (high probability)

**Summary.** Every transform application commits its output `Snapshot` to a world's branch in graph-vcs, so run history, diffs, blame, and branching come from the existing commit model, and the History panel becomes the run timeline.

The transform contract and `graph-iter.nix` are as in Approach 1, but persistence and timeline are delegated to `crates/graph-vcs`. A scheduler task in `session-manager` (native host) or graph-api (standalone, using the embedded `MinigrafStore`) holds `TransformSchedule { world, branch, transform, cadence }`; each tick materialises the branch head, applies the transform, and writes a `Commit` whose message and metadata carry the run record (transform id, variables hash, diff counts, duration, outcome). A failed run commits nothing and is recorded as a branch-scoped event in `/progress`. "Upcoming" is the scheduler's next tick per schedule, served with the branch log. Because commits have parents, a transform can fork: run the same transform with two variable sets on two branches and compare; the existing merge/conflict model handles reconciling manual edits with transform output.

The UI reuses the History panel for past runs (commit rows gain diff counts and outcome badges), adds an **Upcoming** strip fed by the schedule endpoint, and animates by materialising two commits client-side (`graph_data_from_snapshot`, `graph_canvas.rs:174`) and tweening between their buffer layouts. Node content gains **blame**: `/node/:id` returns the commit that introduced the node and the last commit that changed its metadata, plus the same Facts section as Approach 1.

- **Key decisions:** run = commit; schedule lives with worlds; diff = snapshot diff already in graph-vcs; timeline = History panel.
- **Trade-offs:** richest history model with zero new persistence format; but worlds are a separate host from the standalone importer path, so the standalone `graph-api --source=…` user only gets this if graph-api also embeds a store per source.
- **Probability:** 0.80. **Complexity:** high.
- **Risks:** commit growth for high-cadence runs (needs squash/retention); snapshot materialisation cost in the browser for large graphs; scheduler duplication between hosts.
- **Non-requester benefit:** any world owner gets versioned, branchable, revertable transforms; a "dream" consolidation that misfires is one revert away.

### Approach 3 — Client-resident transform pipeline on the existing Timeline panel (high probability)

**Summary.** Keep the server untouched: the Dioxus app evaluates transform packages in-browser with `tvix-wasm` (as the Generate panel already does), records runs in localStorage, schedules with a browser timer, and repurposes the existing Timeline `FrameRing` to hold graph-state frames instead of only position frames.

The Rust transformation struct lives in `app/ui/src/transform.rs`: `Transform { package: ValidatedPackage, variables, cadence }` with `stage()` steps expressed through `panel_kit::loading` stores (fetch snapshot → build Nix input → eval → diff → commit to ring). Input is the current `GraphData` plus `/graph/meta_summary` and a bounded `/node/:id` prefetch, rendered into the VFS as JSON for `builtins.fromJSON`. `graph-iter.nix` ships in the embedded VFS so the same package text works server-side later. Runs (`Run` as in Approach 1) persist to localStorage under `jc_runs_v1` with a bounded count; the Timeline panel's frame ring becomes `enum Frame { Positions(..), GraphState { run_id, ids, edges, colors } }` and the scrubber moves between graph states with a tween. Upcoming runs are the timer's next fire plus the cadence, shown in the same panel.

Node content: the app already has `frontmatter_json`; the Nodes focus view gets the Facts section, and transform outputs that add `metadata` are merged client-side into a per-run overlay shown as "run-derived facts" with the run id.

- **Key decisions:** no server changes; wasm eval; timeline = extended `FrameRing`; schedule = browser timer.
- **Trade-offs:** fastest to ship and demoable on any graph-api; but runs exist only while a tab is open, history is per-browser, and wasm eval blocks the UI thread above the caps (the retired tvix-worker would need restoring, `lib.rs:39-41`).
- **Probability:** 0.80 for a first vertical slice; 0.55 as the final shape.
- **Complexity:** medium.
- **Risks:** duplication once a server path is wanted; localStorage quota; eval on the main thread.
- **Non-requester benefit:** a viewer of a read-only deployment (no packages dir write access) can still author and run transforms against what they see.

### Approach 4 — The reflexive graph: runs, schedules, and transforms are nodes (low probability)

**Summary.** Model transforms, schedules, and runs as nodes and edges in the graph they operate on (`type = transform|schedule|run`, edges `run→touched node`, `schedule→transform`, `run→run` for sequence), so the timeline is a subgraph and node content is enriched by traversal rather than by new wire fields.

The transform contract stays tvix `{ graph, … }`, but `GraphTransform::apply` writes its own provenance into the output: a `run:<id>` node with metadata `{started, finished, outcome, added, removed}` and edges to every node it created or changed. Schedules are `schedule:<id>` nodes whose metadata holds cadence and `next_run_at`, kept current by the scheduler rewriting that one node. The UI needs no new panel: the Nodes navigator's tag tree shows `run/…` and `schedule/…` groups; selecting a run node highlights its neighbourhood (touched nodes) using `set_search_highlights`; the Inspector's neighbour pills already render the lineage. A time scrubber is a filter over `run` nodes by `started`.

- **Key decisions:** no new persistence, no new endpoints, no proto change; provenance is topology.
- **Trade-offs:** maximally generic and searchable (runs are Tantivy documents); but it pollutes metrics (PageRank of a run node is meaningless), inflates edge counts by O(touched nodes), and makes "upcoming" a mutable node that the importer must overwrite.
- **Probability:** 0.08. **Complexity:** low-medium.
- **Risks:** users cannot opt out of the pollution without a filter; the existing `Unchanged` short-circuit no longer fires because provenance nodes change every run.
- **Non-requester benefit:** any source gains provenance nodes explorable with tools that already exist; nothing to learn.

### Approach 5 — Transforms as a graph-compute engine with Kueue-scheduled runs (low probability)

**Summary.** Run transformations as a job kind in `graph-compute` (the gRPC worker behind graph-api), schedule them as Kubernetes CronJobs admitted by Kueue, and stream the results as frames over the existing layout WebSocket.

graph-api gains a `LoadGraph`-style RPC for transform jobs; the worker evaluates the Nix package natively, computes the diff, and returns a new CSR + metadata patch; graph-api swaps a snapshot and appends a run record. Scheduling is delegated to the cluster (CronJob per binding, `next_run_at` read from the CronJob status), so "upcoming" is authoritative and survives restarts. Animation reuses `/graph/layout/stream` framing (`server.rs:2374-2385`) by emitting position frames for a morph between old and new layouts.

- **Key decisions:** evaluation and scheduling off-box; timeline from the cluster's job history.
- **Trade-offs:** scale and durable scheduling for free in the Helm deployment; but the standalone/local user has neither Kueue nor a worker (`JUMP_CANNON_COMPUTE_URL` unset), so the feature would be cluster-only — contrary to "serve users beyond the requester".
- **Probability:** 0.06. **Complexity:** high.
- **Risks:** abusing the position-stream contract for topology changes; two implementations (local + worker) or none for local.
- **Non-requester benefit:** cluster operators get scheduled graph maintenance with cluster-native observability.

### Approach 6 — Time as a variable: the schedule is a lazy Nix sequence and runs are pure replays (low probability)

**Summary.** Make the transform a function of `{ graph, t }` and the schedule itself a Nix expression that yields a lazy sequence of `t` values; the Rust side walks the sequence against the wall clock, and because evaluation is pure, any past or future state is recomputed on demand from the input snapshot and `t` — no run ledger at all.

The package declares `[parser] input = "graph"` and a `[parser.schedule] expr = ''{ now }: builtins.genList (i: now + i * 300) 64''`; graph-api evaluates the schedule expr to get the next 64 fire times (upcoming runs, exactly), and the transform expr at each `t` when it fires. A run's identity is `(package hash, variables hash, input revision, t)`, which is a memo key rather than a record; the timeline scrubber asks the server to evaluate the transform at an arbitrary `t` between two input revisions, so "animation" can be continuous in `t` rather than stepwise. `graph-iter.nix` gains `takeWhile/iterate/unfold` so a schedule can be an infinite lazy generator consumed by `builtins.head` on demand.

- **Key decisions:** no persistence; schedule = Nix; run = memo key; timeline is a continuous function.
- **Trade-offs:** the purest expression of "lazy parameterised generators"; but the input snapshot is not deterministic for any live source, so replaying `t` in the past requires the input revision to be retained — which reintroduces Approach 2's store through the back door.
- **Probability:** 0.07. **Complexity:** medium.
- **Risks:** users expect a run to be a fact, not a recomputation; a slow transform makes scrubbing unusably slow; error attribution for failed runs vanishes.
- **Non-requester benefit:** demo/synthetic users get smooth "evolve this graph over time" views with zero storage.

## Step 4: Diversity and overfitting check

- **Region coverage.** Approaches 1–3 sit in the conventional region and differ on *where truth lives*: graph-api ledger (1), graph-vcs commits (2), browser (3). Approaches 4–6 each break a different assumption: that provenance needs new wire fields (4), that evaluation is in-process (5), that history must be stored (6).
- **Shared floor.** Every approach fixes T7 (Nix metadata dropped at `RawNode` and `convert_generated_graph`) and adds a generic Facts section to the node view, because the requester's node-content complaint is source-neutral: the tvix projection produces nodes with empty frontmatter today, and the Nodes focus view shows only a header for metadata-only nodes.
- **Overfitting guard.** No approach mentions JSONL events, sessions, or loop kinds. The requester's "dream" consolidation is one transform package (`{ graph, window }:` that folds event nodes into labelled summary nodes) run on a cadence; the same machinery contracts Kubernetes owner chains, labels vault folders, or evolves a generated cluster graph. The pest package continues to be the only place the omp projection is named, and nothing in this design reads it specially.
- **Recommended synthesis for Phase 2.** Approach 1 as the spine (transform = tvix package with graph input; `GraphTransform` decorator; `graph-iter.nix`), Approach 2's commit model as the persistence backend where a world exists (with the JSONL ring as the standalone fallback), Approach 3's ring-based scrubber as the animation surface, and Approach 6's `t` binding as an optional variable so pure generators can evolve without a source.
