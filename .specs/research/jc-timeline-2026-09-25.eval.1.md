# Evaluation: jc-timeline full solutions B1 / A1 / C1

- **Specification:** `.specs/research/jc-timeline-eval-spec.yaml` (read verbatim; 34 checklist items, 10 weighted dimensions, `total = Σ(w_d × s_d)`)
- **Artifacts:** `jc-timeline-2026-09-25.solution.{b1,a1,c1}.md` (720 / 686 / 885 lines; read in full)
- **Ground truth used for CK-034:** the repository itself. ~25 cited line references were spot-checked against the tree.

> **Aggregation note.** This spec states `total = Σ(weight × score)` with weights summing to 1.00 and **no division by 5**, and it states **no global essential cap**: an `essential` NO caps only its linked dimension at 2, an `important` NO at 3, a `pitfall` YES at 2. Every cap below is dimension-local and applied *after* the anchor placement. Where a cap bit, the native placement is recorded so the cap is auditable.

---

## 0. Repository facts verified before scoring

These are the anchors the spec's own rationales cite. All confirmed directly.

| Fact | Spec's claim | Verified |
|---|---|---|
| `NodeMeta` field set | 11 fields, `crates/vault-data/src/node.rs:5-25` | ✅ exactly 11 |
| Output caps | `JC_MAX_NODES`/`JC_MAX_EDGES` = 1M/4M | ✅ `DEFAULT_MAX_NODES: usize = 1_000_000`, `DEFAULT_MAX_EDGES: usize = 4_000_000` |
| `[[schema.fields]]` rejected on tvix | `crates/importer/src/tvix.rs:293-307` | ✅ `validate_package_schema` rejects it, with the exact comment quoted |
| Route set | `server.rs:58-118` | ✅ `api_routes()` at :54, all named routes present |
| `panels/timeline.rs` size | 768 lines | ✅ |
| `panels/nodes.rs:754` `Spinner` | bare Spinner on node click | ✅ `Spinner { label: "loading node…" }` at :754 |
| `eval_seed` idiom | `let n = {n}; in (\n{expr}\n)` at `lib.rs:416-418` | ✅ exact, line for line |
| `/generate` blocking path | `spawn_blocking` at `server.rs:751` | ✅ exact |
| `spawn_poll` + coalesce | `watcher.rs:263-285`, `MissedTickBehavior::Skip` at :283 | ✅ exact |
| `graph-combinators.nix` closure workaround | file header, `{ graph }:` shape | ✅ :3-6 warning, :12 `{ graph }:`, :26 `Y` |
| `RuntimeObserver::observe_execute_op` | exists, default no-op | ✅ `observer.rs:76` |
| `graph.nix` attrset shape | `:4-6` | ✅ exact |
| `graph.nix` `toGraphJSON` emits node+edge metadata | `:143-149` | ✅ exact |
| `graph.nix` attrset-copy `addNode` | `:16-50` | ✅ `emptyGraph` :16, `addNode` :21 |
| `graph.nix` edge ids `"e<i>"` | `:128` | ✅ `addEdge ("e" + idx)` :128 |
| `RawNode`/`RawEdge` drop extra keys | `:219-252` | ✅ `GenNode` :221, `RawNode` :242, `RawEdge` :249 |
| `update_colors` / `update_edge_colors` / `update_sizes` / `set_positions` | exist on `GraphPipelines` | ✅ `pipelines.rs:1231 / 1250 / 1270 / 1743` |
| proto `NodeMeta` highest field number | — | ✅ **28** (`bool anomaly_flag = 28;`) |
| `DiscoveryField` shape | — | ✅ `#[serde(deny_unknown_fields)]`, fields `key`, `field_type`, `required`, `searchable`, `facetable`, `snippet`, `boost` |
| `validate_output` per-node namespace prefix | `data-loader:595-600` | ✅ exact |
| `WatchPlan` / `ImportProgress` / `Importer` | `data-loader:957-970 / 972-1120` | ✅ `WatchPlan` :960, `ImportProgress` :1069, `Importer` :1102 |
| AGENTS.md styling + impeccable law | `:196-200` | ✅ "Styling lives in `app/ui/assets/app.css` + `panel_kit::CSS`"; "correct ARIA roles, one clear hierarchy per surface" |

---

## 1. Per-solution checklist resolution

Deciding quote given for every item. **NO** on a `fatal` item would disqualify; none occurred.

### 1.1 B1 — "Transform packages + run ledger in graph-api"

| ID | Tier | Answer | Deciding quote / evidence |
|---|---|---|---|
| CK-001 | fatal | **YES** | "`crate::motion::prefers_reduced()` (`web_sys::Window::match_media("(prefers-reduced-motion: reduce)")`, cached, re-read on the media query's `change` event)" — Rust/web_sys only; no JS, no bundler, no three.js anywhere. |
| CK-002 | fatal | **YES** | "A transform is a tvix package with `input = \"graph\"`" + manifest `format_version = 3` / `[parser] engine = "tvix"` / `[[parser.variables]]`. No `SourceKind` variant; §4.4 `ValidatedPackage::tvix_input()` *rejects* hosting a graph-input package as a source. Built-ins are retrofitted as packages precisely to keep the law: "Built-ins as Rust types \| Built-ins packaged as `engine = \"transform\"` packages in phase 5 \| \"Importers: packages, not crates\" (AGENTS.md)." |
| CK-003 | fatal | **YES** | §3.4: "`default-features = false` drops `readFile`/`readDir`/`pathExists`/`getEnv`/`fetch*`. The transform sees precisely two things: the `graph` value and its declared variables." Workaround named: "Per-run external state … is bound as a **declared, typed variable** whose catalog value is a host reference" with the `$run.index` / `$run.started_at_ms` / `$run.input_revision` / `$run.trigger` table. |
| CK-004 | fatal | **YES** | §10: "The words `session`, `agent`, `tool`, `goal`, `omp`, and any filesystem path are absent from `graph-transform`, `runs`, the routes, `proto::NodeMeta`'s new fields, and `panels/runs.rs`." Q5 reports a self-audit with only 3 benign hit classes (the pre-existing `session-manager` crate, the prohibition itself, and the `OMP Auto-Loop Topos.md` example config). |
| CK-005 | essential | **YES** | §5.3 is an 11-row table with a record status, a **Snapshot** column reading `kept` on every failure, a progress row, and a schedule effect. Covers input-too-large, invalid variables, eval throw, ops cap, wall cap, output-too-large, empty, projection, snapshot-build, internal panic, disk-write. |
| CK-006 | essential | **YES** | §5.2: "The output caps are enforced where they already are: `check_caps_against` in `parse_graph_json` … with `max = min(JC_MAX_NODES / JC_MAX_EDGES, package [limits])`, **before any `GenNode` is materialised**." |
| CK-007 | essential | **YES** | §7.1, headed "The bound, in one sentence", in the same section that introduces the ledger: 512 records / 64 deltas / 64 MiB / keyframe every 16 / ≤4 keyframes. |
| CK-008 | essential | **YES** | `RunDelta { nodes_added, nodes_removed, nodes_changed, edges_added, edges_removed }` of `NodeRef`/`(String,String)` — ids only — plus `Keyframe`; `presence_delta` is O(n+e), `graph_hash` an XOR fold. Q3 bounds the worst case: "A 1M-node `allow_empty` run produces a delta of 1M removed ids ≈ 40 MiB — inside the byte bound". |
| CK-009 | essential | **YES** | Header line 6: "a bounded `RunLedger` beside the `ArcSwap<GraphSnapshot>` in `AppState`". Diagram keeps `RunLedger` and the `ArcSwap` snapshot as separate nodes. |
| CK-010 | essential | **YES** | Output is built as a *new* `GraphSnapshot` via `GraphSnapshot::build` and only then `snapshot.store(...)`; the record carries `output_revision: Option<u64>`. §5.3 marks the previous snapshot `kept` for every non-`Succeeded` status, so no in-place half-mutation exists. |
| CK-011 | essential | **YES** | §6.2: "This design adds **one `select!` arm** to those loops and nothing else" — with the literal `tokio::select!` block. Closes the `WatchPlan::Static` gap explicitly. |
| CK-012 | essential | **YES** | §6.1 table with **Missed interval** and **Overlap** columns for all four triggers. `every`: "**Coalesced**: however many intervals elapsed … exactly **one** run fires … Never replayed, never queued." Overlap: "**Impossible by construction**". `daily`: grace window, then a recorded `Skipped { MissedWindow }`. |
| CK-013 | essential | **YES** | `Trigger { AfterImport, Every { interval_ms }, Daily { at_utc }, Manual }` in the catalog's `TransformBinding`, `#[derive(Serialize, Deserialize)]`, `deny_unknown_fields`; `next_due_at_ms` seeded "from the ledger on boot"; editable via the workbench routes. |
| CK-014 | essential | **YES** | §3.3: "Takes the graph.nix library as the `graph` PARAMETER (the same tvix closure-across-3+-imports workaround graph-combinators.nix uses, graph-combinators.nix:3-12) and imports nothing itself", with the depth-3 `birds.nix` analysis. Cited range verified exact. |
| CK-015 | essential | **YES** | §3.2 heading: "the `eval_seed` idiom, done natively" — "`EvaluationBuilder::env` … is precisely what `eval_seed`'s `let n = …; in (…)` does textually (`tvix-wasm/src/lib.rs:418`)". Verified: line 418 is exactly that. |
| CK-016 | essential | **YES** | §5.1: "the expression string, the `Rc<InputGraph>` … and the budget cross into `tokio::task::spawn_blocking`; only the `Result<GeneratedGraph, EvalFailure>` crosses back." |
| CK-017 | essential | **YES** | §9.3: "`reload_graph` computes `added = new_ids − old_ids`, `removed = old_ids − new_ids` client-side (**no fetch needed for the live case**)" then patches a subset of the **sizes** buffer via a new writer "beside `update_sizes`, `pipelines.rs:1270`" (verified: `update_sizes` is at 1270) and pulses colours. No per-tick `/graph/init`+`/graph/ids`+`/graph/edges` refetch anywhere. |
| CK-018 | essential | **YES** | §12 Phase 1: "the existing revision probe drives a plain `reload_graph` (**full re-seed as today**)"; §12 Phase 3 splits `graph_canvas::load` (id-keyed carry) from `render::begin_transition` (per-frame). Structurally distinguished, though never stated as a constraint sentence — noted in D6. |
| CK-019 | essential | **NO** | §8's justification column is headed **"Why it exists"**, not "why no existing route fits", and answers that question for only 3 of 7 routes. `GET /runs`: "The Runs panel's history list" (what it's for). `GET /runs/upcoming`: "Separate from `/runs` because it is configuration + schedule state" (argues against its own sibling). `GET /runs/:id`: "The list omits `error` text to stay small" (same). `GET /importers`: "The Importers panel shows which sources carry transforms" (what it's for). The existing set is cited only as `api_routes()` (`server.rs:54-119`) and `/progress` is reused without ever engaging its `?since=<seq>` cursor. → **caps D6 at 2.** |
| CK-020 | essential | **YES** | §4.3 "Projection (`crates/importer/src/tvix.rs::convert_generated_graph`, extended)": "`frontmatter` ← every remaining metadata key (this is the fix for \"frontmatter empty, content_readable false\")". |
| CK-021 | essential | **YES** | `/node/*id` "gains `properties`, `provenance`, `edges` … `history` … additive protobuf fields, no second round-trip, no new store"; Phase 4 acceptance: "a pest node, a tvix node, and an Obsidian node show the same four sections with **no per-source branch** in `inspector.rs`". |
| CK-022 | essential | **YES** | §9.5: duration 0, highlight set for 2 s, "the Runs panel's selected-run section lists the same ids as **chips** so the information is also available as text", plus the `app.css` media rule beside the existing `:62-64`. |
| CK-023 | essential | **YES** | §9.7: "`Evaluating` is **indeterminate** (`fraction: None`)"; countdown from server `now_ms`; scrub as counted `k/N deltas`; "store pending → indeterminate bar; **never a number**". |
| CK-024 | essential | **YES** | §9.6: two `loading_store`s + `LoadingGate` + `GlobalLoadingBar`; "The Inspector's node fetch moves from its `Spinner` (`inspector.rs:37`) to `loading_store(\"inspector\", …)` + `LoadingGate`" — the exact `nodes.rs:754` wait. |
| CK-025 | important | **YES** | §3.3: "Every builder produces its attrset in ONE listToAttrs (O(n)); no `foldl'` over addNode (O(n²), see soupGen)"; code uses `nodesFromList = ns: builtins.listToAttrs (builtins.map …)`. (`degrees` does use `//` per edge; B1 flags and bounds that itself.) |
| CK-026 | important | **YES** | §15 is a full source-reference list; §4/§8/§9/§12 name dozens of concrete symbols with lines. |
| CK-027 | important | **YES** | §11, headed "Knowledge-map update (item 4) — **same change as the code**", tabulating 11 notes. |
| CK-028 | important | **YES** | §12, Phases 0-5, each with **Ships** / **Acceptance** / **Kill**, each landing on a named seam. |
| CK-029 | important | **YES** | Built-ins `LabelCommunities{field}`, `PruneDegree{max_degree}`, `ContractChains`; Phase 5 acceptance: "`ContractChains` on a **1M-node Kubernetes fixture**"; Phase 1 acceptance exercises Obsidian, Kubernetes, httpjson and the shipped generators. No omp example anywhere. |
| CK-030 | important | **NO** | Zero occurrences of `app-proto` (grep). `/node/*id` is described only as "additive protobuf fields" — the checked-in prost types in `app/ui/src/proto/` and the `just app-proto` regen are never named. → **caps D7 at 3.** |
| CK-031 | important | **YES** | §9.5 puts the new strip rule in `app.css` (`.runs-strip .run { transition: r 200ms, fill 200ms }`); §9.1 uses `panel_kit::badge` "colours by status"; §9.5 reuses the existing `render::set_search_highlights` alpha convention. No inline styles. |
| CK-032 | pitfall | **NO** | No conflation. `Panel::Runs` / `panels/runs.rs` is a separate panel; the strip is "an inline `svg` in rsx: one node per run … y-lane per transform id", i.e. run records, not keyframes. `timeline.rs` is cited only for the `ScrubState::Paused` host-hold idiom. The distinction is *implicit* rather than stated — a D6 note, not a defect. |
| CK-033 | pitfall | **NO** | §6.2: "one `select!` arm … and nothing else"; §14 records replacing the per-transform ticker. No task, queue, or cron thread per transform. |
| CK-034 | pitfall | **NO** | §13/§14 are accurate — caps named correctly, `observe_execute_op` cited at `observer.rs:42-76` (verified exact), `eval_seed` at :418 exact, `graph.nix:4-6` and `:143-149` exact, `graph-combinators.nix:3-12` exact, `watcher.rs:283` exact, `pipelines.rs:1270` exact. Q1 **admits** a real residual risk ("a `Drop` impl in tvix aborting on unwind") with a Phase-2 kill criterion. The one false statement — `[[schema.fields]]` written as `name`/`type` in §3.1, which `DiscoveryField`'s `deny_unknown_fields` would reject — sits in the manifest example, not in a risk/caveats section, so it is carried into **D9** rather than triggering this pitfall. |

**B1 caps: D6 ≤ 2 (CK-019), D7 ≤ 3 (CK-030). No fatal, no pitfall.**

### 1.2 A1 — "Transform packages + in-memory run journal"

| ID | Tier | Answer | Deciding quote / evidence |
|---|---|---|---|
| CK-001 | fatal | **YES** | §2.8: "`window.matchMedia("(prefers-reduced-motion: reduce)")` through `web_sys` (**Rust, no JS**)"; §2.5 panel-kit stores. |
| CK-002 | fatal | **YES** | `format_version = 3`, `[parser] engine = "tvix"`, plus exactly two new parser fields with defaults that keep every shipped package valid: "`role: String (default \"generate\")`" and "`input: String`". No new engine, no `SourceKind`, no `--flags` per source. |
| CK-003 | fatal | **YES** | §2.11: "`readFile`/`readDir`/`getEnv` live in `eval/src/builtins/impure.rs` and are absent from the pure builtin set"; workaround named — the graph arrives "as a `let graph = builtins.fromJSON …; in …` binding … All other external state enters as typed `[[parser.variables]]` bound at apply time". |
| CK-004 | fatal | **YES** | §2.12: "No `session`, `agent`, `tool call`, `goal`, `omp`, or local path appears in any type, endpoint, package field, or panel string." omp appears only in acceptance fixtures and a knowledge-note example, not in the core. |
| CK-005 | essential | **YES** | §2.1's four-gate table + throw + boot; "**Every gate produces the same three visible effects**: (1) a `RunRecord` with the outcome and error text in the journal … (3) the previous `GraphSnapshot` stays in the `ArcSwap` untouched". |
| CK-006 | essential | **YES** | Q1: "G1–G4 and throw all return `Err` **before** `snapshot.store`"; G3 is `check_caps_against` (verified existing), and every row's Snapshot column reads "previous retained". |
| CK-007 | essential | **YES** | §2.2 states the bound in one bolded sentence in the same breath as the journal: 256 records, 64 MiB deltas, 2 keyframes every 16 runs, 256 MiB keyframe bytes. |
| CK-008 | essential | **YES** | "**Per-run full-graph copies do not exist.**" `RunDelta { ops: Vec<GraphOp>, touched, added_idx, changed_idx, approx_bytes }`; a big run (>25% of nodes touched) takes a keyframe *instead of* a delta. |
| CK-009 | essential | **YES** | Line 5: "an in-memory, bounded run journal inside graph-api's per-source `AppState`"; §2.2: "the live graph is never copied because it is already the `Arc<GraphSnapshot>` in the `ArcSwap`". |
| CK-010 | essential | **YES** | The transform chain yields one complete `LoadResult`; the journal records `revision_before` / `revision_after`; a failure keeps the previous snapshot and no record claims a revision. |
| CK-011 | essential | **YES** | §2.6: "Runs execute **inside the existing driver loops**; no new tokio task per transform." |
| CK-012 | essential | **YES** | §2.6: "**Missed intervals: skip, never replay**" citing `MissedTickBehavior::Skip` at `watcher.rs:283` (verified); "**Overlap: never**" via the per-source run slot; `pending_change` + "exactly one coalesced follow-up". Self-found gap: "a Filesystem change arriving during a manual run was `Skipped` in the first draft and therefore lost. **Fixed**". |
| CK-013 | essential | **NO** | A transform has **no schedule of its own**. `ImporterTransformBinding { package, variables, transform_id }` (§4.4.5) has **no trigger, interval, or plan field**; `RunSchedule { driver, interval_ms, next_due_ms: AtomicU64, last_checked_ms: AtomicU64 }` is a live `AppState` field whose `next_due_ms` "is written only by the driver that owns the timer … after each tick" — i.e. `now + k*interval`, recomputed at read time and never persisted (§4.6.3 writes "**Records only**"). §4.7: "`Push`/`Static` publish nothing" — two of four driver kinds have no upcoming row at all. Nothing to inspect or edit in the workbench. → **caps D5 at 2** (native placement 3). |
| CK-014 | essential | **YES** | §2.10: "`graph-transform.nix` takes `{ graph }` explicitly (`{ graph }:` header) exactly like `graph-combinators.nix:12`" (verified exact), "imports nothing itself (not `birds.nix`, not `graph.nix`), and inlines its own `Y`". |
| CK-015 | essential | **YES** | §1.1: "`eval_seed` already injects host state into the expression as a `let` binding (`crates/tvix-wasm/src/lib.rs:416-419`: `let n = {n}; in (…)`)". Verified exact. |
| CK-016 | essential | **YES** | §2.1: "which runs on the same `tokio::task::spawn_blocking` path `POST /generate` and `TvixImporter::import` already use". |
| CK-017 | essential | **YES** | §4.9.3, headed "Live: arrival pulse (**no remount beyond the one the app already does**)": fetches only `/runs?limit=1` and `/runs/:id/delta`, then `render::pulse` writes via `pipes.update_sizes` (`pipelines.rs:1270-1285`) — verified exact. §4.9.4's scrub interpolates via `pipes.update_colors` (`pipelines.rs:1231-1245`) — verified exact. |
| CK-018 | essential | **YES** | §1.3 gap 8 states the constraint from the failure side: "Removed nodes are absent from the live buffers, so \"dim removed nodes via `set_selected`\" cannot work on the latest run." §4.9.3-4 then splits live-reload (existing remount) from scrub (explicit `mount_canvas` of a union scene). |
| CK-019 | essential | **YES** | §4.8's column is literally "**Why this endpoint**" and names existing routes: `/runs/:id/delta` "the same shape `/search/matches` uses (`server.rs:2027-2064`)" (verified: `async fn search_matches(` at 2027); `/runs/:id/frame` "mirrors `/graph/ids`"; `/runs/:id/frame/edges` "mirrors `/graph/edges`"; `/node/*id` "**No new endpoint**: the node the user clicked already has one." |
| CK-020 | essential | **YES** | Phase 0 lands "`frontmatter` = `metadata` minus the promoted keys" in `convert_generated_graph`; §4.10.1: "the Inspector's chip walker and leftover grid light up for generated and transformed graphs with **zero new UI**". |
| CK-021 | essential | **YES** | §4.10.2: Facts are "for each `snap.schema.fields` entry in declared order" — the source-neutral contract every importer already populates; Provenance derives from `meta.source_id`; served by the existing `/node/*id`. |
| CK-022 | essential | **YES** | §2.8 + §4.9.4: `REDUCED_MOTION` global signal; arrival pulse → static 4 s `push_focus_set`; playback disabled, ◀▶ only; union-frame fade → fixed alpha; CSS media rule. |
| CK-023 | essential | **YES** | §2.9: "the tvix eval stage has no fuel and reports **indeterminate** (`ProgressBar { fraction: None }`) with elapsed time. **Never a synthetic percentage** for eval"; countdown mapped through `progress.rs:17-21`'s existing clock convention. |
| CK-024 | essential | **YES** | §2.5: two `loading_store`s, `LoadingGate`, `GlobalLoadingBar` (`main.rs:1686`), "No hand-rolled `loaded` signals", "mirrors `panels/history.rs:21,96` exactly". |
| CK-025 | important | **YES** | §2.10: "Every builder is one `builtins.genList` + one `builtins.listToAttrs` (the `soupGen` pattern), **never a `foldl'` over `addNode`** (O(n²))". (`foldNodes` uses `foldl'` over a *list* with an accumulator — O(n), and is not the attrset-copy case.) |
| CK-026 | important | **YES** | §3's S1-S11 table names ~15 files with line numbers, plus a dependency DAG. |
| CK-027 | important | **YES** | §2.4, 13 notes; §3 binds S11 to "**each phase**". |
| CK-028 | important | **YES** | §5, Phases 0-4, each with **Lands** / **Seam** / **Acceptance** / **Kill** — the only document that names the seam as a required per-phase field. |
| CK-029 | important | **YES** | §2.12: "`collapse-by-facet.toml` (collapse nodes sharing a metadata value), meaningful for **Kubernetes namespaces, Obsidian folders, generated cluster types**, or any pest `kind` alike." Phase 0/1 acceptance exercise Kubernetes and Obsidian. |
| CK-030 | important | **YES** | §4.4.6: `facts = 29; provenance = 30; touched_runs = 31;` then "Regenerated into `app/ui/src/proto/` via **`just app-proto`** (checked-in prost types, AGENTS.md)". Field numbers verified exactly right against `bool anomaly_flag = 28;`. |
| CK-031 | important | **YES** | §2.8 places the strip's reduced-motion rule in `app/ui/assets/app.css` with the `:62-64` precedent; §4.9.2 reuses `badges::frontmatter_chips`, `ProgressBar`, and the live community colour as a palette value. No inline styles. |
| CK-032 | pitfall | **NO** | §4.9.5: "Unchanged: it scrubs layout-position frames (`timeline.rs:1-27`); Runs scrubs graph states. The two are **different axes and stay separate panels** (one hierarchy per surface)." Explicit. |
| CK-033 | pitfall | **NO** | §2.6: "no new tokio task per transform". |
| CK-034 | pitfall | **NO** | Every spot-checked citation exact: proto field numbers 29/30/31 (existing max 28 ✓), `data-loader:595-600` namespace check ✓, `graph.nix:128` edge ids ✓, `server.rs:2027-2064` `search_matches` ✓, `graph-combinators.nix:12` and `:26` ✓, the `hubRule`…`parallelMerge` range ✓, caps 1M/4M ✓. And Q3 **self-corrects a false claim in place**: "the first draft claimed `validate_local_id` rejects `:`; it does not. **Corrected**." Q2 likewise: the first draft's "2 keyframes at 1 M nodes ≈ 2 × ~500 MB" is called "**not acceptable**" and fixed. No surviving falsehood found. |

**A1 caps: D5 ≤ 2 (CK-013). No fatal, no pitfall.**

### 1.3 C1 — "Transform packages as an importer decorator"

| ID | Tier | Answer | Deciding quote / evidence |
|---|---|---|---|
| CK-001 | fatal | **YES** | §10.4: "A **Rust** helper queries `window.matchMedia(...)` and listens for changes"; §10.2 panel-kit stores. |
| CK-002 | fatal | **YES** | "`TvixParserToml` gains **one optional field**. Existing generator packages remain valid because the default is `none`." Plus a type-level enforcement of the same law: "`build_tvix_importer` accepts only `TvixInput::None`; `build_graph_transform` accepts only `TvixInput::Graph`. This prevents accidentally hosting a transformation as a source." |
| CK-003 | fatal | **YES** | §4.2: "The build keeps `tvix-eval` with `default-features = false`; impure builtins and arbitrary host I/O remain unavailable", and the correction in the gaps list: "It does **not** read `/jc/input/graph.json`: `builtins.readFile` is impure in the pinned tvix build." Context arrives only via `TvixValueSource::{RunSequence, ScheduledAtMs, InputRevision}`. |
| CK-004 | fatal | **YES** | §12.12: "No requester-specific event taxonomy or machine-local location appears in a shared type, route, or panel"; §7: "**No storage location appears in shared run types or HTTP responses**." |
| CK-005 | essential | **YES** | §6 "Exact terminal behavior": 9 enumerated cases, each with an append and "retain the previous snapshot". "There is no partial success and no silent truncation. The ledger append occurs before the response/UI can observe terminal state." |
| CK-006 | essential | **YES** | §6: "enforced by a **streaming `serde` visitor before vectors are fully materialized**"; "**Node cap exceeded:** stop decoding at `limit + 1` … never truncate and publish." Strictly earlier than "before admission". |
| CK-007 | essential | **YES** | §7, one sentence introducing the store: "retains at most **256 terminal records, at most 30 days, and at most 64 MiB per source—whichever bound prunes first—plus at most one active and one upcoming entry per transform**". |
| CK-008 | essential | **YES** | "The compact reversible `RunDelta` stores stable IDs, minimal node tombstones (`id`, `type`, and fact hash), changed-property patches, and added/removed edge endpoint pairs. It is **O(changes), not O(graph)** by construction." §12.2: "no per-run full-graph copy exists." |
| CK-009 | essential | **YES** | §5.1: "The baseline is exactly one latest successful upstream graph, **not one graph per run**. The active `GraphSnapshot` remains graph-api's publication copy." §15 Q2: "The only full graphs are one source baseline, one active snapshot, and transient evaluation output." |
| CK-010 | essential | **YES** | Strongest of the three. §5.2: chain works in temporary state; "If any transform fails … discard the whole working chain. Earlier staged records become `RolledBack { cause_run_id }`; no partial graph is published"; "Only after that [snapshot build] succeeds does it atomically swap the snapshot and finalize every staged record with the published revision." |
| CK-011 | essential | **YES** | §8.1: "graph-api still starts **one** watcher task per hosted source. It does not start a Tokio task per transform." |
| CK-012 | essential | **YES** | §8.2: `MissedTickBehavior::Skip`; "`next_run_at` advances to the first future point on the original schedule anchor, so cadence does not drift"; poll ticks dropped; filesystem events coalesce into "exactly one trailing source refresh"; push/manual coalesce; "On process restart, elapsed intervals are skipped; there is no catch-up storm." |
| CK-013 | essential | **YES** | `ImporterTransformBinding { id, package, variables, run_on_source_refresh, watch: WatchPlan, budget }` — a declared, serialisable, per-transform `WatchPlan` in the catalog. Persistence is explicit: §7's `checkpoint.json` "preserves the next sequence and **schedule anchors**", and §8.2 "The checkpointed anchor determines the first future run." `ImporterDescriptor.watch` → `watches: Vec<WatchPlan>` is a declared cutover. |
| CK-014 | essential | **YES** | §4.4: `graph-iter.nix` headed `{ graph }:`; "graph-combinators.nix continues to take `{ graph }`; it does not import `graph.nix`, `graph-iter.nix`, or `birds.nix` through another layer. Package expressions instantiate each module directly and pass `graph` explicitly, avoiding the known three-import closure failure." |
| CK-015 | essential | **YES** | §4.2: binds a top-level `graph` value "through `EvaluationBuilder::env`"; applied expression is "(`<expr>`) { `inherit graph;` … }" — host state bound into scope ahead of the body, the same mechanism class as `eval_seed`; §2 cites `lib.rs:402-450` (span includes the real `fn eval_seed` at 416). |
| CK-016 | essential | **YES** | §5.1: "synchronous inside `spawn_blocking`, as in `POST /generate`"; §6: "the same native `spawn_blocking` boundary". |
| CK-017 | essential | **YES** | §10.3: "they use `mount_canvas` for the union topology and `GraphPipelines::set_positions`, `update_colors`, `update_edge_colors`, and `update_sizes` for frames" — **all four verified to exist** (`pipelines.rs:1743 / 1231 / 1250 / 1270`). "The revision poll in `main.rs` continues to call `reload_graph` after real publication" — the per-frame path never refetches. The only document naming all four APIs. |
| CK-018 | essential | **YES** | §10.3 splits the paths explicitly (union `mount_canvas` for topology, buffer patches for frames); §10.5: "graph-api leaves the graph revision unchanged, so `main.rs`'s revision probe does not reload and the canvas remains exactly on the previous good state." |
| CK-019 | essential | **NO** | §9 gives a "Justification:" line per route, but for `GET /runs` it argues against a *category* ("not a bulk numeric buffer or protobuf node payload") without naming `/graph/csr.bin` or `/node/*id`; for `/runs/:id/delta` it argues against its **own** route ("placing them in `/runs` would defeat bounded hydration"); for `POST /transforms/:id/run` it argues a mechanism, not a route. The existing route set is **never named**, and `/progress?since=<seq>` is never engaged. → **caps D6 at 2.** |
| CK-020 | essential | **YES** | §2 identifies the root cause precisely — "`convert_generated_graph` builds empty frontmatter (`crates/importer/src/tvix.rs:507-566`)" — and Phase 1 acts on it ("map it in `convert_generated_graph`"); §4.3 adds `GenNode.metadata` retention. |
| CK-021 | essential | **YES** | §11.2 `NodeFacts` is a **reused component** — "It reuses/moves, rather than duplicates" — called from **both** `nodes::focused_node` and `inspector::node_view`. Strongest cross-source statement of the three. |
| CK-022 | essential | **YES** | §10.4: play/auto-advance hidden; "selecting 0 or 1 **snaps immediately**"; intermediate dragging shows only a static highlight set; "no pulsing, tweening, or CSS transition runs"; app.css media query agrees. |
| CK-023 | essential | **YES** | §10.2: "the panel mirrors the server's `progress: Option<f32>` … `Some(f)` is shown as a percentage; `None` stays indeterminate. **Elapsed time is never converted into a fake percentage.**" |
| CK-024 | essential | **YES** | §10.2 gives the literal `loading_store` / `begin` / `update` / `succeed` / `fail` code, `LoadingGate`, `GlobalLoadingBar`, and the discipline that "A run's own failure is **data**, not a hydration failure". |
| CK-025 | important | **YES** | §4.4: "Every mapper/filter builds its result with one `builtins.map`/`builtins.filter` and one `builtins.listToAttrs`; **none folds `graph.addNode` or `graph.addEdge`** over a growing attrset … following `soupGen`, `random`, and `clustered`." The cited `graph.nix:151-261` precedent is verified and says exactly that. |
| CK-026 | important | **YES** | §2, §3, §13, §14 all name files and symbols. |
| CK-027 | important | **YES** | §12.4 names the new `Transformations and Runs.md` and 8 updated notes; §3 dependency 8: "Knowledge-map and browser coverage land with each corresponding behavior, not afterward." |
| CK-028 | important | **YES** | §14, Phases 1-4, each with **User-visible value** / **Work** / **Acceptance** / **Kill criterion**. |
| CK-029 | important | **YES** | Phase 2 acceptance: "the same annotation package runs **unchanged against markdown, Kubernetes, and generated fixtures**"; the catalog example is an `obsidian` kind. No omp example. Weakest of the three here (its shipped example is deliberately trivial), but it meets the gate. |
| CK-030 | important | **NO** | Zero occurrences of `app-proto` (grep). §11.1 says only "checked-in app proto types are regenerated" — the regen command is not named. → **caps D7 at 3.** |
| CK-031 | important | **NO** | C1 names `app.css` once, in the reduced-motion context, and never states that new styling lives in `app.css` + `panel_kit::CSS` on the existing palette — while introducing a fresh visual vocabulary (red/amber/neutral outcome badges, five marker shapes, alpha conventions) with no stated home. Ambiguous, and ambiguity resolves NO. → **caps D8 at 3** (not score-changing: D8 was already placed at 3). |
| CK-032 | pitfall | **NO** | §2: "The existing Timeline panel owns layout-position history, not graph revisions. It remains intact." §10.1: "**do not repurpose `Panel::Timeline`**, whose `FrameRing` remains layout-position history." Most explicit of the three. |
| CK-033 | pitfall | **NO** | §8.1: "It does not start a Tokio task per transform"; Phase 3 kill: "stop rollout if any configuration creates a task per transform." |
| CK-034 | pitfall | **NO** | Every spot-checked citation exact: VFS at `lib.rs:85-120` ✓, `RawNode`/`GenNode` at `:219-252` ✓, `addNode` at `graph.nix:16-50` ✓, `data-loader:781-795/957-970/972-1120` ✓, `graph-combinators.nix:3-26` ✓, `watcher.rs:148-285/313-535` ✓, `server.rs:678-774` ✓, caps 1M/4M ✓. No false claim found. |

**C1 caps: D6 ≤ 2 (CK-019), D7 ≤ 3 (CK-030), D8 ≤ 3 (CK-031). No fatal, no pitfall.**

---

## 2. Dimension scores

Each placement names the pole, quotes **both** anchors, and quotes artifact evidence **for each side** — the comparison precedes the number.

### D1 — Core abstraction and external implementability (w 0.14)

`contrast`: "The run is described as a transformation applied to a graph revision, and the transform contract is implementable from outside the repo — not a field list named after one application's loop."

**B1 — placement: strictly better than `score_4` on the same axis → 5**
- *Closer to `score_4`* (`RunRecord { id: RunId, transform: TransformId, input_revision, output_revision, status: RunStatus, delta: RunDelta }` + "// a third party implements `Transform`"): B1:373-395 `RunRecord { run_id: RunId, transform_id: String, package_id, package_version, variables, trigger, scheduled_at_ms, …, status: RunStatus, input_revision: Option<u64>, output_revision: Option<u64>, input_hash, output_hash, input_size, output_size, delta: DeltaStats, delta_retained, keyframe, budget, error }` — a superset of the anchor's spine — and B1:242-246 `pub trait Transform: Send + Sync { fn id; fn descriptor; fn apply(&self, input: &VaultGraph, ctx: &RunCtx) -> Result<TransformOutput, TransformError> }` in a crate built "wasm-clean like `importer --no-default-features`".
- *Further from* (what the anchor has that the artifact goes beyond): the anchor's bare `fn apply(g: &Graph, bound: &Vars) -> Result<Graph>` hands an external party nothing but the graph. B1:250-257's `RunCtx<'a>` additionally hands them the repo's **existing** `progress: &'a dyn data_loader::ImportProgress` — "the existing contract" — plus `budget: RunBudget`, so no outside implementer has to invent progress reporting or cancellation.
- *Lean / placement*: same-axis respect = the external implementer receives the tree's own progress contract and an explicit budget, so implementability is not limited to parties who can re-invent telemetry. `score: 5`

**A1 — placement: matches `score_4` → 4**
- *Closer to `score_4`*: A1:414-417 `RunRecord { id: RunId, trigger, forced, transform_ids: Vec<String>, started_ms, finished_ms, revision_before: u64, revision_after: Option<u64>, counts: Option<RunCounts>, outcome: RunOutcome, stages }` maps field-for-field onto the anchor's spine (`transform_ids`→`transform`, `revision_before/after`→`input/output_revision`, `outcome`→`status`, `counts`→`delta`), and A1:383-386 `pub trait GraphTransform: Send + Sync { fn descriptor() -> TransformDescriptor; fn apply<'a>(&'a self, input: LoadResult, progress: &'a dyn ImportProgress) -> ImportFuture<'a, Result<LoadResult, ImportError>> }` is the public third-party contract the anchor asks for, in the repo's own `data-loader` crate.
- *Further from `score_2`* (`RunRecord { id, session, tool, graph: serde_json::Value }`): artifact lacks — there is no `session`, no `tool`, and no `graph: serde_json::Value` anywhere; `RunCounts { nodes_added, nodes_removed, nodes_changed, … }` is a delta summary, not a graph body. A1:155 enumerates the new vocabulary as purely graph/transform nouns.
- *Lean / placement*: every field of the anchor is present, and no instance resembles `score_2`. Held off 5 because A1's `RunRecord` does not itself name the transform that produced the revision (it carries `transform_ids: Vec<String>`, the caller must read `stages`) and carries no per-run delta — a per-rebuild unit rather than a per-transformation one. `score: 4`

**C1 — placement: past `score_2`, short of `score_4`, no lean → 3**
- *Closer to `score_4`* (field spine): C1:484-502 `RunRecord { id: TransformRunId, transform_id: TransformId, sequence, package_id, package_version, package_digest, trigger, scheduled_at_ms, started_at_ms, finished_at_ms, input_revision: u64, input_digest, output_revision: Option<u64>, chain_index, variables, state: RunState, delta: DeltaAvailability }` — the anchor's spine plus digest, chain index, and a delta-availability marker.
- *Further from* (the anchor's second half, "// a third party implements `Transform`"): **artifact lacks** — C1:342-351 defines `pub struct GraphTransform { pub id, pub package: ValidatedPackage, … }`, a **struct, not a trait**, and `TransformedImporter` holds `transforms: Vec<GraphTransform>` (concrete). There is no `Transform` trait to implement; the only way to add a non-tvix transform is a code change in this repo. B1 and A1 both ship a public trait.
- *Lean / placement*: one half of the anchor is exceeded, the other is wholly absent — genuinely mid-interval. `score: 3`

### D2 — Run-state location, storage shape, and bound (w 0.14)

`contrast`: "A bounded, addressable run ledger of deltas with periodic keyframes, living outside the single-snapshot swap — not an append-only pile of whole-graph copies beside the snapshot." The instruction asks the judge to *say* whether it is scoring shape or bound; this judge places on the **joint** question, weighted toward the named keyframe element.

**B1 — placement: strictly better than `score_4` on the same axis → 5**
- *Closer to `score_4`* (`struct RunLedger { cap: 512, keyframe_every: 32, runs: VecDeque<RunRecord> }` + `struct RunDelta { added, removed, changed }` + "a full keyframe every 32; ring retention of 512; output admitted as its own revision"): B1:411-419 `RunLedger { records: VecDeque<RunRecord>, deltas: BTreeMap<RunId, Arc<RunDelta>>, delta_bytes: usize, keyframes: BTreeMap<RunId, Arc<Keyframe>>, touched, next_run_id, seq }` with §7.1's 512 records, keyframe every 16, ≤4 keyframes; B1:297-303 `RunDelta { nodes_added, nodes_removed, nodes_changed, edges_added, edges_removed }` of `NodeRef{id,kind}` — ids only, never a node body; `output_revision` on the record.
- *Further from* (beyond the anchor): the anchor bounds only by **count**. B1 bounds deltas by **bytes** as well — "at most 64 MiB of deltas (`JC_RUNS_DELTA_BYTES`, `JC_RUNS_DELTA_BYTES`); when either bound is hit the oldest delta is dropped and its record keeps only its `DeltaStats` counts with `delta_retained = false`" — and validates the retained chain by hash so a delta can never be misapplied: "A retained delta chain is **live-scrubbable** if it links … back to the live graph's hash … deltas that link to neither are dropped".
- *Lean / placement*: same-axis respect = a byte bound on top of the count bound plus hash-linked chain validation, which is what makes the ledger's *addressability* trustworthy rather than nominal. `score: 5`

**A1 — placement: past `score_2`, short of `score_4`, lean toward `score_4` → 4**
- *Closer to `score_4`*: A1:438-443 `RunJournal { budget: JournalBudget, transform_ids, log, inner: Mutex<JournalInner /* records: VecDeque, deltas: VecDeque<Arc<RunDelta>>, keyframes: VecDeque<Keyframe> */, schedule }` with §2.2's bound; A1:423 `Keyframe { run_id, graph: Arc<graph_vcs::Snapshot>, approx_bytes }` — the anchor's own "a full keyframe every 32", so a full-graph keyframe is not itself the shortfall; `revision_before/after` make each run addressable.
- *Further from*: A1:419-422 `RunDelta { run_id, ops: Vec<graph_vcs::GraphOp>, touched, added_idx, changed_idx, approx_bytes }` — `GraphOp::UpsertNode` carries the **whole node**, where the anchor's `RunDelta { added: Vec<NodeId>, removed: Vec<NodeId>, changed: Vec<NodeId> }` is ids only. A1 must buy this back with two extra cutoffs the anchor lacks: a big-run→keyframe switch above 25% of nodes touched, and `nodes_changed = None` above `JC_RUNS_DIFF_MAX_NODES` (500 k).
- *Lean / placement*: evidence sits nearer `score_4` on every element — bounded ✓, delta-shaped ✓, keyframed ✓, outside the swap ✓ ("the live graph is never copied because it is already the `Arc<GraphSnapshot>`") — and short only in how much each delta entry carries. B1's id-only delta and A1's node-carrying delta are the sharpest technical separation between the two leaders. `score: 4`

**C1 — placement: past `score_2`, short of `score_4`, no lean → 3**
- *Closer to `score_4`*: C1:470-482 `RunLedger { source_key, retention: RunRetention, state, store }` / `RunRetention { max_terminal_records: 256, max_age: 30 days, max_bytes: 64 MiB, max_delta_bytes: 8 MiB }` — **four** bound dimensions where the anchor has one; and §7's delta is leaner than the anchor's: "stable IDs, minimal node tombstones (`id`, `type`, and fact hash), changed-property patches … It is O(changes), not O(graph) by construction", and *reversible*, which the anchor's id lists are not.
- *Further from*: the anchor's named third element, **periodic keyframes**, is absent — artifact lacks. C1 has no keyframe tier at all. §10.3: "The run scrubber controls a selected transition from 0.0 to 1.0; it **does not claim to materialize every historical full graph**." §15 Q5 confirms it as a chosen limitation: "the scrubber previews a run transition, not an invented full historical graph."
- *Lean / placement*: C1 exceeds the anchor on delta shape and on bounds by a wide margin and misses the third named element outright. The consequence is functional, not cosmetic: a C1 run tells you *what changed* at run #12 and lets you animate that one delta, but there is no way to see the graph **as it was** at #12 — which is the user's "run history (past)" ask. Neither pole dominates. `score: 3`

### D3 — Package model and tvix feasibility (w 0.09)

`contrast`: "A version-3 package on the existing tvix engine, receiving the graph through the established let-binding idiom and building it in one pass — not a bespoke engine block that reaches outside the evaluator."

**B1 — placement: matches `score_4` → 4**
- *Closer to `score_4`*: B1:95-138 `format_version = 3` / `[parser] engine = "tvix"` / `input = "graph"` / `[[parser.variables]]`; §3.2 binds the graph through `EvaluationBuilder::env` and argues it "is precisely what `eval_seed`'s `let n = …; in (…)` does textually (`tvix-wasm/src/lib.rs:418`)" — verified exact; §3.3's builders are `nodesFromList = ns: builtins.listToAttrs (builtins.map …)`, "never repeated `//` merges". The anchor's "would it finish" element is answered by a real in-VM budget on the verified `observe_execute_op` hook.
- *Further from*: the anchor's ideal has no known hazard; B1 self-flags one inside its own design — §3.3 "`degrees` uses `//` per edge on a flat integer map; at 1M edges that is 1M small merges". That is a cost the score_4 shape does not carry.
- *Lean / placement*: every element of the anchor is met and the budget is a genuine addition on the "would it finish" element, but that addition does not make the document go further *along the contrast axis* beyond the anchor's own content. `score: 4`

**A1 — placement: matches `score_4` → 4**
- *Closer to `score_4`*: A1:260-296 is the anchor almost verbatim — `format_version = 3`, `engine = "tvix"`, `[[parser.variables]]` — and §4.2's rendered expression is the anchor's *literal* mechanism: "`let graph = builtins.fromJSON \"<json, escaped>\";` … `in …`", with §1.1 citing the `eval_seed` line as the precedent. §2.10 confirms one `genList` + one `listToAttrs` and no `foldl'` over `addNode`.
- *Further from*: the anchor's ideal does not pay a whole-graph serialization cost; A1's does, and does not price it. A1 sets its own example at `[limits] nodes = 1000000` and renders that input as a **Nix string literal** parsed by `fromJSON` — an O(graph) render plus an O(graph) parse per run, unaddressed. (B1 and C1 both bind a native value and B1's §3.2 explains at length why the literal path was rejected.)
- *Lean / placement*: the anchor's three named elements are all met literally, and the shortfall is on a feasibility concern the contrast line does not name — so it belongs in §Issues, not in the score. `score: 4`

**C1 — placement: matches `score_4` → 4**
- *Closer to `score_4`*: C1:92-134 `format_version = 3` / `[parser] engine = "tvix" input = "graph"` / `[[parser.variables]]`; §4.2 binds "the top-level `graph` value through `EvaluationBuilder::env`" with the expression `(<expr>) { inherit graph; … }`; §4.4's `graph-iter.nix` takes `{ graph }:` and every builder is "one `builtins.map`/`builtins.filter` and one `builtins.listToAttrs`".
- *Further from*: as with B1, the anchor states the budget nowhere, so the anchor is not exceeded on this axis; and C1's budget mechanism is asserted ("tvix's runtime-observer hook") without the line citation B1 supplies.
- *Lean / placement*: all three anchor elements met, no same-axis respect beyond them. `score: 4`

**D3 is a three-way tie by design (w 0.09, deliberately down-weighted for correlation with D9).**

### D4 — Run lifecycle and terminal outcomes (w 0.14)

`contrast`: "Each terminal path is a recorded run carrying a reason, checked before admission, with the previous graph left intact — not an error that is logged and a flag that is set." Instruction: "Give credit proportional to how many of the four terminal paths are given a real, visible, recorded outcome — not to how much prose is spent on them."

**B1 — placement: strictly better than `score_4` on the same axis → 5**
- *Closer to `score_4`* (`RunStatus { Succeeded{output_revision}, Failed{reason, at_revision} }`, `RunFailure::NodeCap|EdgeCap|Timeout`, "checked **BEFORE** admission", "an empty result is a recorded, addressed state"): B1:399-407 `RunStatus { Queued, Running, Succeeded, Failed{reason: FailureReason}, TimedOut{ops, elapsed_ms, kind: BudgetKind}, Rejected{reason: RejectReason}, Skipped{reason: SkipReason} }`; §5.2's cap check "**before any `GenNode` is materialised**"; §5.3's table marks the snapshot `kept` on all eleven rows. Nine of the four required paths, each with a progress row and a schedule effect.
- *Further from*: the anchor has no user-visible state that is not terminal. B1:467 adds one — "`Suspended` is a *visible* state: `GET /runs/upcoming` reports it with the suspending run id, the Runs panel shows it in the Upcoming row with a **Resume** button … and no further `Skipped` rows are written while suspended", reached from "3 [consecutive budget failures] in a row".
- *Lean / placement*: same-axis respect = a recorded, visible, **resumable** outcome reached from repeated failure, where the anchor's set stops at terminal. Plus the empty case gets both branches designed (default `Failed{EmptyOutput}`, opt-in `allow_empty` with a dedicated §9.4 row). `score: 5`

**A1 — placement: past `score_2`, short of `score_4`, lean toward `score_4` → 4**
- *Closer to `score_4`*: A1:409 `RunOutcome { Succeeded, Failed{error}, TimedOut{ceiling_ms}, Refused{reason}, Skipped{reason: SkipReason} }` with a four-gate table whose every row reads "previous retained"; G3 is the existing `check_caps_against`; Q1: "G1–G4 and throw all return `Err` **before** `snapshot.store`"; wall-clock G4 → `TimedOut`; empty is a labelled UI state. A1 also adds a check the anchor has no slot for — G2 forces the list spine with `builtins.length` "**before** `toJSON` materialises a multi-hundred-MB string".
- *Further from*: the anchor's `RunFailure::NodeCap { got, max }` is a **typed** failure; A1:409 carries `Failed { error: String }` and `Refused { reason: String }` — free text. The number the operator needs is in the message string, not in the type. (B1: `Rejected{reason: RejectReason}`; C1: `Failed{kind: RunFailureKind, stage: RunPhase, message}`.) And A1 **publishes** an empty graph by default where the anchor's phrasing — "an empty result is a recorded, addressed state, not a blank canvas" — and C1's explicit reading both refuse to blank the view.
- *Lean / placement*: the full four-gate structure, the before-admission check, the hang→`TimedOut` conversion and the recorded empty are all present and evidenced; the single shortfall is that failures are strings rather than typed reasons. Nearer `score_4`. `score: 4`

**C1 — placement: strictly better than `score_4` on the same axis → 5**
- *Closer to `score_4`*: C1:510-518 `RunOutcome { Published{summary,stats}, Unchanged{revision,stats}, EmptyGraph, Failed{kind: RunFailureKind, stage: RunPhase, message}, TimedOut{limit_ms}, RolledBack{cause_run_id}, Skipped{reason} }` — the anchor's enum plus typed kind and stage; §6's "Node cap exceeded: stop decoding at `limit + 1`" is checked before **materialization**, strictly earlier than the anchor's "before admission"; every case retains the previous snapshot.
- *Further from*: the anchor enumerates no failure of a *later* transform in a chain, because it does not contemplate one. C1:383-384 handles it as a first-class outcome: "If any transform fails, times out, or returns an empty graph, discard the whole working chain. Earlier staged records become **`RolledBack { cause_run_id }`**; no partial graph is published." C1 also records `Unchanged` as its own outcome and recovers an interrupted run as `Failed{kind: ProcessInterrupted}`.
- *Lean / placement*: same-axis respect = a recorded rollback outcome for the case where a *later* transform fails — precisely the property the `score_2` "log line and a flag" pole fails on — plus streaming caps checked before decode rather than before admission. `score: 5`

### D5 — Scheduling model and the upcoming-runs surface (w 0.09)

`contrast`: "A serialisable plan evaluated by the existing poll tick, with coalesce and no-overlap semantics written down — not a timestamp recomputed from the current clock."

**B1 — placement: strictly better than `score_4` on the same axis → 5**
- *Closer to `score_4`* (`enum RunPlan { Manual, Every(Duration) }` declared in the package, survives restart; existing `spawn_poll`; `MissedTickBehavior::Skip`; no overlap; UI renders the same plan): B1:353-360 `Trigger { AfterImport, Every { interval_ms }, Daily { at_utc }, Manual }` in the catalog's `TransformBinding`, `#[derive(Serialize, Deserialize)]`; §4.5 `next_due_at_ms … // from the ledger on boot`; §6.1 gives Fires / **Missed interval** / **Overlap** for all four; §6.2 adds one `select!` arm; §9.1 renders `next_due_at_ms − now_ms_server` with `now_ms` returned by the server "so the client computes countdowns from server time".
- *Further from*: the anchor's model has two triggers and no ledger-recorded consequences for missing one. B1's `Daily { at_utc }` is a **third** trigger whose miss is *recorded*: "Beyond the grace: a `Skipped { MissedWindow }` record is written (so the gap is visible in the ledger)". B1 also closes the anchor's unstated hole — the `WatchPlan::Static` source: "`spawn_with_gate` now returns a driver task that has **only** the runs arm (plus the manual-trigger channel) — otherwise it keeps returning `None`."
- *Lean / placement*: same-axis respect = a recorded skip for a missed window and a driver case for static sources, both making "upcoming" truthful where the anchor's `Every` alone cannot. `score: 5`

**A1 — placement: native 3 (past `score_2`, short of `score_4`, lean toward `score_2` on the serialisability half) → capped at 2 by CK-013**
- *Closer to `score_2`* (`"upcoming": [next_run_at(now + 3000ms), …]`, "nothing to serialise, and the list is recomputed on every render"): A1:432 `RunSchedule { driver, interval_ms, next_due_ms: AtomicU64, last_checked_ms: AtomicU64 }`, with §4.7's rule "`Poll { interval_ms }` \| `spawn_poll` after every `tick()`: `now + interval_ms`", Q5's "`next_due_ms` is written only by the driver that owns the timer", `Push`/`Static` publishing `0`/nothing, and §4.6.3 persisting "**Records only**". A1:464 `ImporterTransformBinding { package, variables, transform_id }` has **no trigger, interval, or plan field at all** — a transform cannot be scheduled independently.
- *Further from* (`score_2`'s "no miss policy, no overlap policy"): A1:124-127 states all three — "`Missed intervals: skip, never replay`", "`Filesystem bursts: coalesce`", "`Overlap: never`" with a run slot, `Skipped{InFlight}`, and a `pending_change` + "exactly one coalesced follow-up" repair it found itself. On the semantics half it is decisively past `score_2` and near `score_4`.
- *Lean / placement*: the contrast leads with **"A serialisable plan"**; A1 has the existing tick and full semantics but neither a declared per-transform plan nor restart survival. That is the `score_2` half of the pole, matched by the `AtomicU64` clock projection. Native 3; **CK-013's `caps D5 at 2` binds** and the reported score is 2. This cap costs A1 more than any other cap costs any document (0.09 × 3 = 0.27, the single largest gap contributor in the whole comparison). `score: 2`

**C1 — placement: strictly better than `score_4` on the same axis → 5**
- *Closer to `score_4`*: C1:409-416 `ImporterTransformBinding { id, package, variables, run_on_source_refresh, watch: WatchPlan, budget }` — a **declared, serialisable, per-transform** `WatchPlan` in the catalog; §8.1's "`ImporterDescriptor.watch` becomes `watches: Vec<WatchPlan>` … `TransformedImporter` merges and deduplicates the upstream plan with transform-owned `Static`, `Poll`, and `Push` plans", evaluated inside the one existing watcher task; §7's `checkpoint.json` "preserves the next sequence and **schedule anchors**"; §8.2's restart rule, `Skip`, drop-while-busy, coalesce-once, and no-self-overlap.
- *Further from*: the anchor's `RunPlan { Manual, Every(Duration) }` has no answer for an event-driven plan. C1:571-576 does, and makes truthfulness the rule: "Poll: an exact `due_at_ms`. Source refresh: \"on next source change,\" **with no invented timestamp**. Push: \"waiting for trigger.\" Static after initial application: no upcoming row." Phase 3's kill condition is "upcoming time is inferred rather than **host-provided**".
- *Lean / placement*: same-axis respect = a truthful, non-invented upcoming row for the three plan kinds the anchor cannot express, backed by a persisted anchor and a drift-free re-arm. `score: 5`

### D6 — Serving and animation integration (w 0.08)

`contrast`: "Frame cost proportional to what changed, patched into the existing buffers under the existing revision guard — not a whole-scene refetch and rebuild per tick."

**B1 — placement: native 3 (past `score_2`, short of `score_4`) → capped at 2 by CK-019**
- *Closer to `score_2`*: B1's endpoint set is justified on the axis's *other* half. `GET /runs` is justified with "The Runs panel's history list" and `GET /runs/upcoming` with "Separate from `/runs` because it is configuration + schedule state" — a "Why it exists" column, not the required "why no existing route fits", for 4 of 7 routes; the existing set is cited only as `api_routes()`, and `/progress?since=<seq>`'s cursor is never engaged.
- *Further from* (`score_2`'s "re-fetch … and rebuild every GPU buffer from scratch"): B1:566 "**no fetch needed for the live case**"; the transition writes a *subset* of the sizes buffer via a writer "beside `update_sizes`, `pipelines.rs:1270`" (verified exact) and pulses colours; scrubbing applies `RunDelta::invert` "one run per step through `begin_transition`". No per-tick scene rebuild anywhere. But it names 1 of the anchor's 4 APIs, invents `write_sizes_subset` rather than reusing `set_positions`/`set_selected`, and never names the revision guard.
- *Lean / placement*: native 3 — the frame-cost half is sound and the endpoint-justification half is not. **CK-019's `caps D6 at 2` binds**; reported score 2. `score: 2`

**A1 — placement: past `score_2`, short of `score_4`, lean toward `score_4` → 4**
- *Closer to `score_2`*: nothing on this axis. A1 refetches `/runs?limit=1` and one delta and patches two buffers; it adds no bulk numeric endpoint and reuses the existing `run` group on `GET /progress`.
- *Further from* (`score_4`: all four APIs, the revision guard, "only a topology change reloads", run events on `/progress?since`): §4.9.3 — headed "**no remount beyond the one the app already does**" — writes via `pipes.update_sizes` (`pipelines.rs:1270-1285`) and §4.9.4 via `pipes.update_colors` (`pipelines.rs:1231-1245`); both line references verified exact. Topology vs. patch is stated from the failure side in §1.3 gap 8 ("Removed nodes are absent from the live buffers, so \"dim removed nodes via `set_selected`\" cannot work on the latest run") and in §4.9.3 ("Removed nodes are not in the buffers; the panel reports the count"). Short of the anchor: `set_positions` and `set_selected` are not named, the revision guard is not cited, and `/progress?since=<seq>`'s cursor is used only as "the `run` progress group".
- *Lean / placement*: two of four APIs named with verified lines, the existing `reload_graph` retained as the admission path, and the only document that explains *why* a live topology change cannot be a patch. Nearer `score_4`. `score: 4`

**C1 — placement: native 4 (matches `score_4`) → capped at 2 by CK-019**
- *Closer to `score_4`*: C1:645 "Internally they use `mount_canvas` for the union topology and `GraphPipelines::set_positions`, `update_colors`, `update_edge_colors`, and `update_sizes` for frames" — **all four verified to exist** (`pipelines.rs:1743 / 1231 / 1250 / 1270`) — and "The revision poll in `main.rs` continues to call `reload_graph` after real publication", i.e. the per-frame path never touches the network. `set_selected_node` and `set_search_highlights` are reapplied after each union mount, preserving semantics.
- *Further from* (the anchor's "`/progress?since=<seq>` stream" element, and the endpoint-justification half): **artifact lacks** — C1 never mentions `/progress` at all, and its §9 justifications argue against a category ("not a bulk numeric buffer or protobuf node payload", without naming those routes) or against its own route, for all three endpoints.
- *Lean / placement*: on the frame-cost axis proper C1 is the **strongest document in the field** and would score 4 uncapped. The cap is triggered by a sub-clause about endpoint justification, which is orthogonal to frame cost; the spec's cap is dimension-local and mechanical, so it binds. **Reported 2, with the note that this score misrepresents C1's actual animation design.** `score: 2`

### D7 — Node content depth and cross-source generality (w 0.14)

`contrast`: "New fields on the shared node metadata every importer serves, including the generated-graph path that currently discards Nix metadata — not a panel that appears for one source." Anchor adds: "three fields that are always populated beat eight that are empty for most nodes" and "prost types regen via `just app-proto`".

**B1 — placement: native 3 (past `score_2`, short of `score_4`) → capped at 3 by CK-030**
- *Closer to `score_2`* (a section gated on a source prefix): B1:540 extends `GET /node/*id` and Phase 4 accepts only when "a pest node, a tvix node, and an Obsidian node show the same four sections with **no per-source branch** in `inspector.rs`". Artifact lacks: nothing on the source-generality half.
- *Further from*: the anchor's `first_seen_at`/`last_seen_at` spine and its regen step. B1's provenance is run-id-based with no first/last-seen timestamps, and the proto addition is described only as "additive protobuf fields" — **no field numbers, no `app/ui/src/proto/`, no `just app-proto`** (grep: 0 hits). It does add a bounded `touched` index as the source of `history` with a kill criterion ("if the touched index exceeds 10 % of snapshot memory … restrict it to the newest 16 runs").
- *Lean / placement*: native 3 — strong on depth (`properties` resolved from declared `[[schema.fields]]` "by name and typed by `DiscoveryFieldType`; a value of the wrong type is a projection error, never coerced silently") and on breadth, short of the anchor's regen step and its timestamp spine. **CK-030's `caps D7 at 3` binds**; reported 3. `score: 3`

**A1 — placement: strictly better than `score_4` on the same axis → 5**
- *Closer to `score_4`*: A1:481-485 `message NodeMeta { /* existing 1-28 … */ repeated NodeFact facts = 29; Provenance provenance = 30; repeated uint64 touched_runs = 31; }` — "existing 1-28" verified exactly (`bool anomaly_flag = 28;`), next free numbers verified exactly. `Provenance { source_id, transform_id, first_seen_run, last_changed_run, change, history_bounded }` *is* the anchor's `first_seen_run`/`changed_in` spine. "Regenerated into `app/ui/src/proto/` via **`just app-proto`**". Generated path: "`frontmatter` = `metadata` minus the promoted keys … the Inspector's chip walker and leftover grid light up for generated and transformed graphs with **zero new UI**". Every importer gains it, because Facts iterate the source-neutral `snap.schema.fields`.
- *Further from*: the anchor's `Provenance` is a static field; A1's carries a **completeness flag** — "`bool history_bounded = 6; // true when the retained deltas do not reach the node's first appearance`" — and a live `change: "unchanged" | "added" | "changed"` relative to the latest run. A field that is always populated **and says when it is incomplete** is deeper on the "always populated beats empty" axis than a field that is always populated and quietly truncated. A1's Phase 0 kill is also a depth test, not a threshold: "if for Obsidian ≥ 90 % of Facts rows duplicate the Inspector frontmatter grid, merge Facts into the Inspector's grid header instead of a second section."
- *Lean / placement*: same-axis respect = the `history_bounded` honesty flag plus `touched_runs` as a first-class repeated field. `score: 5`

**C1 — placement: native 3 (past `score_2`, short of `score_4`) → capped at 3 by CK-030**
- *Closer to `score_2`*: nothing. C1 adds a field on the shared `NodeMeta`, not a source-gated section.
- *Further from*: §11.2 `NodeFacts` is **one implementation called by both** `nodes::focused_node` and `inspector::node_view` — "It **reuses/moves, rather than duplicates**" — which is the strongest cross-source guarantee of the three, and `NodeLineage { first_seen_revision, last_changed_run_id, last_changed_transform_id }` is revision-keyed, so it survives log truncation where a run-keyed first-seen would not. But the anchor's `changed_in: Vec<RunId>` has **no equivalent**: C1 records only the *last* changed run, so a node touched by #12, #15 and #41 shows #41 alone. C1 explains the storage reason — "Lineage lives once on the current node; it is not a separate per-run node copy" — which is the correct call that produces the shallower field. And the regen command is not named.
- *Lean / placement*: native 3 — best-in-class on the cross-source half the contrast leads with, materially shallower on the depth half the anchor's `changed_in` names. **CK-030's `caps D7 at 3` binds**; reported 3. `score: 3`

### D8 — Impeccable surface discipline (w 0.08)

`contrast`: "A named reduced-motion equivalent and a store-shaped loading path for the new surfaces — not motion and loading left to implementation." The `score_4` anchor's four elements are: (1) app.css reduced-motion → stepped equivalent, (2) `loading_store` + `LoadingGate`, (3) measured % else explicitly indeterminate, (4) styling in `app.css` + `panel_kit::CSS` on the existing palette. D8's description additionally asks about ARIA roles and hierarchy (`AGENTS.md:199`, verified).

**B1 — placement: strictly better than `score_4` on the same axis → 5**
- *Closer to `score_4`*: §9.5 `crate::motion::prefers_reduced()` with the CSS rule beside the existing `app.css:62-64`; §9.6 two `loading_store`s + `LoadingGate` + `GlobalLoadingBar`; §9.7 "`Evaluating` is **indeterminate** (`fraction: None`)"; §9.1 `panel_kit::badge` "colours by status"; §9.4 `role="alert"` on failure rows.
- *Further from*: the anchor reduces motion to a stepped scrubber. B1 gives the same run state a **second modality**: "added and changed nodes are then held in the existing highlight set … for 2 s so the *set* that changed is legible without any motion; the Runs panel's selected-run section lists the same ids as **chips** so the information is also available as **text**." And its motion query is re-read live — "re-read on the media query's `change` event" — where A1 reads "once at boot".
- *Lean / placement*: same-axis respect = a non-motion, text-borne equivalent plus a change-tracking query. `score: 5`

**A1 — placement: strictly better than `score_4` on the same axis → 5**
- *Closer to `score_4`*: §2.8 + §4.9.4 give the reduced-motion variant (static `push_focus_set` for 4 s, playback disabled, fixed alpha, CSS media rule at the `:62-64` precedent); §2.5 gives two `loading_store`s + `LoadingGate` + `GlobalLoadingBar` with "No hand-rolled `loaded` signals"; §2.9 gives indeterminate-for-eval and a server/client clock mapping cited to `progress.rs:17-21`; styling reuses `badges::frontmatter_chips` and the live community colour.
- *Further from*: the anchor's four elements include none about roles, and D8's description asks for them. A1:574 supplies a complete contract: "the strip is `role=\"listbox\"` with `aria-activedescendant`; markers are `role=\"option\"`; ◀ ▶ are buttons with labels; the countdown is `aria-live=\"polite\"` updated at 1 Hz." B1 and C1 supply no roles at all.
- *Lean / placement*: same-axis respect = an explicit ARIA role/hierarchy contract where the anchor and the other two documents have none. `score: 5`

**C1 — placement: past `score_2`, short of `score_4`, no lean → 3**
- *Closer to `score_2`* ("motion and loading left to implementation"; "node detail: `Spinner` reused as-is"; "hand-rolled `let mut loaded`"): C1 designs both in — §10.2 gives literal `loading_store`/`begin`/`update`/`succeed`/`fail` code, `LoadingGate`, `GlobalLoadingBar`; §10.4 gives a full reduced-motion variant. Artifact lacks: nothing on this half.
- *Further from*: the anchor's **element (4)**, "styling stays in app.css + `panel_kit::CSS` on the existing palette". C1 introduces a fresh visual vocabulary — red/amber/neutral outcome badges, five marker shapes (filled, red, red hollow, amber, hollow, grey tick, ◐), an alpha convention — and never says where it lives; `app.css` appears once, in §10.4's reduced-motion sentence. It also states **no ARIA roles at all** for a drag-based scrubber, which `AGENTS.md:199` names directly.
- *Lean / placement*: three of four anchor elements met, the fourth absent; the ARIA gap is on a *different* axis than the contrast and is deliberately not what holds it here — on the contrast line alone C1 would read 4. Native 3, and **CK-031's `caps D8 at 3` binds** without changing it. `score: 3`

### D9 — Landmine risk accounting and groundedness (w 0.05)

`contrast`: "This repository's documented landmines, each paired with a countermeasure that exists in this codebase — not generic worry words." Instruction: "the discriminating question is truthfulness, not volume. **Confident but false claims about the tree score at or below `score_2`; admitted uncertainty scores above a document that asserts something wrong.**"

Landmines checked for all three: no fuel/step limit + inline CPU-bound eval; O(n²) attrset-copy `addNode`; closures across 3+ imports; the rejected `[[schema.fields]]` on tvix; the single-snapshot `ArcSwap`; the pure evaluator; the 1M/4M caps. **B1 7/7, A1 7/7, C1 6/7** (C1 never engages the `[[schema.fields]]` restriction — it does not need it, since its node facts read `NodeMeta.frontmatter` directly rather than declared schema fields; an omission, not an error).

**A1 — placement: strictly better than `score_4` on the same axis → 5**
- *Closer to `score_4`* (all three named landmines → concrete in-tree countermeasures): §2.1 "tvix has no fuel or preemption (`lib.rs:25-31`)" → four gates + orphan slot; §2.10 "never a `foldl'` over `addNode` (O(n²`)" → `genList`+`listToAttrs`, "the `soupGen` pattern"; §2.10 `{ graph }` "exactly like `graph-combinators.nix:12`". Every spot-checked citation is exact, and the proto field numbers 29/30/31 against an existing max of 28 are the single most precise citation in the field.
- *Further from*: the anchor asserts; it never corrects itself. A1 corrects itself twice **in the document**: Q3 "the first draft claimed `validate_local_id` rejects `:`; it does not. **Corrected**", and Q2 "the first draft's \"2 keyframes\" at 1 M nodes ≈ 2 × ~500 MB — **not acceptable**. **Fixed** with `JC_RUNS_KEYFRAME_BYTES`". Q1 also admits a hard limit: "a non-terminating expression that allocates without bound cannot be stopped in-process."
- *Lean / placement*: the instruction's test is truthfulness against confident assertion; A1 is the only document whose *stated* record of uncertainty actually beats a false assertion, because it demonstrates one twice. Same-axis respect. `score: 5`

**B1 — placement: matches `score_4` → 4**
- *Closer to `score_4`*: all seven landmines paired with in-tree countermeasures, and the citations were checked and hold: `observe_execute_op` at `observer.rs:42-76` (exact — the default no-op trait method), `eval_seed` at `lib.rs:418` (exact), `graph.nix:4-6` (exact), `graph.nix:143-149` (exact — `toGraphJSON` does emit node and edge metadata), `graph-combinators.nix:3-12` (exact), `watcher.rs:283` `MissedTickBehavior::Skip` (exact), `pipelines.rs:1270` = `update_sizes` (exact), `tvix.rs:293-307` schema rejection (exact). No wrong cap value anywhere.
- *Further from* (truthfulness): §3.1's worked manifest declares `[[schema.fields]]` as `name = "weight"` / `type = "number"`. `DiscoveryField` (`crates/data-loader/src/lib.rs:57-69`) is `#[serde(deny_unknown_fields)]` over `key`, `field_type`, `required`, `searchable`, `facetable`, `snippet`, `boost`. There is no `name` and no `type`; the example package **would be rejected by its own design**. Stated as a working example, with no hedge, and §14 restates the feature confidently ("Declared `[[schema.fields]]` allowed and typed"). Separately, §13 **admits** a real risk — "a `Drop` impl in tvix aborting on unwind; that is what phase 2's kill criterion covers" — which is the behaviour the instruction rewards.
- *Lean / placement*: substance matches the anchor on all seven landmines and 15+ verified citations; one confidently-stated false field-name pair in an example keeps it off the level A1 reaches. `score: 4`

**C1 — placement: matches `score_4` → 4**
- *Closer to `score_4`*: six of seven landmines with in-tree countermeasures — the pure evaluator and embedded VFS (§4.2, citing `lib.rs:85-120` for the VFS header, exact), the O(n²) `addNode` (`graph.nix:16-50`, exact), closures across 3+ imports (§4.4), the `ArcSwap` (§5.2), the 1M/4M caps (§6, "Native policy defaults remain `JC_MAX_NODES = 1_000_000` and `JC_MAX_EDGES = 4_000_000`"), no fuel + inline eval (the `BudgetObserver` + watchdog). Every spot-checked citation is exact, including the two hardest (`lib.rs:219-252` covering `GenNode`/`RawNode`/`RawEdge`; `data-loader:781-795 / 957-970 / 972-1120` covering `ImportOutcome`/`WatchPlan`/`ImportProgress`/`Importer`).
- *Further from*: the seventh landmine — the tvix engine's rejection of `[[schema.fields]]` (`tvix.rs:293-307`) — is never engaged, and the anchor's pairing is "**each** landmine". C1's silence is defensible (its design does not declare schema fields) but leaves one listed hazard unreconciled, and its `BudgetObserver` is asserted without the line citation B1 supplies for the same hook.
- *Lean / placement*: no false claim, six-for-six on the hazards it needs, one hazard unreconciled. `score: 4`

### D10 — Phase slicing and kill criteria (w 0.05)

`contrast`: "Phases that each land on an existing seam and are usable the day they merge, each with a stated kill criterion — not one phase that contains the whole feature." Instruction: "judged on what each phase demonstrably does, not on how many phases there are."

**B1 — placement: strictly better than `score_4` on the same axis → 5**
- *Closer to `score_4`*: §12, Phases 0-5, each with **Ships** / **Acceptance** / **Kill**, each landing on a named seam (`/node/*id` + Inspector; `input="graph"` + the after-import chain; `spawn_blocking` + the disk store; `graph_canvas::load` + `render::begin_transition`; `proto::NodeMeta`; the variables route).
- *Further from*: the anchor's kills are bare parentheticals ("kill: no bounded delta"). B1's kills each name a **degraded mode to fall back to**: "if the unwind-based budget proves unreliable on the pinned tvix rev … fall back to fuel-only with `wall_ms` enforced by `tokio::time::timeout` plus a hard cap of one orphaned evaluation per source"; "if the hold scene remount is judged too disruptive by users … keep the strip and transition, disable scrubbing to non-live runs, and ship \"compare with #N\" as a highlight set instead." And the acceptances are the testable form of the user's success criterion: "after a run that changes 50 of 10k nodes, the other 9 950 do not move (**positions_cpu diff < 1 wu**)"; "no `tokio` task count growth over 1 000 runs"; "a `while true` expression under `wallMs = 2000` records `TimedOut` within 2.5 s".
- *Lean / placement*: same-axis respect = every kill names a surviving degraded mode, so a phase can be abandoned without losing the feature, and the acceptances are measurable rather than descriptive. `score: 5`

**A1 — placement: strictly better than `score_4` on the same axis → 5**
- *Closer to `score_4`*: §5, Phases 0-4, each with **Lands** / **Seam** / **Acceptance** / **Kill**. A1 is the only document that makes **Seam** a required per-phase field and backs it with the S1-S11 dependency table + DAG, which is what makes "an explicit dependency on the types an earlier phase introduces" checkable rather than asserted.
- *Further from*: two things the anchor's five lines do not have. First, ordering: A1 puts **node content in Phase 0**, not last — §7: "Node content phase last \| Node content is Phase 0 \| Approach-independent, smallest, immediately useful". The user's most concrete complaint (clicking a node shows little) is thus fixed in the first merge, and §12's `graphs` chain is not the reason B1 defers it to Phase 4. Second, an assertion with a value: "a 1 000-run soak in a `runs.rs` unit test … holds `records ≤ 256` and `delta bytes ≤ 64 MiB`", and a kill with a number: "if `diff_graphs` p95 … exceeds 2 s, lower `JC_RUNS_DIFF_MAX_NODES` default to 100 000". Also honest: Phase 4's kill is "**none** (documentation and read-only surfaces)".
- *Lean / placement*: same-axis respect = the required seam field plus a dependency graph that makes the slicing auditable, and node-content-first ordering. `score: 5`

**C1 — placement: matches `score_4` → 4**
- *Closer to `score_4`*: §14, Phases 1-4, each with **User-visible value** / **Work** / **Acceptance** / **Kill criterion**, and each independently shippable on a named seam: P1 = `/node/*id` + the Nodes/Inspector surface (alone fixes the reported symptom for every source); P2 = the importer chain (manual decorator runs, budgets, atomic publication, bounded ledger); P3 = the watcher; P4 = the renderer. The cutover order in P3 — deliver manual first, schedule second, with `watch` → `watches` as an explicit "one clean cutover" — is the right dependency shape.
- *Further from*: the anchor's phases each name a kill; C1's kill criteria are **stricter in kind** — expressed as invariants rather than thresholds ("do not enable transformation bindings if … **any terminal failure changes the revision**, ledger growth is unbounded, or **a source-specific branch is required**"; "stop rollout if … **upcoming time is inferred rather than host-provided**"; "do not ship animation if it … **leaves the canvas on a historical preview** … or fabricates progress"). That is a real strength. But no phase names a **seam** as a field (it is woven into "Work"), and **no kill names a fallback** — every C1 kill is a stop, where B1's and A1's are degradations. Coarser cut: 4 phases carry the same ground as the anchor's 5.
- *Lean / placement*: the requirement is fully met — each phase demonstrably lands a usable slice with a stated kill — and the kill quality exceeds the anchor's; what is missing is the explicit seam and the degraded mode, so it matches rather than exceeds. `score: 4`

---

## 3. Totals

`total = Σ(weight × score)`, no division, weights sum to 1.00. No global cap.

| Dim | w | B1 | A1 | C1 |
|---|---|---|---|---|
| D1 Core abstraction | 0.14 | **5** (0.700) | 4 (0.560) | 3 (0.420) |
| D2 Run-state model | 0.14 | **5** (0.700) | 4 (0.560) | 3 (0.420) |
| D3 Package + tvix | 0.09 | 4 (0.360) | 4 (0.360) | 4 (0.360) |
| D4 Run lifecycle | 0.14 | **5** (0.700) | 4 (0.560) | **5** (0.700) |
| D5 Scheduling | 0.09 | **5** (0.450) | 2 (0.180) ⛔ | **5** (0.450) |
| D6 Serving/animation | 0.08 | 2 (0.160) ⛔ | **4** (0.320) | 2 (0.160) ⛔ |
| D7 Node content | 0.14 | 3 (0.420) ⛔ | **5** (0.700) | 3 (0.420) ⛔ |
| D8 Impeccable | 0.08 | **5** (0.400) | **5** (0.400) | 3 (0.240) ⛔ |
| D9 Landmines | 0.05 | 4 (0.200) | **5** (0.250) | 4 (0.200) |
| D10 Phasing | 0.05 | **5** (0.250) | **5** (0.250) | 4 (0.200) |
| **TOTAL** | 1.00 | **4.34** | **4.14** | **3.57** |

⛔ = a dimension-local cap from a checklist NO. Every cap's native placement is recorded above.

---

## 4. Comparative analysis

### B1 (4.34) — the run-state design and the run-contract design

**Excels at:** the ledger (D2, 5) and the transform contract (D1, 5). `RunLedger` bounds deltas by **bytes as well as count**, degrades honestly (`delta_retained = false`), and validates the retained chain by `graph_hash` so a delta can never be misapplied to the wrong graph — "nothing is misapplied to the wrong graph, and the panel says which runs can be scrubbed". The `Transform` trait is public and wasm-clean, and its `RunCtx` hands an external implementer the tree's own `ImportProgress` and an explicit budget. D4 (5) and D5 (5) follow: eleven terminal paths each with a snapshot column and a schedule effect, a visible resumable `Suspended` state, four triggers with a per-trigger miss/overlap table, and a recorded `Skipped{MissedWindow}` for a missed daily window.

**Where it loses:** **D7 (3, capped by CK-030)** — it never names `just app-proto`, so its four new `NodeMeta` fields are, by the spec's own rationale, "invisible in the UI … the exact class of 'node has no info' bug the user reported, re-introduced by the fix". **D6 (2, capped by CK-019)** despite a sound frame-cost design. And the one D9 defect: a `[[schema.fields]]` example using `name`/`type`, which `DiscoveryField`'s `deny_unknown_fields` rejects — the example package B1 ships would not validate.

**Single change that would raise it most:** add `just app-proto` to §8/§12 Phase 4 and give the new `proto::NodeMeta` fields explicit numbers. That alone lifts D7 from 3 to 4 (+0.14) and removes one of two caps. — *Run-state home:* a bounded `RunLedger` in `AppState` beside the `ArcSwap<GraphSnapshot>`, keyed by transform, with a trigger per transform. — *Non-omp example:* `ContractChains` on a 1M-node Kubernetes fixture.

### A1 (4.14) — the node-content design and the run-as-rebuild unit

**Excels at:** **D7 (5)** — the only document that satisfies CK-030 end to end, with `facts = 29 / provenance = 30 / touched_runs = 31` verified against an existing max of 28 and `just app-proto` named; `history_bounded` makes the field say when its own history is incomplete; Phase 0 puts node content **first**, so the reported symptom is fixed in the first merge. **D8 (5)** — the only explicit ARIA contract in the field. **D9 (5)** — the most precisely cited document, and the only one that corrects two of its own false claims in place. **D10 (5)** — the only required per-phase *Seam* field plus an S1-S11 dependency DAG.

**Where it loses:** **D5 (2, capped by CK-013)** — this is the single most consequential finding in the comparison and the largest single contributor to the B1–A1 gap (+0.27). A transform in A1 has **no schedule of its own**: `ImporterTransformBinding` has no trigger or interval field, `next_due_ms` is an `AtomicU64` written post-tick (= `now + k*interval`, recomputed at read time, never persisted), and `Push`/`Static` publish no upcoming row at all. The user's explicit "upcoming / past **scheduled** runs" ask is only half met — you see when the *source* will next reload, not when a *transform* will next run. Also D4 (4): failures are untyped strings where the anchor wants `NodeCap { got, max }`, and A1 publishes an empty graph where B1 and C1 both refuse to. Also D2 (4): `RunDelta.ops: Vec<GraphOp>` carries **whole changed nodes** where the anchor's id-lists and B1's `NodeRef` carry only ids.

**Single change that would raise it most:** add a `trigger: Trigger` field to `ImporterTransformBinding` with `Every`/`Daily`/`Manual`, persist the anchor in the `runs.jsonl` checkpoint, and have `GET /runs/upcoming` render it. That lifts D5 from 2 to 5 (+0.27) and would put A1 at 4.41 — first. — *Run-state home:* an in-memory bounded `RunJournal` in the per-source `AppState`, with a **records-only** `runs.jsonl`; the run unit is the **rebuild**, not the transform. — *Non-omp example:* `collapse-by-facet.toml`, "meaningful for Kubernetes namespaces, Obsidian folders, generated cluster types".

### C1 (3.57) — the chain atomicity and the durable ledger

**Excels at:** **D4 (5)** — the best terminal-outcome model of the three. `RunOutcome` is typed (`kind: RunFailureKind`, `stage: RunPhase`), the caps are enforced by "a streaming `serde` visitor **before vectors are fully materialized**" (strictly earlier than the anchor's "before admission"), and it is the only document with a recorded answer for *a later transform in a chain failing*: `RolledBack { cause_run_id }` with "no partial graph is published". Plus `Unchanged` as a recorded outcome and `ProcessInterrupted` recovery. **D5 (5)** — the only document with a **declared, serialisable, per-transform `watch: WatchPlan`** in the catalog, persisted via `checkpoint.json`'s "schedule anchors", plus the truthful non-invented upcoming rows ("with no invented timestamp") the anchor's `Every`-only model cannot express. **D7's cross-source half** — `NodeFacts` is *moved* out of `inspector::node_view` and called by both surfaces, which is a stronger guarantee than "the same sections render for each source".

**Where it loses:** **D1 (3)** — `GraphTransform` is a **struct, not a trait**. There is no extension point: a stranger cannot implement a non-tvix transform without forking, and there is no escape hatch for graphs too large for the Nix tier (B1 ships `Builtin::{LabelCommunities, PruneDegree, ContractChains}` as packages; A1 ships a public trait). **D2 (3)** — the contrast names "deltas **with periodic keyframes**" and C1 has no keyframe tier at all. This is the functional reason C1 places third: you can see *what changed* at run #12 and animate that one delta, but **not the graph as it was** at #12, which is the user's "run history (past)" ask. **D6 (2, capped by CK-019)** — and this cap is the most misleading number in the report, because C1 is the **strongest of the three on frame cost**: it is the only document naming all four verified buffer APIs. **D8 (3)** — no ARIA roles, no styling placement. **D7 (3, capped)** — `NodeLineage` records only the *last* changed run, so the anchor's `changed_in: Vec<RunId>` has no equivalent.

**Single change that would raise it most:** add a bounded keyframe tier (`node ids + kinds + edge index pairs`, every 32 runs, retained ×2, byte-gated) so past runs are viewable and not merely animatable. That lifts D2 from 3 to 4–5 (+0.14 to +0.28). — *Run-state home:* a **source-owned** `RunLedger` reached through the decorator's `recorder: Arc<dyn TransformRunRecorder>`, durably persisted at `run-ledger/v1/<encoded-source-key>/{ledger.jsonl, checkpoint.json, deltas/*.cbor.zst}`. — *Non-omp example:* one annotation package run "unchanged against markdown, Kubernetes, and generated fixtures".

---

## 5. Ranked vote

**1. B1 — 4.34** · **2. A1 — 4.14** · **3. C1 — 3.57**

No tie; the spec's D2→D4→D7→D1 tie-break chain is not invoked. B1 leads on D1, D2, D4, D5; A1 leads on D6, D7, D9 and ties B1 on D3, D8, D10.

**The gap decomposition (B1 − A1 = +0.20):** D5 +0.27 (A1's CK-013 cap), D1 +0.14, D2 +0.14, D4 +0.14, against D6 −0.16, D7 −0.28, D9 −0.05. A single CK regrade moves the order: if CK-019 were read YES for B1, B1 → 4.42 (still first). If CK-019 were read YES for C1, C1 → 3.73 (still third). **The ranking is robust to the single most contestable call in this report.**

**Recommendation for Phase 5:** treat B1 and A1 as co-winners. They lead different dimensions, and B1's two lead dimensions (D1, D2 — 0.28 combined) are precisely the axis the three were selected to differ on, which is why the spec weights D2 at 0.14. C1 is not merely third; it holds two seams neither other has (§7), and it should not be pruned.

---

## 6. Diversity audit

**Verdict: three genuinely distinct run-state homes. The spec's collision rule is not triggered; the ranking is clean, not advisory.**

| | Declared home | Where it physically lives | What a "run" is | What the transform reads | Durability |
|---|---|---|---|---|---|
| **B1** | "a bounded `RunLedger` **beside the `ArcSwap<GraphSnapshot>`** in `AppState`" | `crates/graph-api/src/runs/{ledger,store}.rs`, keyed by transform | a **scheduled transformation** of a graph revision, plus "run zero" for the import | "the **live published snapshot** at the moment it starts" | records + deltas + keyframes → `runs/` under the packages dir, compaction at 1024 lines |
| **A1** | "an in-memory, bounded run journal **inside graph-api's per-source `AppState`**" | `crates/graph-api/src/runs.rs`, keyed by source | the **snapshot rebuild** — "Every source gets runs", `transform_ids` may be empty | the **fresh source import** ("view semantics … never a fold over its own previous output") | **records only** → `<overlay_dir>/runs/<source_id>/runs.jsonl`; deltas and keyframes never written |
| **C1** | a **source-owned** `RunLedger`; "runs belong to the source instance that supplied the graph" | reached through `TransformedImporter.recorder: Arc<dyn TransformRunRecorder>`; graph-api finalizes | a **staged transform evaluation**, committed in two phases | the **latest upstream baseline** — "re-evaluate the complete configured transform chain … from the latest upstream baseline. This makes results deterministic and prevents schedule-dependent compounding" | **full durable ledger** — `ledger.jsonl` + `checkpoint.json` + `deltas/*.cbor.zst`, checksums, atomic compaction |

**Are B1 and A1 phrasings of one design?** No — but the honest answer needs the near-collision stated rather than glossed. Both put the store in graph-api's `AppState`, which is why the spec's taxonomy names "ledger beside the snapshot" and "in-memory journal with the snapshot" as two of three *distinct* homes. Beneath the location, they diverge on the three things that actually determine behaviour: (a) the **unit** — B1's record names the transform and the revision it produced, A1's names the rebuild and carries a `transform_ids: Vec<String>`; (b) the **input** — B1's transform reads the *published* graph, so `every` transforms compose as `g_{t+1} = T(g_t)` and need a stated idempotence rule, while A1's is a *view* recomputed from the source "on every run", so runs are idempotent and drift-free by construction; (c) **durability** — B1 persists deltas, A1 persists records only.

That (b) divergence is a real design fork, not a presentational one: it determines whether a scheduled run can ever be a fold, and it is the reason B1 must add "Packages that are not idempotent … are therefore expected to be `afterImport`, and the note says so" while A1 simply states "State-accumulating transforms are out of scope."

---

## 7. Synthesis seams

Per the spec: "the ONE thing it does that the other two do not, expressed as (a) where its run state lives, (b) which file that lands in, and (c) what it would cost to adopt in isolation."

### Seam 1 — from **B1**: the in-VM budget that actually *terminates* the evaluation (§5.2 `DeadlineObserver`)

The op-counting `RuntimeObserver` on `observe_execute_op` (verified at `observer.rs:76`) + `panic_any(BudgetExceeded)` + `catch_unwind(AssertUnwindSafe(..))` in the spawn_blocking closure. A1 **cannot** do this — it uses a `tokio::time::timeout` and states the consequence plainly: "a non-terminating expression that allocates without bound **cannot be stopped in-process**", and it permanently quarantines the transform's slot afterwards. C1 has a `BudgetObserver` too but without the citation, plus quarantine.

- **(a) Run state:** unchanged — the seam is the *execution* budget, not the store. Lift it into whichever home wins.
- **(b) Files:** `crates/tvix-wasm/src/budget.rs` (new: `EvalBudget`, `DeadlineObserver`) + the blocking closure in `crates/graph-api/src/runs/executor.rs` (B1) or `crates/importer/src/tvix.rs::apply` (A1). One `RuntimeObserver` impl, ~60 lines.
- **(c) Cost alone:** the lowest-cost, highest-value seam in the field. It **un-solves A1's worst property** — with a real budget, A1's G4 never produces an orphan to quarantine, and A1's "an orphan observed to outlive `4 × eval_ms` → move eval to a `graph-api transform-eval` subprocess" kill criterion becomes unnecessary. The only risk is the one B1 itself names: an aborting `Drop` on unwind, pinned to tvix `1dbdd85` and covered by B1's Phase 2 fallback (fuel-only + `timeout` + one-orphan cap).

### Seam 2 — from **A1**: the `facts` / `provenance` / `touched_runs` spine on the shared `NodeMeta` (§4.4.6 + §4.10)

`repeated NodeFact facts = 29; Provenance provenance = 30; repeated uint64 touched_runs = 31;` with `history_bounded`, Facts resolved from `snap.schema.fields` in declared order, served by the **existing** `GET /node/*id`. B1 has `properties`/`provenance`/`edges`/`history` but no regen step; C1 has `NodeLineage` with only `last_changed_run_id` and no regen step. A1 is the only document that satisfies CK-030 at all, and its field numbers are verified exactly right against `anomaly_flag = 28`.

- **(a) Run state:** the journal's per-delta `touched` id set — this is the one seam where run state is *joined to a node*, and it is what makes per-node history possible at all. B1 already carries this structure (`RunLedger.touched`, a `HashMap<String, SmallVec<[(RunId, TouchKind); 4]>>` bounded by delta eviction); C1 has no equivalent.
- **(b) Files:** `crates/graph-api/proto/graph.proto` (3 fields at 29/30/31) + `crates/graph-api/src/server.rs::node_meta` (one pure function) + `app/ui/src/panels/nodes.rs::focused_node` (three sections) + regen via `just app-proto`. ~10 lines in the journal for `touched_runs`.
- **(c) Cost alone:** very low coupling, high value. Every field is a pure function of (snapshot, node, journal) — no new endpoint, no new store, no new state. The `history_bounded` flag travels with it and is the single cheapest way to stop a future document shipping a silently-truncated history panel.

### Seam 3 — from **C1**: the two-phase chain commit with `PublicationMetadata` (§5.2)

`LoadResult.publication: PublicationMetadata { transform_run_ids }` + the protocol *stage → build → swap → finalize*, with `RolledBack { cause_run_id }` when a later transform fails. **Neither leader has an atomicity story for a multi-transform chain:** B1's `afterImport` chain explicitly degrades to partial publication ("A failed transform in the chain records its failure and passes its *input* through unchanged, so one bad transform never blocks the import"), and A1's `TransformedImporter` "folds `apply` in order" with no rollback at all. C1 is the only document that says a chain is all-or-nothing.

- **(a) Run state:** the ledger, source-owned; the seam is the **commit protocol**, not the location.
- **(b) Files:** `crates/data-loader/src/lib.rs::LoadResult` (one defaulted field) + the `GraphTransform` trait's `ImportFuture` return + `crates/graph-api/src/watcher.rs::rebuild_snapshot`'s finalize step.
- **(c) Cost alone:** the **widest blast radius of the three seams** — it touches the importer contract that every importer implementer must know about. But it is cheap in lines (~40) because `Non-decorated importers return `PublicationMetadata::default()`` and the change is additive. Highest cost, and it is the only way to make a chain all-or-nothing.

**Runner-up seam (C1, if the synthesizer needs a second):** §7's durable ledger format — `run-ledger/v1/<encoded-source-key>/{ledger.jsonl, checkpoint.json, deltas/*.cbor.zst}`, checksummed appends, fsync-then-rename compaction, "A truncated final line is ignored on recovery; any recovered nonterminal run becomes `Failed { kind: ProcessInterrupted }`". B1 mentions "runs/ on disk" with "compaction at 1 024 lines" and no recovery story; A1 persists records only. C1 is the only one that says what happens when the process dies mid-write.

---

## 8. Self-verification

| # | Question | Answer | Adjustment made |
|---|---|---|---|
| 1 | **Evidence completeness** — did I read everything and verify enough? | All three documents read in full (720/686/885 lines). ~25 cited line references verified against the tree, including every citation that decided a score: `eval_seed:418`, `spawn_blocking:751`, `watcher.rs:283/263`, `pipelines.rs:1231/1250/1270/1743`, proto max field 28, `DiscoveryField` fields, `data-loader:595-600`, `graph.nix:4-6/16-50/128/143-149`, `graph-combinators.nix:3-12/3-26/12/26/162-259`, `lib.rs:85-120/219-252`, `observer.rs:76`, `NodeMeta` 11 fields, caps 1M/4M, `tvix.rs:293-307`, `timeline.rs` 768 lines, `nodes.rs:754`. **Not** verified: B1's `lib.rs:61-78 check_caps_against`, B1's `vm/mod.rs:458-461`, A1's `data-loader:1454-1460`. None is a deciding citation. | None. D9 is reported as resting on ~25 verified citations rather than every one, and the three unverified references are named here so the reader can discount them. |
| 2 | **Bias check** — length, tone, formatting | C1 is the **shortest** document (52,971 B vs 68,895 / 70,862) and scores lowest — the right direction, so no length reward is operating. C1's five sub-3 scores are each a *named quoted absence* (no trait, no keyframes, no `app-proto`, no styling placement, no ARIA) — not an impression of terseness — and C1 still takes two 5s on the same axes B1 earns its 5s, which shows the scale is usable at its altitude. Conversely, B1's 15-section structure earned it D10 5 partly for having 6 phases; I checked that the credit is for the *fallback-per-kill* and *measurable acceptance*, which the spec's "not on how many phases" instruction permits, and not for the count. A1's 5 phases are credited for the named *Seam* field and node-content-first ordering, also not the count. | None, but §4 records explicitly that C1's low score is traceable to five specific quoted absences rather than to its length, and that D8's placement for C1 was audited for exactly this bias. |
| 3 | **Anchor fidelity** — both sides quoted before the number, stayed on the contrast axis | Every dimension carries a `closer_to`/`further_from` pair, each with its own quoted anchor text **and** its own quoted artifact text, written before the score. Two placements were audited for axis drift: (a) **D1/C1** — the trait's absence is one of the contrast's two own clauses, so it is on-axis; (b) **D8/C1** — the styling-placement element is in the `score_4` **anchor text** but not in the `contrast` line, so the placement leans on the anchor's positive pole rather than the contrast's wording. | The D8/C1 placement is disclosed as such: on the contrast line alone C1 would read 4, and the 3 rests on the anchor's fourth element being absent. A reader who scores the contrast line strictly will see C1 at +0.08 (3.65) — still third. |
| 4 | **Comparison integrity** — is my reference result right, are my own calls sound? | The reference was generated from the spec + task before any artifact was read. My two most consequential calls were re-litigated: **CK-013 NO for A1** (checked A1 §4.4.5's binding struct field-by-field — there is genuinely no trigger or interval — and §4.7, which derives `upcoming` from the *source's* `WatchPlan`); and **CK-019 NO for B1 and C1** (checked whether B1's "Why it exists" column and C1's "Justification:" lines answer the required question, and enumerated which routes do and do not). CK-031 for C1 was flipped from YES to NO on the ambiguity rule after re-reading, and I recorded that the cap is not score-changing. | Both contestable calls were run through a robustness test before publishing: re-grading CK-019 as YES moves B1 to 4.42 (first) and C1 to 3.73 (third) — **the order does not change under either alternative reading.** That is stated in §5 so the orchestrator can see the ranking is not hostage to the judgment call. |
| 5 | **Proportionality** | B1 4.34 / A1 4.14 / C1 3.57. I decomposed the B1−A1 gap to confirm it is not a rounding artefact: +0.27 (D5) +0.14 (D1) +0.14 (D2) +0.14 (D4) −0.16 (D6) −0.28 (D7) −0.05 (D9) = **+0.20**, matching the arithmetic. The largest single term is A1's CK-013 cap — a real, evidenced design gap, not a marginal call. A1–C1 is 0.57, a real separation. No dimension was scored 1 or 2 except where a cap forced it (B1 D6, A1 D5, C1 D6), and no document received a 5 it did not earn against quoted text. | None. |

---

## 9. Issues raised (for the orchestrator, not for the documents' authors to read as verdicts)

**High**

1. **A1's `[[parser.variables]]`-only scheduling model cannot answer "upcoming / past scheduled runs" for a transform.** The binding has no trigger or interval; `next_due_ms` is a post-tick `AtomicU64`; two of four driver kinds publish nothing. This is the largest single term in the B1–A1 gap and the one change that would reverse the ranking.
2. **C1 has no periodic keyframes**, so past runs are animatable but not viewable. Against the user's "run history (past)" ask this is a functional reduction, and it is the decisive reason C1 places third despite taking two 5s.
3. **C1's `GraphTransform` is a struct, not a trait.** There is no way for an external implementer — or for a Rust built-in at scale — to add a transform without forking. That directly contradicts the requester's upstreamability requirement and is D1's 3.

**Medium**

4. **B1's `[[schema.fields]]` example uses `name`/`type`; `DiscoveryField` is `deny_unknown_fields` over `key`/`field_type`/`required`/`searchable`/`facetable`.** The example package in §3.1 would be rejected by B1's own design. Confidently stated, no hedge. (A1's manifest example is exactly right; C1 sidesteps it.)
5. **Neither B1 nor C1 names `just app-proto`**, so both would add `NodeMeta` fields that are invisible in the UI — "the exact class of 'node has no info' bug the user reported, re-introduced by the fix", in the spec's own words. A1 alone satisfies it.
6. **CK-019's consequence is harsh where the document's actual strength is elsewhere.** C1 is the only document naming all four verified buffer APIs and the best on frame cost, and it is capped to 2 on an endpoint-justification sub-clause; B1's frame-cost design is likewise sound. Both scores understate their animation work, and the report says so rather than letting the number stand alone.
7. **C1 states no ARIA roles** for a drag-based scrubber, against `AGENTS.md:199` ("correct ARIA roles, one clear hierarchy per surface"). A1 supplies a complete contract; B1 supplies `role="alert"` only.

**Low**

8. **A1's reduced-motion query is read once at boot**, so it does not track a mid-session OS change. B1 and C1 both listen to the media query's `change` event.
9. **A1 embeds the whole graph as an escaped Nix string literal** (`builtins.fromJSON "<…>"`) while permitting a 1 M-node input via its own `[limits]`, with no cost discussion. B1 and C1 bind a native value; B1 §3.2 explains at length why the literal path was rejected. Feasibility concern, not a gate failure.

---

## 10. Rules — none created, with the reason recorded

Stage 7's candidacy filter was run on every issue above. Outcome: **no rules written.**

- **Issues 1, 2, 3, 7** (missing run-plan, missing keyframes, no trait, no ARIA) are **task-specific design gaps**, not generic anti-patterns. A rule cannot state "a transform binding needs a trigger field" without becoming a restatement of this one design.
- **Issue 4** (a config example that drifts from the real struct) is the one genuine *systemic* pattern — design documents with worked config examples drift because nothing in the toolchain type-checks prose. But the repo **already carries** `.claude/rules/transform-terminal-failure-path.md` and `.claude/rules/per-event-store-retention-bound.md`, which are the codified answers to the two most frequent round-1 findings; adding a third rule to a repository currently *under evaluation*, for a single observed occurrence, with no visibility into the existing rule set's scope, is not justified. **Recommendation for the orchestrator instead:** if a rule is wanted, fold "validate worked manifest examples against the defining struct before publishing" into a new `serde-config-examples.md` — not into this evaluation's outputs.
- **Issues 5, 6, 8, 9** are either consequences of checklist items that already exist, or single-document observations.

The correct Stage 7 outcome here is no rules: **it is better to create none than to pollute every future session with a rule derived from one document's one example.**
