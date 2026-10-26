# jump-cannon Transformation Runs + Live Timeline: Six Architectural Approaches

**Date:** 2026-09-25
**Status:** Research only (Tree-of-Thoughts phase 1); nothing here is implemented
**Scope:** `crates/tvix-wasm`, `crates/importer` (tvix engine), `crates/graph-api`, `app/ui` panels; lands upstream in `ocasazza/jump-cannon`

Probabilities are sampling/selection likelihoods, not confidence. Approaches 1–3 are conventional; 4–6 deliberately sample low-probability regions.

## Step 1: Problem decomposition

### Core problem

jump-cannon today evaluates a Nix generator **once per import** (`crates/importer/src/tvix.rs`: `( <expr> ) { <bindings> }` → `tvix_wasm::eval_graph` → `convert_generated_graph`), publishes one `ArcSwap<GraphSnapshot>`, and the UI reloads wholesale when `x-graph-revision` advances (`app/ui/src/main.rs:1198`, `graph_canvas::load` re-seeds every node on a sphere shell). There is no notion of a transformation *of* a graph (the expression takes only scalar variables — no `graph` input), no run, no run history, no schedule, and a node click surfaces only `NodeMeta` (`server.rs:1792-1822`): title/path/tags/frontmatter/metrics/body. The tvix projection even drops node `metadata` on the floor (`tvix-wasm/src/lib.rs:242-246` `RawNode` ignores extra fields; `tvix.rs:523-535` sets `frontmatter: HashMap::new()`).

The feature must add: (a) a transformation described once and applied repeatedly, (b) a record of each application, (c) a scheduled and visible future, (d) an animated view of graph state over time, (e) node detail that is rich for *every* source.

### Constraints (source-grounded)

1. **Packages, not crates** (`AGENTS.md` "Importers: packages, not crates"): a transformation is a `format_version = 3` TOML package; `engine = "tvix"`, `{ expr, [variables] }`, admin binds values at apply time. No `SourceKind` variant per transformation.
2. **Rust everywhere**; Dioxus panels in `app/ui/src/panels/`; styling in `app.css`; panel-kit loading stores; honest progress; `prefers-reduced-motion`.
3. **tvix closure bug**: library files take `{ graph }` as a parameter (`graph-combinators.nix:3-12`); a transform package imports `graph.nix` at depth 1 and receives the input graph as *data*, never as closures.
4. **Wire**: `{ nodes, links }` via `toGraphJSON`/`parse_graph_json`/`to_graph_json`; node = `{ id, type, ...metadata }`; caps 1M nodes / 4M edges; `eval_graph` is synchronous, runs on `spawn_blocking` server-side, inline in the browser (Generate panel).
5. **Discovery contract**: every import emits one validated `SearchDocument` per node against a declared `ImporterSchema`; the tvix schema is fixed (`tvix_schema()`, `validate_package_schema` rejects `[schema]`). Transform output must re-enter that contract.
6. **Existing seams to reuse, not duplicate**: `WatchPlan::{Static,Filesystem,Poll,Push}` and `watcher.rs` drivers; `/progress` event log; `graph-vcs` `Commit`/`diff_snapshots`/`GraphOp`; the Timeline panel's `FrameRing` scrub UI; the Importers panel's catalog + variables editor.

### Subproblems

| ID | Subproblem | Required property |
|---|---|---|
| S1 | Transform input | The expression must receive the current graph (`{ nodes = {id={id,type,metadata}}; edges = {...} }`), not just scalars, without O(n²) attrset construction or evaluating the whole graph eagerly. |
| S2 | Run identity | A run = (transform id, bound variables, input revision, start/end, outcome, output revision, delta stats). Deterministic re-run of the same tuple yields the same output. |
| S3 | Composition | Runs chain: `g_{t+1} = T(g_t, vars)`; several transforms may be ordered; a transform may be pure-Rust (metrics → labels) or Nix. |
| S4 | Schedule | A source of "next run at": periodic, on-change (revision advanced), or manual; visible before it fires; survives restart. |
| S5 | Timeline | Sequence of graph states + deltas, scrubbable, animated (added/removed/relabelled nodes), without re-seeding layout on every state. |
| S6 | Node content | Every node exposes: typed properties (declared schema fields first), provenance (source, importer, revision, run), full edge list with types/metadata, per-node history, body when any. |
| S7 | Generality | The same mechanism serves Obsidian, Kubernetes, OKF, pest, httpjson, tvix generators — not only an agent-loop viewer. |

### Success criteria

- A transform package installs like `generate-clusters.toml` and applies to any active source via catalog configuration only.
- `GET` of run history and the upcoming schedule is one endpoint each; the UI shows both without polling more than the existing 2 s revision probe / `/progress` cadence.
- Scrubbing between two consecutive runs changes only the nodes in the delta on screen; positions of unchanged nodes are preserved.
- Clicking a pest node, a tvix node, and an Obsidian node all show properties, provenance, edges, history — the same panel, no per-source branch.
- Requester's loop (pest package + "dream" consolidation transform) is one *configuration* of the feature, not a code path.

## Step 2: Solution space

| Dimension | Options |
|---|---|
| Where the transform runs | (a) graph-api native, `spawn_blocking` like the tvix importer; (b) browser tvix-wasm like the Generate panel's Inline backend; (c) both behind one wasm-clean crate |
| What the Nix expr returns | (a) a whole `toGraphJSON` graph; (b) a *delta* `{ upsertNodes, deleteNodes, upsertEdges, deleteEdges }`; (c) a program (combinator AST) executed by Rust |
| How the input graph enters Nix | (a) `builtins.fromJSON` of an injected literal; (b) a VFS file `/jc/input/graph.json` read lazily via `GraphNixIO`; (c) per-node lazily-served VFS directory; (d) never — Rust iterates, Nix only maps one node/edge at a time |
| Run persistence | (a) in-memory ring + JSONL on disk under packages dir; (b) `graph-vcs` commits in a world; (c) nodes in the graph itself |
| Timeline representation | (a) list of snapshots (ArcSwap history); (b) list of deltas replayed forward/back; (c) one temporal graph with validity intervals filtered at render time |
| Schedule source | (a) `[schedule]` table in package/catalog + graph-api ticker (extends `spawn_poll`); (b) on-revision trigger; (c) external (CronJob → `POST .../run`); (d) the graph itself declares runs |
| Node enrichment | (a) widen `/node/:id` with edges/provenance/history; (b) keep `/node/:id`, add `/node/:id/{edges,history}`; (c) client-derived from buffers already fetched |

Trade-off axes: scale (1M nodes through Nix vs Rust iteration) ↔ expressiveness (full Nix) ↔ portability (browser) ↔ durability (VCS) ↔ blast radius (touching `VaultGraph`/renderer).

## Step 3: Approaches

### Approach 1 — Transform packages + run ledger in graph-api (probability 0.90, complexity medium)

**Summary.** Extend the tvix engine so a package's expression may take `graph` as a declared input; graph-api applies an ordered list of such packages after every import (and on a schedule), records each application in a bounded run ledger, and serves ledger + deltas to a new Runs panel that animates the canvas.

The package gains `[parser] input = "graph"` (default `"none"`, preserving `generate-*.toml`). The rendered call becomes `( <expr> ) { graph = import /jc/input/graph.nix; <bindings> }` where `GraphNixIO` serves `/jc/input/graph.nix` from memory as a `builtins.fromJSON` of the current snapshot's `{nodes,edges}` attrset form — the VFS already exists (`lib.rs:122-205`), and the import is lazy so an expression that only touches `graph.nodes` never forces `edges`. Nix-side, `graph.nix` grows `mapNodes`, `filterNodes`, `foldNodes`, `mapEdges`, `filterEdges`, `partitionBy`, `relabel` — thin wrappers over `builtins.map/filter/foldl'` on `toNodeList`/`toEdgeList` — plus `delta g1 g2`. The expression returns `toGraphJSON` output as today (S1, S3).

Rust-side, a new wasm-clean crate `graph-transform` defines
`trait Transform { fn id(&self) -> &str; fn apply(&self, input: &VaultGraph, ctx: &RunCtx) -> Result<TransformOutput, TransformError>; }` with `TransformOutput { graph: VaultGraph, documents: Vec<SearchDocument>, delta: DeltaStats }`. `TvixTransform` (engine package + bound variables) and a few built-ins (`LabelCommunities`, `PruneDegree`, `ContractChains`) implement it; built-ins are what a "map/reduce" over metrics looks like when Nix is the wrong tool for 1M nodes. graph-api's `rebuild_snapshot` (`watcher.rs:476`) becomes import → transforms → snapshot; each `apply` writes a `RunRecord { run_id, transform_id, variables, trigger, scheduled_at, started_at, finished_at, status, input_revision, output_revision, delta }` into a `RunLedger` (ring in `AppState`, appended to `runs.jsonl` under `JUMP_CANNON_IMPORTER_PACKAGES_DIR` for restart survival; S2). Schedule: the catalog source definition gains `transforms = [{ package, variables, schedule = { every_ms | on_change | manual } }]`; the existing poll driver pattern (`spawn_poll`) runs a ticker per transform; `RunLedger::upcoming()` is computed from the last run + schedule (S4). Endpoints: `GET /runs?since=`, `GET /runs/upcoming`, `GET /runs/:id/delta` (ids added/removed/changed as dense indices + revisions), `POST /runs/:transform_id` (manual trigger, default-source only like `/generate`).

UI: a **Runs panel** (past list + upcoming list + trigger button, honest `m:ss` until next), and the canvas gains `render::apply_delta` — on revision advance, `reload_graph` carries positions by id from `positions_cpu()` into the new scene so unchanged nodes stay put; added nodes spawn at the centroid of their neighbours and fade in over 400 ms, removed nodes fade out, `prefers-reduced-motion` snaps. Scrubbing past runs replays deltas from the ledger onto the current buffers (S5). Node content (S6): `RawNode`/`RawEdge` gain `#[serde(flatten)] extra` so tvix metadata reaches `frontmatter`; `/node/:id` gains `edges: [{other, direction, edge_type, metadata}]`, `provenance {source_id, importer_id, importer_version, graph_revision, last_run_id}`, `history: [run_id, op]` (from the ledger's per-run touched-id index); the Inspector renders schema-declared fields as a typed table, then leftovers, then edges, then a compact run strip.

**Key decisions.** Whole-graph output, not deltas (Rust computes the delta with `graph_vcs::diff_snapshots`, so Nix stays simple). Ledger in graph-api, not VCS. Schedule in catalog, not package (a package stays pure mechanism; cadence is deployment configuration — mirrors `pollIntervalMs` on httpjson sources).
**Trade-offs.** Full-graph JSON into Nix is O(n) memory and slow past ~100k nodes; acceptable because transforms on generator/pest/Obsidian scale are the common case and built-in Rust transforms cover the big graphs. Ledger is per-process; multi-replica deployments see divergent ledgers.
**Non-requester value.** Kubernetes source: hourly `ContractChains` collapses ReplicaSet→Pod fans; Obsidian: `LabelCommunities` writes a `community-label` tag from Louvain; OpenAlex: periodic citation-cluster relabelling. The Runs panel shows importer reloads too (an import is run zero), so every source gets "what changed and when".
**Risks.** `deepSeq` on a 1M-node attrset can stall the blocking thread — needs a per-run time budget and `JC_MAX_*` caps applied to input as well as output. Position carry-over must key on id, not index, or a reorder scrambles the scene.

### Approach 2 — Runs are graph-vcs commits; the History panel is the timeline (probability 0.85, complexity medium-high)

**Summary.** Reuse the jj-style VCS already in the repo: every transformation run commits the resulting snapshot to a branch of a world, the commit's `GraphOp` diff is the delta, and the History/Branches panels (plus the session manager's Push importer) become the run timeline.

The transform contract and Nix-side library are identical to Approach 1 (`input = "graph"`, `graph-transform` crate). What differs is persistence and scheduling: graph-api embeds a `MinigrafStore` (already wasm/native, single-file) keyed by source id; each run does `store.commit(branch = "transform/<id>", snapshot, message = run metadata)`. `diff_snapshots(prev, next)` is the delta; `ChangeId` lets a re-run of the same tuple be recognised as the same change. Scheduling lives where reconcile loops already exist — the session manager's world host — with a `TransformSchedule` per world; the standalone graph-api runs the same scheduler struct in-process. "Upcoming" is the scheduler's next-fire list; "past" is `vcs.log(branch, n)`, which `panels/history.rs` already renders and polls every 1.5 s.

The UI needs less new surface: History gains a "runs" filter and a per-commit delta badge; clicking a commit pushes that snapshot to the canvas through the existing `graph_data_from_snapshot` path, with the position carry-over from Approach 1. Node history is `vcs` op-log filtered by node id (`OpLogEntry`), so `/node/:id/history` is a store query rather than a new index. Branching is free: a "dream" transform can commit to `transform/dream` while `main` tracks the raw import, and the Merge panel shows the consolidation as a merge with first-class conflicts when the import moved underneath it.

**Key decisions.** Persist through VCS, not a bespoke ledger; scheduler is a world-host concern; branches separate raw vs transformed graphs.
**Trade-offs.** Deepest reuse and the strongest durability/provenance story, but the standalone graph-api binary today has no VCS store; wiring one in means every deployment grows a minigraf file, and commit-per-run of a 1M-node snapshot is heavy (snapshot dedupe by content hash mitigates). Conflicts between a scheduled transform and a live import are a real semantic that users must understand.
**Non-requester value.** Anyone using worlds already gets run history in the panel they know; teams can branch a transformed view without disturbing the raw source. Obsidian users get "what did the nightly labelling change" as a diffable commit.
**Risks.** Two sources of truth if the served graph tracks `main` but runs commit elsewhere; must define that the served snapshot is the head of the configured branch. Session-manager coupling makes the local `just dev-up` path heavier.

### Approach 3 — Client-side lazy pipeline over the fetched graph (probability 0.80, complexity low-medium)

**Summary.** Run transformations in the browser with the tvix-wasm the app already ships (Generate panel Inline backend), over the buffers the app already fetched, on a browser-side scheduler; the existing Timeline panel's `FrameRing` generalises from position frames to graph-state frames.

The `graph-transform` crate is wasm-clean by construction (like `importer` with `--no-default-features`), so `TvixTransform::apply` runs identically in a worker. The input graph is built from `GraphData.ids` + `/graph/edges` + `/graph/meta_summary` (already client-resident) into the Nix attrset form. Runs are scheduled by a `spawn_forever` ticker (the Timeline panel's `ensure_ticker` pattern) with a per-transform interval persisted in localStorage; each run stores `{record, delta}` in a bounded ring and, when an embedded world is open, commits to it via `EmbeddedSessionManager` so it survives reload. The Timeline panel gets a second track: "graph states" beside "positions"; scrubbing applies the delta to the live scene with the same `set_positions` hold semantics. Node content: the client derives edges/history for the focused node from its own buffers and ring; `/node/:id` still supplies title/properties/body, but a client-only graph (Generate output) finally gets a real focused-node view instead of "No hosted content" (`nodes.rs:745-751`).

**Key decisions.** Zero server changes in the first increment; runs are per-viewer; server-side execution is a later `POST /transform` mirror of `/generate`.
**Trade-offs.** Fast to ship and works for client-only graphs, but history is not shared between viewers or machines, the browser thread limits graph size (the tvix worker with timeout is retired), and "upcoming runs" are only as durable as the tab.
**Non-requester value.** Generate-panel users can iterate `subdivideRule`/`hubRule` rewrites as animated steps; researchers with a static vault can prototype a labelling transform without deploying anything.
**Risks.** Diverges from "packages bound by an administrator" — a per-viewer schedule is not deployment configuration; risk of building a second, weaker run model that must later be migrated to the server.

### Approach 4 — Temporal graph: validity intervals, timeline as a render-time filter (probability 0.08, complexity high)

**Summary.** Stop producing snapshots; instead every node and edge carries `[valid_from, valid_to)` in run-index units, one snapshot holds all history, and the timeline is a uniform `t` the shader uses for alpha — scrubbing and animation cost nothing and never reload.

A transform's output is merged into the temporal graph: unchanged nodes keep their interval, removed nodes get `valid_to = t`, added nodes get `valid_from = t`, relabelled nodes are closed and reopened (history is then literally the node's list of intervals, answering S6 for free). `VaultNode` gains `intervals: Vec<(u32,u32)>`; graph-api serves a `/graph/validity` u32 buffer; the renderer adds a `t` uniform; metrics/search operate on "current" (`valid_to == u32::MAX`) unless a `?t=` is given. Node click shows the node's entire lifetime.

**Key decisions.** Time is a dimension of the data, not of the store.
**Trade-offs.** The only design where scrubbing 1000 runs is instant and layout is continuous by construction. But it changes `VaultGraph`, every importer's output, metrics, facets, the binary cache, and the shaders; unbounded growth needs compaction; edges whose endpoints have disjoint intervals are a new invariant class.
**Non-requester value.** Kubernetes and GitHub sources are naturally temporal (pods churn, notes get renamed) — users would see churn as a scrub, not a reload.
**Risks.** Blast radius across seven crates; conflicts with the `SourceEvent`/delta plan in `docs/importer-architecture.md` unless intervals are derived from that op-log rather than replacing it.

### Approach 5 — The graph schedules itself: runs, schedules, and transforms are nodes (probability 0.07, complexity medium)

**Summary.** Represent the ledger and the schedule *inside* the graph: `kind = transform`, `kind = schedule`, `kind = run` nodes with `run -> run (next)`, `run -> transform`, and `run -> touched node` edges, produced by a reserved built-in mapper; the "upcoming runs" UI is a facet filter on the canvas, and the executor is a Nix `foldl'` over `schedule` nodes.

A source's transforms are declared as today, but graph-api projects them as a subgraph under a reserved namespace (`jc:run:*`), merged into every snapshot with `graph.merge`. The scheduler is a Rust transform that reads `schedule` nodes (`every_ms`, `last_run`) and emits `run` nodes whose `scheduled_at > now` are the upcoming list. Node click on a data node shows its `run` neighbours = its history; node click on a `run` node shows its delta as neighbours. The Runs panel is a saved search (`tags:run kind:upcoming`) rendered as a table, the same way the topos note's "Sessions" panel is a table over `kind=session`.

**Key decisions.** No new endpoints, no new ledger type; provenance is edges.
**Trade-offs.** Radically dogfooded and self-describing; every existing surface (search, facets, Inspector neighbours) works on runs immediately. But meta-nodes pollute metrics (PageRank of a `run` hub), layouts (a run touching 10k nodes is a giant star), and counts; needs a `hidden` node class in the renderer and metric exclusion.
**Non-requester value.** Any source gains an explorable provenance graph; "show me everything the nightly relabel touched" is a neighbour click.
**Risks.** Users confuse data with bookkeeping; reserved namespace collisions with pest packages that emit `run` kinds; the requester's loop already emits `session`/`event` nodes, so the two vocabularies must be visibly distinct.

### Approach 6 — Nix compiles a combinator program; Rust executes it lazily over a graph op-stream (probability 0.05, complexity high)

**Summary.** The tvix expression evaluates not to a graph but to a small AST of `map | filter | reduce | window | relabel | contract` combinators (the `gclTypeEnv` vocabulary made concrete); the `graph-transform` crate interprets that AST as a lazy `Iterator` pipeline over the importer's `GraphOp` stream, so a 1M-node graph never materialises as Nix attrsets and every run is a window over the op-log.

`graph-combinators.nix` gains a `program` namespace whose functions return data (`{ op = "filter"; field = "tags"; has = "gate"; }`, `{ op = "reduce"; by = "community"; into = "cluster"; }`) rather than acting on `graph`; `eval_graph` gets a sibling `eval_program`. Rust owns execution: `Program::compile(ast) -> Pipeline`, `Pipeline::run(ops: impl Iterator<Item=GraphOp>) -> impl Iterator<Item=GraphOp>`. This lands the `SourceEvent`/`GraphOp` boundary from `docs/importer-architecture.md` Phase 3 as a prerequisite; the timeline is the op-log with run boundaries as checkpoints, deltas are free, and node history is a seek in the log.

**Key decisions.** Nix is the configuration language, Rust is the engine; Nix never sees the data.
**Trade-offs.** Scales to the caps and is the only design consistent with the retired tvix-worker's absence (no unbounded eval), but per-node arbitrary Nix logic is lost — the AST vocabulary bounds expressiveness, and the requester's "dream consolidation" must be expressible in it. Depends on unlanded op-stream work.
**Non-requester value.** Kubernetes watches and httpjson polls become native op streams; a transform over a stream is the same as a transform over a reload.
**Risks.** A DSL inside Nix inside TOML is three layers of syntax for users; vocabulary churn until real transforms exist.

## Step 4: Diversity and overfitting check

| | Exec site | Nix returns | Persistence | Timeline | Schedule | Node content |
|---|---|---|---|---|---|---|
| 1 | server | graph | ledger ring + jsonl | delta replay | catalog ticker | widened `/node/:id` |
| 2 | server/world host | graph | graph-vcs commits | commit log | world scheduler | op-log query |
| 3 | browser | graph | localStorage / embedded world | FrameRing track | tab ticker | client-derived |
| 4 | server | graph | one temporal graph | shader `t` | any | intervals |
| 5 | server (transform) | graph | nodes in the graph | facet filter | schedule nodes | neighbour edges |
| 6 | server iterator | program AST | op-log | op-log windows | checkpoints | log seek |

The three conventional approaches agree on the contract (`input = "graph"`, `graph-transform` crate, metadata flattening into frontmatter, position carry-over by id) and differ only in persistence/scheduling/host — that shared core is the likely selection regardless of winner. 4–6 each move one axis to an extreme: data model (4), representation (5), execution model (6).

**Overfitting guard.** The requester's use is: pest package → periodic "dream" consolidation → watch live. Every approach above serves that as configuration (a transform package with `input = "graph"` bound to the `ocasazza.omp-auto-loop` source with `schedule.every_ms`), and none introduces `session`/`event`/`omp` vocabulary into Rust. Generic wins that a non-requester notices first: tvix node metadata finally reaches the Inspector; the Inspector shows edges with types and provenance for Obsidian/Kubernetes/OKF nodes; importer reloads appear as runs; a Louvain-label transform is a one-file package for any source. The animated DAG is the delta animation, which is source-agnostic; the loop's "DAG of tasks" is simply the subgraph the pest package already emits. The "dream" topos itself should be an example package under `charts/jump-cannon/packages/` and a knowledge note beside `OMP Auto-Loop Topos.md`, not a code path.

## Grounding

- `crates/importer/src/tvix.rs:354-371,512-566,625-672` — rendering, projection (frontmatter empty), import path.
- `crates/tvix-wasm/src/lib.rs:104-116,122-205,241-259,268-298` — VFS, `RawNode` drops metadata, eval wrapper.
- `crates/tvix-wasm/src/nix/graph.nix:16-149` and `graph-combinators.nix:162-231` — attrset graph, rewrite rules to generalise.
- `crates/graph-api/src/watcher.rs:263-279,464-535` — poll driver, `rebuild_snapshot`; `server.rs:54-119,1753-1921` — routes, `/node/:id`.
- `crates/graph-vcs/src/lib.rs`, `merge.rs:203` — commits, `diff_snapshots`.
- `app/ui/src/main.rs:1198-1204` (revision probe → `reload_graph`), `graph_canvas.rs:38-52` (sphere re-seed), `panels/timeline.rs` (FrameRing/ticker), `panels/nodes.rs:744-816`, `panels/inspector.rs:1-30`, `panels/history.rs:12-65`.
- `docs/importer-architecture.md:178-212,625-640` — `SourceEvent`/`GraphOp` plan; `charts/jump-cannon/knowledge/OMP Auto-Loop Topos.md`.
