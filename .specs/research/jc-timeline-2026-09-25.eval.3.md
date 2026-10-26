# Evaluation report — jc-timeline solution documents B1 / A1 / C1

Judge: `TimelineEvalJudge3`. Specification: `jc-timeline-eval-spec.yaml` (`evaluation_specification`,
rrd cycle applied). Artifacts: `.specs/research/jc-timeline-2026-09-25.solution.{b1,a1,c1}.md`.

Aggregation is the specification's own: `total = sum(weight_d * score_d)`, weights sum to 1.00, so
totals lie in [1.00, 5.00] with no division by 5. No global essential cap is applied; caps are
dimension-local, per `scoring.essential_fail_handling`.

---

```
VOTE: B1
SCORES:
  B1: 4.40/5.0
  A1: 3.81/5.0
  C1: 3.82/5.0
CRITERIA:
 - D1 — Core abstraction and external implementability: B1 4.0 | A1 3.0 | C1 4.0
 - D2 — Run-state location, storage shape, and bound: B1 5.0 | A1 4.0 | C1 4.0
 - D3 — Package model and tvix feasibility: B1 5.0 | A1 4.0 | C1 4.0
 - D4 — Run lifecycle and terminal outcomes: B1 5.0 | A1 4.0 | C1 5.0
 - D5 — Scheduling model and the upcoming-runs surface: B1 5.0 | A1 4.0 | C1 5.0
 - D6 — Serving and animation integration: B1 4.0 | A1 3.0 | C1 2.0 (cap)
 - D7 — Node content depth and cross-source generality: B1 3.0 (cap) | A1 5.0 | C1 3.0 (cap)
 - D8 — Impeccable surface discipline: B1 5.0 | A1 2.0 (cap) | C1 2.0 (cap)
 - D9 — Landmine risk accounting and groundedness: B1 3.0 | A1 4.0 | C1 4.0
 - D10 — Phase slicing and kill criteria: B1 5.0 | A1 5.0 | C1 5.0
DIVERSITY: YES — the ranked set contains two distinct run-state homes. B1 keeps a process-wide
 bounded ledger in graph-api's AppState beside the single ArcSwap<GraphSnapshot> (default-source-only,
 persisted under the importer packages dir, memory-only fallback). C1 keeps a per-source-keyed,
 durable ledger under run-ledger/v1/<encoded-source-key>/ (JSONL + checkpoint + per-delta CBOR),
 finalized through the decorator's LoadResult::publication metadata, with the decorator holding
 exactly one SourceBaseline. Different homes, so the ordering is CLEAN, not advisory.
SYNTHESIS SEAMS: B1 §5.2 + §7.2 (in-VM fuel/unwind budget + hash-chain-validated disk ledger with
 an O(n+e) presence+fingerprint delta) | A1 §4.4.3 + §4.10 (TransformedImporter decorator in
 data-loader + always-derived node facts/provenance) | C1 §7 + §11.2 (source-keyed durable ledger
 with DeltaAvailability honesty + one NodeFacts component shared by Nodes and Inspector)
```

---

## 1. Checklist results (34 items × 3 solutions)

Every item resolved from the solution text. "Deciding quote" is the line that decided it.
File references are `solution.<id>.md:<line>`.

### Tier `fatal` — repo law. A NO disqualifies from ranking.

| ID | B1 | A1 | C1 | Deciding quote / note |
|---|---|---|---|---|
| CK-001 Rust+Dioxus only, no JS/TS/bundler/three.js, incl. tweening | YES | YES | YES | A grep of all three documents for `javascript\|typescript\|npm\|vite\|three.js\|OrbitControls\|Cosmograph\|protobufjs\|bundler` returns **zero hits in all three**. Tweening is Rust in all three: B1 `render::begin_transition` rAF + `write_sizes_subset` (b1:566); A1 `render::pulse` → `pipes.update_sizes` (a1:558); C1 `begin_graph_transition`/`set_graph_transition_fraction` (c1:640-642). |
| CK-002 transformation rides existing `format_version=3` `{expr, [[parser.variables]]}`, not a new crate/SourceKind/match arm/--flags | YES | YES | YES | B1 `format_version = 3` + `[parser] engine = "tvix"` + `[[parser.variables]] name = "threshold"` (b1:97-129). A1 adds only `[parser] role = "transform"`, "default `\"generate\"` keeps every existing package valid" (a1:284). C1 `TvixParserToml gains one optional field. Existing generator packages remain valid because the default is `none`" (c1:90). |
| CK-003 no FS/network at eval time; external state as declared typed variables; re-bind workaround named | YES | YES | YES | B1 §3.4: "`default-features = false` drops `readFile`/`readDir`/`pathExists`/`getEnv`/`fetch*`. The transform sees precisely two things: the `graph` value and its declared variables" (b1:221) + a typed `$run.*` catalog table (b1:227-231). A1 §2.11: "The live graph enters as a `let graph = builtins.fromJSON "<escaped JSON>"; in …` binding — a string literal, not a file. All other external state enters as typed `[[parser.variables]]` bound at apply time" (a1:151). C1 §4.2: "The host converts `GeneratedGraph` directly into nested `tvix_eval::Value::Attrs`… `default-features = false`" (c1:201). |
| CK-004 core vocabulary is graph/transformation only | YES | YES | YES | B1 §10 + a self-audit: "A regex audit of this document for `session\|agent\|tool call\|goal\|omp\|/Users\|/home` finds exactly three kinds of hit" (b1:685) — the existing `session-manager` crate name, §10's own prohibition, and the pre-existing note `OMP Auto-Loop Topos.md` used as an example config. A1 §2.12 lists every new name and asserts no forbidden token "appears in any type, endpoint, package field, or panel string" (a1:155). C1 §12.12: "No requester-specific event taxonomy or machine-local location appears in a shared type, route, or panel" (c1:771). **No disqualification for any solution.** |

### Tier `essential` — correctness requirements, local cap only.

| ID | B1 | A1 | C1 | Deciding quote / note |
|---|---|---|---|---|
| CK-005 → D4 | YES | YES | YES | B1 §5.3: an 11-row terminal table, each row with "Snapshot = kept" (b1:453-465). A1 §2.1: four gates + throw, "the previous `GraphSnapshot` stays in the `ArcSwap` untouched… Never a crash, never a hang… never a silently truncated graph" (a1:66). C1 §6: eight bullet-exact terminal behaviors (c1:451-459). |
| CK-006 → D4 (pre-admission check + previous state on refusal) | YES | YES | YES | B1: "`check_caps_against` in `parse_graph_json`… with `max = min(JC_MAX_NODES / JC_MAX_EDGES, package [limits])`, **before any `GenNode` is materialised**" (b1:449). A1: G2 is "inside the Nix wrapper… `__n = builtins.length __r.nodes`… before `toJSON` materialises a multi-hundred-MB string" (a1:57, 62) and every gate row says "previous retained". C1: "enforced by a streaming `serde` visitor before vectors are fully materialized… stop decoding at `limit + 1`; never truncate and publish" (c1:447, 452). |
| CK-007 → D2 (bound in the same breath as the store) | YES | YES | YES | B1 §7.1 is literally titled "The bound, in one sentence" (b1:512). A1 §2.2's first sentence is the bound (a1:74). C1 §7's first sentence: "retains at most 256 terminal records, at most 30 days, and at most 64 MiB per source—whichever bound prunes first" (c1:465). |
| CK-008 → D2 (delta + keyframes, not a per-run whole-graph copy) | YES | YES | YES | B1 `RunDelta` + `Keyframe` "after every import run and after every 16th run", and the worst case computed: "A 1M-node `allow_empty` run produces a delta of 1M removed ids ≈ 40 MiB" (b1:514, 679). A1 `RunDelta { ops: Vec<graph_vcs::GraphOp> }` + `Keyframe { graph: Arc<Snapshot> }` "written every `JC_RUNS_KEYFRAME_EVERY` (default 16)… **Per-run full-graph copies do not exist**" (a1:74). C1 has deltas and **no keyframes**; the item's discriminating content (the rationale's own words) is "rather than a whole-graph copy per run", and C1 states: "The only full graphs are one source baseline, one active snapshot, and transient evaluation output—not one per run" (c1:856). YES on the gate; the absent keyframe is charged in D2's placement. |
| CK-009 → D2 (run state outside the single ArcSwap) | YES | YES | YES | B1: "`AppStateInner` gains `pub runs: Arc<runs::RunHost>` (*new*)… `keyframes: BTreeMap<RunId, Arc<Keyframe>>, // ≤ 4`" (b1:411-419). A1: "`AppStateInner` gains `pub runs: Arc<RunJournal>`" (a1:459). C1: "graph-api introduces one source-owned `RunLedger`… the run-state authority" (c1:465) and "The active `GraphSnapshot` remains graph-api's publication copy" (c1:372). |
| CK-010 → D2 (run output is a distinct addressable artifact) | YES | YES | YES | B1 `RunRecord.output_revision: Option<u64>, // Some only on Succeeded` + "the previous `GraphSnapshot` is untouched unless the status is `Succeeded`" (b1:385, 443). A1 `revision_before`/`revision_after` + step 5 "the record is visible with `counts: None` for the ≤ seconds the diff takes, then updated in place" (a1:416, 495). C1 is strongest: "Only after that succeeds does it atomically swap the snapshot and finalize every staged record with the published revision" + `RolledBack { cause_run_id }` (c1:385, 516). |
| CK-011 → D5 (reuse spawn_poll/WatchPlan, no new task per transform) | YES | YES | YES | B1 §6.2: "This design adds **one `select!` arm** to those loops and nothing else" (b1:494) — and for static sources, "a driver task that has *only* the runs arm", i.e. per source, never per transform (b1:504). A1 §2.6: "Runs execute **inside the existing driver loops**; no new tokio task per transform" (a1:121). C1 §8.1: "graph-api still starts **one** watcher task per hosted source. It does not start a Tokio task per transform" (c1:549). |
| CK-012 → D5 (miss / overlap / coalesce stated) | YES | YES | YES | B1 §6.1: "**Coalesced**: however many intervals elapsed… exactly **one** run fires… Never replayed, never queued" and "**Impossible by construction**", plus a daily grace window that writes `Skipped { MissedWindow }` (b1:480-481). A1 §2.6: "Missed intervals: skip, never replay… Filesystem bursts: coalesce… Overlap: never" + `pending_change` guaranteeing one coalesced follow-up so a real change is never lost (a1:124-126). C1 §8.2, seven bullets including "Poll ticks observed while busy are dropped, not queued" and "On process restart, elapsed intervals are skipped; there is no catch-up storm" (c1:562-569). |
| CK-013 → D5 (plan is a declared, serialisable value) | YES | YES | YES | B1 `Trigger { AfterImport, Every{interval_ms}, Daily{at_utc}, Manual }` in the catalog, `next_due_at = last_finished_at + intervalMs` "from the ledger on boot" (b1:355-360, 480), editable via `PUT /importers/:source_id/transforms/:transform_id/variables` in phase 5 (b1:537). A1's run plan is the catalog-declared, serialisable source `WatchPlan`; the displayed value is written by the owning driver from the same `Interval` that will fire, not recomputed at read time (a1:128). A1's shortfall — the *anchor* re-seeds from `now` after a restart, and a transform has no cadence of its own — is charged in D5, not here. C1 declares `watch: { kind: "poll", interval_ms: 300000 }` in the catalog binding and persists it: "`checkpoint.json` preserves the next sequence and schedule anchors" (c1:433, 543). |
| CK-014 → D3 (closure-across-3-imports workaround) | YES | YES | YES | B1 §3.3 header: "Takes the graph.nix library as the `graph` PARAMETER (the same tvix closure-across-3+-imports workaround graph-combinators.nix uses…) and imports nothing itself", with an import-depth derivation (b1:179-186). A1 §2.10: "takes `{ graph }` explicitly… imports nothing itself (not `birds.nix`, not `graph.nix`)… composes at two import levels max" (a1:147). C1 §4.4: "`graph-combinators.nix` continues to take `{ graph }`; it does not import `graph.nix`, `graph-iter.nix`, or `birds.nix` through another layer" (c1:275). Verified: `graph-combinators.nix` does carry the `{ graph }` header. |
| CK-015 → D3 (graph injected by the eval_seed idiom) | YES | YES | YES | A1 is the literal form: "`let graph = builtins.fromJSON "<json, escaped>"; in (<expr>) { graph = graph; tag = "kind"; min_size = 2; }`" (a1:248-249). B1 and C1 use the builder, with the same applied shape: B1 "`( <expr> ) { inherit graph; <bindings> }`… `EvaluationBuilder::env`… binds names into the top-level scope — the compiler declares them as root-scope locals… which is precisely what `eval_seed`'s `let n = …; in (…)` does textually" (b1:148, 168). C1: "The applied expression is conceptually `( <expr> ) { inherit graph; <declared-variable-bindings>; }`. Only the small expression and scalar bindings are rendered as source text. The graph is not serialized into that string" (c1:205-209). Verified: `EvaluationBuilder::env` is at tvix `eval/src/lib.rs:261`, exactly as B1 cites. |
| CK-016 → D3 (native `spawn_blocking` path) | YES | YES | YES | B1 §5.1: "does exactly what `POST /generate` (`server.rs:733-774`)… do… cross into `tokio::task::spawn_blocking`" (b1:441). A1 §4.4.2: "`timeout(eval_ms, spawn_blocking(|| eval_transform(expr, json, caps)))`" (a1:368). C1 §6: "inside the same native `spawn_blocking` boundary used by `POST /generate`" (c1:443). Verified: `generate_post` begins at `server.rs:733`. |
| CK-017 → D6 (patch the delta into existing buffers, no per-tick refetch) | YES | YES | YES | B1: `write_sizes_subset` "beside `update_sizes`, `pipelines.rs:1270`, writing only contiguous runs with `queue.write_buffer`" (b1:566) — verified `update_sizes` is at `render/pipelines.rs:1270`. A1: `render::pulse` → `pipes.update_sizes` (`pipelines.rs:1270-1285`) and α interpolation via `pipes.update_colors` (`1231-1245`) — both verified. C1: "`GraphPipelines::set_positions`, `update_colors`, `update_edge_colors`, and `update_sizes` for frames" (c1:645) — all four verified to exist (`pipelines.rs` 1743 / 1231 / — / 1270). None of the three re-fetches `/graph/init`, `/graph/ids`, `/graph/edges` per frame; the reload happens once per publication, which is the existing app behaviour. **Shared shortfall, charged in D6:** none of the three names the existing revision guard *inside* `graph_canvas::load` (verified at `graph_canvas.rs:62-70, 92-100`) as the per-frame admission check. |
| CK-018 → D6 (topology change = full reload, distinguished from a patch) | YES | YES | YES | B1: live = "`reload_graph` fetches the new buffers"; scrub = "mounts a **hold scene**" (a full remount) versus the per-frame subset write; Phase 1 says "the existing revision probe drives a plain `reload_graph` (full re-seed as today)" (b1:568, 644). A1: "`reload_graph` remounts as today" versus `render::pulse` in place, and `render::mount_canvas` for a scrub frame (a1:578, 582). C1: "they use `mount_canvas` for the union topology and … `set_positions`, `update_colors`, `update_edge_colors`, and `update_sizes` for frames" + "the revision probe … does not reload" for non-publication (c1:645, 662). |
| CK-019 → D6 (every new endpoint justified against the existing route set) | YES | YES | **NO → D6 ≤ 2** | B1 §8 is a per-route table with a "Why it exists" column, and it points at the search space: "All routes join `api_routes()` (`server.rs:54-119`)" (b1:528-530), with per-route reasoning such as "no second round-trip, no new store" for `/node/*id`. A1 §4.8 is a per-route table with a "Why this endpoint" column naming concrete existing routes: "`the same shape /search/matches uses (server.rs:2027-2064)`", "`mirrors /graph/ids`", "`mirrors /graph/edges`", and for node content "`No new endpoint: the node the user clicked already has one`" (a1:541-545). C1's three justifications never reference an existing route — "run history is control-plane data, not a bulk numeric buffer or protobuf node payload", "placing them in `/runs` would defeat bounded hydration", "reusing the watcher channel preserves single-flight ordering" (c1:584, 588, 592) — the last of which is about a channel, not a route. C1's only mention of the existing set is a response *header*: "The existing `X-Graph-Revision` header remains the canvas reload signal" (c1:594). |
| CK-020 → D7 (reaches generated graphs / `convert_generated_graph`) | YES | YES | YES | B1 §4.3 extends `convert_generated_graph` itself, "this is the fix for \"frontmatter empty, content_readable false\"" (b1:330). A1: "`convert_generated_graph` (generate role) changes in one place: `frontmatter` = `metadata` minus the promoted keys" (a1:374) and "the Inspector's chip walker and leftover grid… light up for generated and transformed graphs with **zero new UI**" (a1:590). C1 §2 names the defect with its path, "`convert_generated_graph` builds empty frontmatter (`crates/importer/src/tvix.rs:507-566`)" (c1:45), and Phase 1 works it. |
| CK-021 → D7 (source-generic, on the shared `GET /node/*id`) | YES | YES | YES | B1: "`proto::NodeMeta` gains `properties`, `provenance`, `edges` (+ `edges_total`, `edges_truncated`), `history`" on the existing route, with acceptance "a pest node, a tvix node, and an Obsidian node show the same four sections with **no per-source branch**" (b1:538, 660). A1: facts from `snap.schema.fields` (every importer publishes a schema) + provenance from the journal, and because "every rebuild is a run" the run section is populated for every source (a1:591-593). C1: one `NodeFacts` component used by both `nodes::focused_node` and `inspector::node_view` (c1:696-710). |
| CK-022 → D8 (reduced-motion variant + non-animated equivalent) | YES | YES | YES | B1 §9.5: `duration_ms = 0`, changed/added nodes "held in the existing highlight set… for 2 s so the *set* that changed is legible **without any motion**", and "the Runs panel's selected-run section lists the same ids as **chips** so the information is also available as text" (b1:585-586). A1 §2.8: the pulse "becomes a **static** 4 s emphasis via `render::push_focus_set`", playback disabled, "union-frame fade-in/out becomes fixed alpha" (a1:136). C1 §10.4: "play/auto-advance controls are hidden; selecting 0 or 1 snaps immediately… intermediate dragging changes only the static added/removed highlight set… The semantic before/after information and counts remain available" (c1:652-658). |
| CK-023 → D8 (measured % or explicitly indeterminate, never fabricated) | YES | YES | YES | B1 §9.7, five rows, including "`Evaluating` is **indeterminate** (`fraction: None`)" and "History list / upcoming → store pending → indeterminate bar; **never a number**" (b1:600-603). A1 §2.9: "Never a synthetic percentage for eval"; past-due shows "`due · waiting for the driver` rather than a negative number" (a1:140-141). C1 §10.2: "`Some(f)` is shown as a percentage; `None` stays indeterminate. **Elapsed time is never converted into a fake percentage**" (c1:625). |
| CK-024 → D8 (store-shaped hydration for the run list **and the node detail on click**) | YES (contestable — see §5) | **NO → D8 ≤ 2** | **NO → D8 ≤ 2** | B1 §9.6 creates three stores and states the node-body answer: "The Inspector's node fetch moves from its `Spinner`… to `loading_store("inspector", "loading node…")` + `LoadingGate` over the body; `Spinner` stays only for the tag-browser search box" (b1:593). **But the citation is wrong**: `app/ui/src/panels/inspector.rs:37` is `use panel_kit::Spinner;` — the bare `Spinner { label: "loading node…" }` is at `app/ui/src/panels/nodes.rs:754` in `focused_node`, exactly as the specification's rationale states. Ruled YES on substance (the store-shaped path is stated and the panel-level bare Spinner is explicitly eliminated); the mislocation is charged in D9. A1 upgrades only the runs list and the scrub frame: "`loading_store("runs", …)`… `loading_store("runs-frame", …)`… `LoadingGate { store, … }`… `GlobalLoadingBar`… **No hand-rolled `loaded` signals**" (a1:117) — and says nothing about the node-detail surface, whose new Facts/Provenance/Runs sections (a1:593) leave `nodes.rs:754`'s `Spinner` in place. C1 is the same: §10.2 covers only the ledger and delta stores (c1:616-623), while its node work is a pure refactor of existing Inspector markup (c1:691-710) with no statement about that surface's loading path. |
| CK-025 → D3 (no `foldl'` over `addNode`; `genList` + one `listToAttrs`) | YES | YES | YES | B1: "Every builder produces its attrset in ONE listToAttrs (O(n)); no `foldl'` over `addNode` (O(n²), see soupGen…)" (b1:189-190). A1: "Every builder is one `builtins.genList` + one `builtins.listToAttrs` (the `soupGen` pattern…), never a `foldl'` over `addNode`" (a1:147). C1: "one `builtins.map`/`builtins.filter` and one `builtins.listToAttrs`; none folds `graph.addNode` or `graph.addEdge` over a growing attrset" (c1:273). Verified: `soupGen`-style `genList`+`listToAttrs` is real at `graph.nix:178-230`. |

### Tier `important` — graded via a dimension cap at 3.

| ID | B1 | A1 | C1 | Deciding quote / note |
|---|---|---|---|---|
| CK-026 → D9 (names concrete files and symbols) | YES | YES | YES | B1 §15 lists ~40 `file:line` pairs (b1:711-720); A1 §3's S1-S11 table names `crates/tvix-wasm/src/lib.rs`, `crates/importer/src/tvix.rs`, `graph-api/src/watcher.rs::rebuild_snapshot`, `app/ui/src/graph_canvas.rs::graph_data_from_frame` (a1:163-172); C1 §2-§4 name ~20 paths with line ranges. All three are checkable. |
| CK-027 → D10 (knowledge-map update in the same change) | YES | YES | YES | B1 §11 titles it "Knowledge-map update (item 4) — **same change as the code**" and tabulates 11 notes (b1:613-630). A1 §2.4: "Landed in the **same change** as each phase (§5)" — 13 notes (a1:98-113). C1 §12.4 names the new `Transformations and Runs.md` and 8 linked notes (c1:728-739), and *also* repeats the note edits inside each phase's work list in §14 (c1:812, 822, 832, 842). |
| CK-028 → D10 (phase list, per-phase value, explicit dependencies) | YES | YES | YES | B1 §12 Phases 0-5, each with **Ships**/**Acceptance**/**Kill**, and the framing "usable the day it merges, and has a kill criterion that returns the team to the previous phase's behaviour without a revert of shared types" (b1:636-666). A1 §5 Phases 0-4 with **Lands**/**Seam**/**Acceptance**/**Kill** *plus* an explicit S1-S11 dependency table and mermaid DAG (a1:161-185, 616-643) — the strongest dependency articulation of the three. C1 §14 Phases 1-4 with **User-visible value**/**Work**/**Acceptance**/**Kill criterion** plus §3's eight-step dependency list (c1:75-84, 806-846). |
| CK-029 → D1 (at least one non-omp worked example) | YES | YES | YES | B1 `transform-relabel-hubs.toml` on "the default **Obsidian** source", with acceptance naming Kubernetes, httpjson and `generate-clusters` (b1:645). A1 `collapse-by-facet.toml` — "meaningful for Kubernetes namespaces, Obsidian folders, generated cluster types, or any pest `kind` alike" (a1:155), bound to `generate-clusters` in Phase 2 (a1:631). C1 `example.annotate-nodes`, whose Phase 2 acceptance is "the same annotation package runs unchanged against **markdown, Kubernetes, and generated** fixtures" (c1:101, 824). |
| CK-030 → D7 (checked-in prost types + `just app-proto`) | **NO → D7 ≤ 3** | YES | **NO → D7 ≤ 3** | A1 §4.4.6: "Regenerated into `app/ui/src/proto/` via **`just app-proto`** (checked-in prost types, AGENTS.md)" (a1:485). B1 adds `proto::NodeMeta` fields (b1:538, 659) and a `grep` for `app-proto\|app/ui/src/proto\|prost` across B1 returns **zero hits** — the checked-in prost types are never mentioned. C1 §11.1: "The protobuf `NodeMeta` gains new fields after the existing field numbers; **checked-in app proto types are regenerated**" (c1:689) — it accounts for the checked-in types and states regen is part of the change, but never names the `just app-proto` step; under the strictness rule ("even minor inaccuracies exclude a YES") this is a NO. |
| CK-031 → D8 (styling in `app.css` + `panel_kit::CSS`, existing palette) | YES | YES | YES | B1 §9.5: "The strip's CSS transitions (`.runs-strip .run { transition: … }`, `app.css`) are disabled under `@media (prefers-reduced-motion: reduce)` **alongside the existing rule at `app.css:62-64`**" (b1:587) — verified: `app.css:62` is exactly that media query. Status colour comes from `panel_kit::badge` (b1:553). A1 reuses `badges::frontmatter_chips` and existing class names (`section.nodes-facts`, `div.nodes-focus-body`) (a1:593) and cites the same `app.css:62-64` precedent. C1 §10.4: "`app/ui/assets/app.css` also disables transition/pulse animations inside the existing reduced-motion media query", and §11.2 *moves* the existing Inspector styling rather than adding a new vocabulary (c1:658, 700-708). |

### Tier `pitfall` — a YES is the defect and caps its dimension at 2.

| ID | B1 | A1 | C1 | Deciding quote / note |
|---|---|---|---|---|
| CK-032 → D6 (timeline.rs is a layout keyframe scrubber, not run history) | no | no | no | A1 §4.9.5 is explicit: "Unchanged: it scrubs layout-position frames (`timeline.rs:1-27`); Runs scrubs graph states. The two are different axes and stay separate panels" (a1:586). C1 §2/§10.1: "The existing Timeline panel owns layout-position history, not graph revisions… It remains intact" / "do not repurpose `Panel::Timeline`, whose `FrameRing` remains layout-position history" (c1:52, 600). B1 never says what `timeline.rs` is, but it demonstrably did not conflate: a separate `panels/runs.rs`, a separate `Panel::Runs`, its own store ids, and a citation that shows it read the file ("the Timeline panel's paused semantics, `panels/timeline.rs:225-235` `ScrubState::Paused`") (b1:560, 568). No cap for anyone. |
| CK-033 → D5 (own scheduler runtime) | no | no | no | B1 "one `select!` arm … and nothing else" (b1:494); A1 "no new tokio task per transform" (a1:121); C1 "It does not start a Tokio task per transform" and "cron is intentionally excluded" (c1:549, 864). No cap for anyone. |
| CK-034 → D9 (risk/caveats section contradicting the files) | no | no | no | B1's risk-bearing text is §5.2 ("Remaining risk: a `Drop` impl in tvix aborting on unwind" — admitted uncertainty, not a false claim) and its §14 rationale table, whose load-bearing claims I verified: "`diff_snapshots` … serialises each node to JSON to detect change (`merge.rs:203-214`)" — `merge.rs:203` is `diff_snapshots` and line 209 is `node_json(old) != node_json(node)`, true; "`readFile` is an *impure* builtin and unavailable under `default-features = false` (`tvix/eval/src/builtins/impure.rs:70`)" — the pure/pure-only split is confirmed; "the caps are 1M/4M" — true. A1's §6 self-verification **corrects** two of its own first-draft errors ("the first draft claimed `validate_local_id` rejects `:`; it does not. **Corrected**") and marks inferences `[INFERENCE]`; its one false claim (`field_type = "integer"`, a1:277 — the real enum is `text\|keyword\|date\|url`, per `pest.rs:224-230`) is in a manifest example, not a risk section. C1's §2 and §13 claims check out one by one (`GenNode` at 221 ⊂ cited 219-252; `addNode` at 20-21 ⊂ 16-50; `random`/`clustered` genList at 174/214 ⊂ 151-261; caps consts at 43-44 ⊂ 25-44; `diff_snapshots` at 203 ⊂ 202-234; `focused_node` at 744). No cap for anyone. **The verified false claims elsewhere in B1 are charged through D9's placement, not through this cap** — see §3 D9 and §4. |

### Checklist summary

| | B1 | A1 | C1 |
|---|---|---|---|
| Total items | 34 | 34 | 34 |
| YES | 32 | 32 | 30 |
| NO | 2 (CK-019 n/a, CK-030) | 2 (CK-024, —) | 4 (CK-019, CK-024, CK-030) |
| Fatal NO (disqualified) | 0 | 0 | 0 |
| Pitfall YES (defect) | 0 | 0 | 0 |
| Dimension caps applied | D7 ≤ 3 | D8 ≤ 2 | D6 ≤ 2, D7 ≤ 3, D8 ≤ 2 |

Precise NO list: **B1** — CK-030. **A1** — CK-024. **C1** — CK-019, CK-024, CK-030.

---

## 2. Rubric scores, dimension by dimension

Each placement names the pole the artifact sits nearer and the pole it sits further from, with both
sides evidenced *before* the number.

### D1 — Core abstraction and external implementability (w 0.14)
`contrast`: "The run is described as a transformation applied to a graph revision, and the transform
contract is implementable from outside the repo — not a field list named after one application's loop."

**B1** — closer to `score_4`. Anchor text: `pub struct RunRecord { pub id: RunId, pub transform: TransformId, pub input_revision: u64, pub output_revision: Option<u64>, pub status: RunStatus, pub delta: RunDelta, }` + "// a third party implements `Transform`: expr + variable schema + fn apply(g: &Graph, bound: &Vars) -> Result<Graph>". Artifact (b1:373-395, 242-246): `pub struct RunRecord { pub run_id: RunId, pub transform_id: String, pub package_id: Option<String>, pub package_version: Option<String>, pub variables: BTreeMap<String, String>, pub trigger: Trigger, pub status: RunStatus, pub input_revision: Option<u64>, pub output_revision: Option<u64>, pub input_hash: u64, pub output_hash: Option<u64>, pub delta: DeltaStats, pub keyframe: bool, pub budget: RunBudget, pub error: Option<String>, }` plus `pub trait Transform: Send + Sync { fn id(&self) -> &str; fn descriptor(&self) -> TransformDescriptor; fn apply(&self, input: &VaultGraph, ctx: &RunCtx) -> Result<TransformOutput, TransformError>; }` in a crate that is "wasm-clean like `importer --no-default-features`" (b1:238). Revision in/out, status, delta and an externally implementable trait — every clause of the pole.
Further from `score_4`: the pole's `apply` takes `(g: &Graph, bound: &Vars)`; B1's takes `(&VaultGraph, &RunCtx)` where `RunCtx` bundles a budget, a `Trigger` enum and a `&dyn data_loader::ImportProgress` (b1:250-257), so a third party must also depend on `data-loader`'s progress contract — implementable, but strictly more coupled. Placement: **matches score_4** → **4**.

**A1** — closer to `score_4`. Anchor: the same `RunRecord` + implementable `Transform`. Artifact (a1:414-417): `pub struct RunRecord { pub id: RunId, pub trigger: RunTrigger, pub forced: bool, pub transform_ids: Vec<String>, pub started_ms: u64, pub finished_ms: Option<u64>, pub revision_before: u64, pub revision_after: Option<u64>, pub counts: Option<RunCounts>, pub outcome: RunOutcome, pub stages: Vec<RunStage> }` and `pub trait GraphTransform: Send + Sync { fn descriptor(&self) -> TransformDescriptor; fn apply<'a>(&'a self, input: LoadResult, progress: &'a dyn ImportProgress) -> ImportFuture<'a, Result<LoadResult, ImportError>>; }` (a1:383-386).
Further from `score_4`: the pole's record carries `delta` and the *bound variables*; A1's carries neither — no `delta` field and no resolved-variable map, so a run is not replayable and A1 says so only implicitly (`tvix.rs` §4.1 routes re-binding through the package's variables). Lean: none — the shape matches on revision/status/contract and falls short on the record's replay fields. Placement: **past score_2, short of score_4** → **3**.

**C1** — closer to `score_4`. Anchor: the pole's `RunRecord`. Artifact (c1:484-502): `pub struct RunRecord { pub id: TransformRunId, pub transform_id: TransformId, pub sequence: u64, pub package_id, pub package_version, pub package_digest: String, pub trigger, pub scheduled_at_ms, pub input_revision: u64, pub input_digest: String, pub output_revision: Option<u64>, pub chain_index: u32, pub variables: BTreeMap<String, ScalarValue>, pub state: RunState, pub delta: DeltaAvailability, }` — every pole field plus a package digest, a chain index and a recorded, resolved variable map ("Resolved variables, including declared context-derived values, are recorded", c1:528).
Further from `score_4`: the pole's contract is `// a third party implements Transform: … fn apply(g: &Graph, bound: &Vars)`. C1 offers **no trait** — `pub struct GraphTransform { … }` is a concrete type in `crates/importer/src/transform.rs` and "`build_tvix_importer` accepts only `TvixInput::None`; `build_graph_transform` accepts only `TvixInput::Graph`" (c1:170), so a stranger writes a tvix package, not a Rust impl. Placement: **matches score_4** (the axis is external implementability, and a package-only contract is the only form CK-002 permits in this repo — demanding a trait here would contradict the fatal gate) → **4**.

D1: **B1 4 | A1 3 | C1 4**

### D2 — Run-state location, storage shape, and bound (w 0.14)
`contrast`: "A bounded, addressable run ledger of deltas with periodic keyframes, living outside the
single-snapshot swap — not an append-only pile of whole-graph copies beside the snapshot."

**B1** — closer to `score_4`. Anchor: `struct RunLedger { cap: usize /* 512 */, keyframe_every: usize /* 32 */, runs: VecDeque<RunRecord> }` + `RunDelta { added, removed, changed }` + "one delta per run; a full keyframe every 32; ring retention of 512; output admitted as its own revision". Artifact (b1:411-419, 514): `pub struct RunLedger { records: VecDeque<RunRecord>, // ≤ retain_runs … deltas: BTreeMap<RunId, Arc<RunDelta>>, // ≤ retain_deltas AND ≤ delta_bytes … keyframes: BTreeMap<RunId, Arc<Keyframe>>, // ≤ 4 … touched: HashMap<…>, next_run_id, seq }` with "at most **512 run records** (`JC_RUNS_RETAIN`)… the full `RunDelta` only for the **newest 64 runs and at most 64 MiB**… a **`Keyframe`** … after every 16th run… as long as the graph has ≤ 200 000 nodes… retaining the **newest 4 keyframes**" and a stated worst case "≈ 115 MiB".
Further from `score_4`: nothing on the axis. Placement: **strictly better than score_4 on the same axis** — the respect is *shape and bound*: the bound is enforced on three independent axes (count, bytes, keyframe node-ceiling) with an explicit memory worst case, a bounded disk fallback, and — uniquely — an `O(n+e)` `presence_delta` that replaces `graph_vcs::diff_snapshots`, whose per-node JSON comparison B1 names as "prohibitive at 10⁵–10⁶ nodes per run" (b1:310), plus hash-chain validation so a retained delta is never applied to the wrong graph after a restart (b1:520). → **5**.

**A1** — closer to `score_4`. Anchor: the bounded `RunLedger` of deltas + keyframes outside the swap. Artifact (a1:438-443, 74): `pub struct RunJournal { budget: JournalBudget, …, inner: Mutex<JournalInner /* records: VecDeque<RunRecord>, deltas: VecDeque<Arc<RunDelta>>, keyframes: VecDeque<Keyframe>, in_flight, pending_change, frame_cache */>, pub schedule: RunSchedule }` with "at most `JC_RUNS_RETAIN` (default 256)… at most `JC_RUNS_DELTA_BYTES` (default 64 MiB)… at most `JC_RUNS_KEYFRAMES` (default 2) full `Arc<graph_vcs::Snapshot>` keyframes… **the live graph is never copied**".
Further from `score_4`: the pole's delta is `{ added, removed, changed }` computed cheaply; A1's per-run diff is "`present and `serde_json::to_value(node)` differs → `UpsertNode`" (a1:510) — the exact per-node JSON serialisation B1 rejects — bounded only by a cutoff (`JC_RUNS_DIFF_MAX_NODES = 500_000`, above which "the fine compare is skipped"), not by a cheaper algorithm. Lean: toward `score_4`. Placement: **matches score_4** (the bound half is richer than the pole: byte gates, a `keyframe_bytes` gate, a big-run substitution rule, `scrubbable: false` honesty) → **4**.

**C1** — closer to `score_4`. Anchor: the bounded, addressable delta ledger outside the swap. Artifact (c1:465-482): "one source-owned `RunLedger` that retains at most 256 terminal records, at most 30 days, and at most 64 MiB per source—whichever bound prunes first… **it never stores per-run full graph snapshots**", with `pub struct RunRetention { max_terminal_records: usize /* 256 */, max_age: Duration /* 30 days */, max_bytes: u64 /* 64 MiB */, max_delta_bytes: u64 /* 8 MiB compressed per run */ }`, a compact reversible `RunDelta` ("stable IDs, minimal node tombstones… changed-property patches… It is O(changes), not O(graph) by construction", c1:530), deterministic BLAKE3 run ids, `checkpoint.json`, atomic compaction, and `DeltaAvailability::Omitted { SizeLimit }`.
Further from `score_4`: the pole names "periodic keyframes"; C1 has **no keyframe mechanism at all** — its scrub path is delta-only and it says so: "The run scrubber controls a selected transition from 0.0 to 1.0; it does not claim to materialize every historical full graph" (c1:647). Lean: toward `score_4` (four independent bounds against the pole's one, deterministic addressable ids, and an explicit typed `Omitted` state where the pole has none; nothing in the artifact resembles `score_2`'s untrimmed `Vec` of whole graphs). Placement: **past score_2, short of score_4** → **4**.

D2: **B1 5 | A1 4 | C1 4**

### D3 — Package model and tvix feasibility (w 0.09)
`contrast`: "A version-3 package on the existing tvix engine, receiving the graph through the
established let-binding idiom and building it in one pass — not a bespoke engine block that reaches
outside the evaluator."

**B1** — closer to `score_4`. Anchor: `format_version = 3` / `[parser] engine = "tvix"` / `expr = "let graph = import /jc/src/graph.nix {}; in …"` / "host binds the live graph the way eval_seed does" / "nodes built with genList + one listToAttrs". Artifact (b1:97-129, 168, 191-192): `format_version = 3`, `[parser] engine = "tvix"`, `expr = '''{ graph, threshold }: let g = import /jc/src/graph.nix { }; t = import /jc/src/graph-transforms.nix { graph = g; }; …`, `nodesFromList = ns: builtins.listToAttrs (builtins.map (n: { name = n.id; value = n; }) ns);`, and the host path "`eval_graph_with_input`… is `eval_to_json` with `.env(Some(&{ "graph" => input_value(input) }))`".
Further from `score_4`: one verified defect in the worked example — B1's `[[schema.fields]]` block is written `name = "weight"` / `type = "number"` (b1:133-137), but the real TOML keys are `key` / `field_type` (verified: `charts/jump-cannon/packages/chembl-pharmacology.toml:52-58` and `crates/importer/src/pest.rs:216-230`), so B1's own example package would not validate. Placement: **strictly better than score_4 on the same axis** — the respect is *feasibility at the cap*: the graph is bound as a native, lazily-forced `Value` ("each thunk, when forced, builds ONE `NixAttrs` via `NixAttrs::from_iter` (O(n))", b1:163-164), avoiding the `score_4` text's JSON materialisation entirely, and a runaway is stopped inside the VM by an op-counted `RuntimeObserver` whose hook and wiring I verified at tvix `eval/src/vm/mod.rs:461`, `eval/src/observer.rs:42`, and `eval/src/lib.rs:282` — the exact three citations B1 gives. → **5**.

**A1** — closer to `score_4`. Anchor: the pole's `[parser] engine = "tvix"` + let-binding + one-pass build. Artifact (a1:246-254, 147): `format_version = 3` / `[parser] engine = "tvix" / role = "transform"`, rendered as `let graph = builtins.fromJSON "<json, escaped>"; … in ( <expr> ) { graph = graph; … }`, and "Every builder is one `builtins.genList` + one `builtins.listToAttrs` (the `soupGen` pattern…), never a `foldl'` over `addNode`".
Further from `score_4`: the pole's text does not materialise the graph; A1's channel does — `graph_to_nix_input(graph: &VaultGraph, with_metrics: bool) -> String` (a1:356) plus `nix_string_literal` (a1:330) means a 1M-node input is rendered as one escaped Nix string literal, and A1 budgets only `eval_ms` (default 120 s) with no acknowledgement of the O(graph) render cost; its own Q1 acceptance caps its scrub measurements at 200 k nodes. A second verified slip: `field_type = "integer"` (a1:277) is not a member of the `DiscoveryFieldType` deserialisation set (`text|keyword|date|url`, `pest.rs:224-230`). Lean: none — the shape is the pole's own, but the pole is silent on the 1M-node channel and the manifest example is invalid. Placement: **past score_2, short of score_4** → **4**.

**C1** — closer to `score_4`. Anchor: the pole's package + injected graph + one-pass build. Artifact (c1:93-123, 194-209, 273): `format_version = 3` / `[parser] engine = "tvix" / input = "graph"` / `expr` importing `graph.nix { }` and `graph-iter.nix { graph = g; }`, applied as "`(<expr>) { inherit graph; <declared-variable-bindings>; }`. Only the small expression and scalar bindings are rendered as source text. **The graph is not serialized into that string**", with "Every mapper/filter builds its result with one `builtins.map`/`builtins.filter` and one `builtins.listToAttrs`".
Further from `score_4`: the pole shows a complete, evaluable expression; C1's combinator library is elided — `mapNodes = f: g: …; filterNodes = pred: g: …; groupNodesBy = keyFn: g: …; zipNodesWith = f: left: right: …` (c1:256-269) — so the Nix that would actually run is not specified, and C1 never analyses the import depth the way B1 does. Lean: none (the channel matches the pole and exceeds it; the *library* the channel feeds is unspecified). Placement: **matches score_4** on the injection and build clauses, short on a complete expression → **4**.

D3: **B1 5 | A1 4 | C1 4**

### D4 — Run lifecycle and terminal outcomes (w 0.14)
`contrast`: "Each terminal path is a recorded run carrying a reason, checked before admission, with
the previous graph left intact — not an error that is logged and a flag that is set."

**B1** — closer to `score_4`. Anchor: `enum RunStatus { Succeeded { output_revision: u64 }, Failed { reason: RunFailure, at_revision: u64 } }` + `RunFailure::NodeCap{got,max}|EdgeCap{..}|Timeout{limit}` + "a wall-clock ceiling converts a hang into `Failed{ Timeout }`; an empty result is a recorded, addressed state". Artifact (b1:399-407, 453-465, 447): `pub enum RunStatus { Queued, Running, Succeeded, Failed { reason: FailureReason }, TimedOut { ops: u64, elapsed_ms: u32, kind: BudgetKind }, Rejected { reason: RejectReason }, Skipped { reason: SkipReason } }` with reasons `Eval | OutputTooLarge | EmptyOutput | Projection | Snapshot | Internal`, `InputTooLarge | VariablesInvalid`, `MissedWindow | Suspended`; an 11-row terminal table each row "Snapshot: kept"; and the hang handled inside the VM — "When `ops > max_ops` or the wall deadline passes it calls `std::panic::panic_any(BudgetExceeded { … })`… `catch_unwind` in the blocking closure downcasts the payload… **No orphaned thread, no hung driver task, no partial graph.**"
Further from `score_4`: nothing on the axis; the pole's "wall-clock ceiling converts a hang" is the clause the other two documents cannot actually deliver in-process, and B1 delivers it with a verified engine hook. Placement: **strictly better than score_4 on the same axis** — the respect is that the hang→recorded-`TimedOut` conversion is *realised*, not merely declared, and the run set is seven variants against the pole's two, each with a UI row in §9.4. → **5**.

**A1** — closer to `score_4`. Anchor: the pole's status enum with typed failure reasons and a pre-admission cap check. Artifact (a1:409, 52-66): `pub enum RunOutcome { Succeeded, Failed { error: String }, TimedOut { ceiling_ms: u64 }, Refused { reason: String }, Skipped { reason: SkipReason } }` with a four-gate table (G1 input, G2 in-eval `builtins.length` before `toJSON`, G3 `check_caps_against`, G4 wall clock) + throw, every row "previous retained".
Further from `score_4`: the pole says a ceiling "converts a hang into `Failed{ Timeout }`"; A1 is explicit that it cannot — "**Timeout is not cancellation.** tvix has no fuel or preemption… on G4 the blocking thread keeps running", with an orphan thread, a `Skipped { OrphanEval }` follow-on, and the admitted limit "a non-terminating expression that allocates without bound **cannot be stopped in-process**" (a1:64, 70). The run is recorded; the runaway is not. Placement: **matches score_4** on recording/binding/visibility, short of the pole's convergence → **4**.

**C1** — closer to `score_4`. Anchor: the pole's status/failure model. Artifact (c1:504-518, 451-459): `pub enum RunOutcome { Published { summary, stats }, Unchanged { revision, stats }, EmptyGraph, Failed { kind: RunFailureKind, stage: RunPhase, message: String }, TimedOut { limit_ms: u64 }, RolledBack { cause_run_id: TransformRunId }, Skipped { reason } }` with an eight-case table, `BudgetObserver` on the (verified) runtime-observer hook, a wall-clock watchdog, a streaming cap visitor, and the chain rule "If any transform fails, times out, or returns an empty graph, discard the whole working chain. Earlier staged records become `RolledBack { cause_run_id }`; **no partial graph is published**" (c1:383).
Further from `score_4`: a "single long builtin delays cooperative abort" leaves the worker quarantined until it exits, so the runaway thread survives its run here too (c1:445) — the same residual as A1, mitigated by quarantine so "no second task can accumulate behind it". Placement: **strictly better than score_4 on the same axis** — the respect is atomicity across a multi-transform chain (`RolledBack { cause_run_id }`) and a first-class non-publishing `Unchanged`, neither of which the pole has, together with `ProcessInterrupted` recovery. → **5**.

D4: **B1 5 | A1 4 | C1 5**

### D5 — Scheduling model and the upcoming-runs surface (w 0.09)
`contrast`: "A serialisable plan evaluated by the existing poll tick, with coalesce and no-overlap
semantics written down — not a timestamp recomputed from the current clock."

**B1** — closer to `score_4`. Anchor: `enum RunPlan { Manual, Every(Duration) }` declared in the package, survives restart; evaluated by the existing `spawn_poll` tick; `MissedTickBehavior::Skip`; no overlap; the UI renders the same plan the backend evaluates. Artifact (b1:355-360, 480-488): `pub enum Trigger { AfterImport, Every { interval_ms: u64 }, Daily { at_utc: String }, Manual }` in the catalog with "60_000 ≤ interval_ms ≤ 24 h"; "`next_due_at = last_finished_at + intervalMs` (**from the ledger on boot**)"; coalesce = "exactly **one** run fires… Never replayed, never queued"; overlap "**Impossible by construction**"; and the UI is fed by `GET /runs/upcoming` with `next_due_at_ms`, not a client recomputation.
Further from `score_4`: nothing on the axis. Placement: **strictly better than score_4 on the same axis** — the respect is a *clock-anchored, persisted* plan: a fourth trigger kind (`Daily { at_utc }`) with a grace window that writes a visible `Skipped { MissedWindow }` record, a persisted `next_due_at` recovered from the ledger, and a `Suspended` state with a `Resume` affordance, none of which the pole has. → **5**.

**A1** — closer to `score_4`. Anchor: the declared, surviving plan with coalesce and no overlap written down. Artifact (a1:124-128, 432): the run plan *is* the source's declared, serialisable `WatchPlan`; "Missed intervals: **skip, never replay**"; "Filesystem bursts: **coalesce**"; "Overlap: **never**… the only cross-driver overlap is `POST /runs` vs a driver tick"; `RunSchedule { next_due_ms: AtomicU64, … }` "written only by the driver that owns the timer, from the same `tokio::time::Interval` that will fire", with "`gated: true` for alternates" and "past-due shows \"due · waiting for the driver\", never negative".
Further from `score_4`: the pole's plan is a *transform's* plan; A1 gives a transform no cadence of all — a transform runs on source refresh or manual trigger — so the upcoming-runs surface the user asked for is only the source's next refresh. A1's own schedule anchor also does not survive a restart: after a boot, `spawn_poll` re-seeds `now + interval_ms` rather than restoring an anchor, which A1 concedes is why the panel shows a re-based countdown. Lean: toward `score_4` (miss/overlap/coalesce all written down; the plan is a declared serialisable value evaluated by the existing tick; the UI renders the backend's own value). Placement: **past score_2, short of score_4** → **4**.

**C1** — closer to `score_4`. Anchor: the serialisable plan + coalesce + no overlap. Artifact (c1:433, 549-577): the catalog binding carries `"watch": { "kind": "poll", "interval_ms": 300000 }`; "`ImporterDescriptor.watch` becomes `watches: Vec<WatchPlan>` in one clean cutover… `TransformedImporter` merges and deduplicates the upstream plan with transform-owned `Static`, `Poll`, and `Push` plans"; "Poll intervals use `tokio::time::MissedTickBehavior::Skip`… `next_run_at` advances to the first future point on the original schedule anchor"; "There is no per-source overlap"; "Filesystem events observed while busy are coalesced into exactly one trailing source refresh"; and truthfulness: "Source refresh: \"on next source change,\" with no invented timestamp… Static after initial application: no upcoming row" (c1:573-576).
Further from `score_4`: the pole has none; C1's one cost is a breaking cutover of a public descriptor type that every importer must adopt — a scope cost, not a shortfall on the axis. Placement: **strictly better than score_4 on the same axis** — the respect is a *persisted* anchor across restart ("The checkpointed anchor determines the first future run", backed by `checkpoint.json` "preserves the next sequence and schedule anchors", c1:543) plus truthful non-timestamp upcoming rows. → **5**.

D5: **B1 5 | A1 4 | C1 5**

### D6 — Serving and animation integration (w 0.08) — C1 capped at 2
`contrast`: "Frame cost proportional to what changed, patched into the existing buffers under the
existing revision guard — not a whole-scene refetch and rebuild per tick."

**B1** — closer to `score_4`. Anchor: "`graph_canvas::set_positions` patches the changed ranges into the existing buffers; the `x-graph-revision` guard already in `graph_canvas::load` admits each frame; only a topology change (added/removed nodes) reloads the scene; run events ride the existing `/progress?since=<seq>` stream". Artifact (b1:566): "`begin_transition`…: the rAF loop in `render/mod.rs` scales the **sizes** of the affected subset each frame (a *new* `GraphPipelines::write_sizes_subset(queue, &[(u32, f32)])` beside `update_sizes`, `pipelines.rs:1270`, writing only contiguous runs with `queue.write_buffer`)", plus "Position carry-over keyed by **id**" replacing an index carry that "only fires on equal counts and would scramble a reorder" (b1:564), and a topology/patch distinction: the live path is a `reload_graph` ("full re-seed as today", b1:644) while scrub "mounts a **hold scene**" (b1:568).
Further from `score_4`: two of the pole's clauses are missed. (i) B1 invents a second cursor — "`GET /runs?since=<seq>`… `since` returns only records changed after that ledger `seq`" (b1:532) — and never mentions or dismisses the existing `/progress?since=<seq>` stream the pole names. (ii) It uses `x-graph-revision` as a *response header* on `/runs/:id/delta` (b1:535), not as the existing in-`graph_canvas::load` frame-admission guard. Lean: toward `score_4` (delta-proportional per-frame writes into the verified pipeline seam; per-endpoint justification against `api_routes()`). Placement: **past score_2, short of score_4** → **4**.

**A1** — closer to `score_4`. Anchor: the pole's in-place patch under the revision guard. Artifact (a1:558, 578, 582): "`pub fn pulse(members, duration_ms)`… the existing rAF loop scales `sizes_base` for members… and writes via `pipes.update_sizes` (`pipelines.rs:1270-1285`) until `t = 1`"; "`on_live_revision(rev)` then fetches `/runs?limit=1`… and calls `render::pulse(added ∪ changed, 600)`"; scrub "the rAF loop interpolates colour α… via `pipes.update_colors` (`pipelines.rs:1231-1245`)"; and a real staleness guard "`x-graph-revision: <revision_after>`… 409 when `revision_after != live`".
Further from `score_4`: (i) A1's own §1.2 states the decision "In-place animation when node sets match, remount otherwise", but §4.9.3 remounts on *every* publication and §4.9.4 rebuilds a whole union `GraphData` client-side via `graph_data_from_frame` for every scrub step — the design does not do what its decision table says; (ii) each scrub step reconstructs a complete union scene (`ids`, `node_state`, `edges`, `edge_state`) rather than applying a delta to mounted buffers, so the scrub path's per-step cost is O(graph), not O(change); (iii) no use of `/progress?since=<seq>`. Lean: none — the live path is at the pole (patch, not refetch) and the scrub path is below it. Placement: **past score_2, short of score_4** → **3**.

**C1** — closer to `score_4` on frame cost; the cap decides the number. Anchor: the pole's `set_positions`/`update_colors`/`update_sizes` patch. Artifact (c1:645): "Internally they use `mount_canvas` for the union topology and `GraphPipelines::set_positions`, `update_colors`, `update_edge_colors`, and `update_sizes` for frames. `set_selected_node` and `set_search_highlights` are reapplied after each union mount. Leaving preview remounts the live `Ctx.graph` scene. The revision poll in `main.rs` continues to call `reload_graph` after real publication." — this names the pole's API set exactly, all four verified present.
Further from `score_4`: the pole's frame-admission guard is not named (shared with the other two), no `/progress?since=<seq>` use — and, decisively for this dimension's instruction, *"is every new endpoint justified against the routes that already exist"*: C1's three new routes are justified against content shape and against each other, never against a named existing route (c1:584, 588, 592). Placement on the anchor's own axis: **past score_2, at or near score_4 on frame cost; short of score_4 on the endpoint-justification clause the same dimension asks for.** Uncapped this dimension is a 4; **CK-019's `NO` caps D6 at 2** → **2 (cap)**.

D6: **B1 4 | A1 3 | C1 2 (cap; uncapped 4)**

### D7 — Node content depth and cross-source generality (w 0.14) — B1, C1 capped at 3
`contrast`: "New fields on the shared node metadata every importer serves, including the
generated-graph path that currently discards Nix metadata — not a panel that appears for one source."

**B1** — closer to `score_4`. Anchor: `pub struct NodeMeta { .., first_seen_at: i64, last_seen_at: i64, provenance: Provenance { first_seen_run, changed_in } }` / "served by the existing `GET /node/*id`; every importer gains it, and `convert_generated_graph` threads the Nix node's attributes into frontmatter/mtime instead of `HashMap::new()` — **prost types regen via `just app-proto`**". Artifact (b1:538, 330, 660): "`proto::NodeMeta` gains `properties`, `provenance`, `edges` (+ `edges_total`, `edges_truncated`), `history`… additive protobuf fields, no second round-trip, no new store"; "`frontmatter` ← every remaining metadata key (this is the fix for \"frontmatter empty, content_readable false\")"; acceptance "a pest node, a tvix node, and an Obsidian node show the same four sections with **no per-source branch**".
Further from `score_4`: the pole names `first_seen_at`/`last_seen_at` and a `provenance` shape, and closes with the prost regen step. B1's `provenance` is never specified field-by-field, no first/last-seen timestamps appear, and the pole's last clause is absent — a `grep` for `app-proto`, `app/ui/src/proto` and `prost` across B1 returns **zero hits**, so a backend-only field addition is exactly the invisible-in-the-UI failure the item names. Placement on the axis: **matches score_4**; **CK-030's `NO` caps D7 at 3** → **3 (cap; uncapped 4)**.

**A1** — closer to `score_4`. Anchor: the pole's new `NodeMeta` fields served to every importer, with prost regen. Artifact (a1:476-485, 590-593): `repeated NodeFact facts = 29; Provenance provenance = 30; repeated uint64 touched_runs = 31;` with `NodeFact { key, value_json, field_type }` and `Provenance { source_id, transform_id, first_seen_run, last_changed_run, change, history_bounded }` — field numbers 29-31 are correct (verified: the checked-in `NodeMeta` proto ends at 28) — plus "`convert_generated_graph`… `frontmatter` = `metadata` minus the promoted keys", "Facts… for each `snap.schema.fields` entry in **declared order**", and "Regenerated into `app/ui/src/proto/` via `just app-proto` (checked-in prost types, AGENTS.md)".
Further from `score_4`: none of substance. Placement: **strictly better than score_4 on the same axis** — the respect is the instruction's own depth test, "three fields that are always populated beat eight that are empty for most nodes": A1's three additions are *derived*, not stored ("Pure function of snapshot + node; no new state", a1:591; `touched_runs` = "`runs.touched_runs(id)` over retained deltas (≤ 256 hash lookups)"), so they cannot be stale or empty and cost no extra snapshot memory, and `history_bounded: true` makes truncation visible instead of silent. → **5**.

**C1** — closer to `score_4`. Anchor: the pole's shared `NodeMeta` fields, generated path included. Artifact (c1:676-689, 691-710): `pub struct NodeLineage { pub first_seen_revision: u64, pub last_changed_run_id: Option<String>, pub last_changed_transform_id: Option<String> }` added to `NodeMeta`; "During `GraphSnapshot::build`, nodes without lineage receive the new revision as `first_seen_revision`. Matching pass-through nodes retain their prior first-seen revision"; and one component, `NodeFacts`, which "reuses/moves, rather than duplicates" identity rows, tags/doctype/folder, `frontmatter_chips`, "the complete frontmatter grid", all metrics, energy/stability/anomaly, neighbour pills, and lineage — used by **both** `nodes::focused_node` and `inspector::node_view`.
Further from `score_4`: C1's marginal *backend* depth is one three-field struct; the pole's example adds four fields plus a `changed_in` list. C1's depth is in presentation consolidation — the user-reported symptom is that "if you click on a node its not much info", and C1's fix moves the entire Inspector fact set into the Nodes panel for every source, which the pole does not do. It also omits the pole's prost-regen clause by name. Placement on the axis: **matches `score_4`**, with a different, defensible reading of "depth" (presentation reach over field count) that the instruction's "Depth matters as much as breadth" leaves open; **CK-030's `NO` caps D7 at 3** → **3 (cap; uncapped 4)**.

D7: **B1 3 (cap) | A1 5 | C1 3 (cap)**

### D8 — Impeccable surface discipline in the new panels (w 0.08) — A1, C1 capped at 2
`contrast`: "A named reduced-motion equivalent and a store-shaped loading path for the new surfaces —
not motion and loading left to implementation."

**B1** — closer to `score_4`. Anchor: "@media (prefers-reduced-motion: reduce) in `app/ui/assets/app.css` switches the playback to a stepped scrubber conveying the identical run state; `let runs = loading_store("runs", "Runs"); <LoadingGate store=runs> … </LoadingGate>`; determinate bar shows its measured %, otherwise explicitly indeterminate; styling stays in `app.css` + `panel_kit::CSS`". Artifact (b1:583-593): `crate::motion::prefers_reduced()` (web_sys, cached, re-read on the media query's `change` event) driving "`Transition.duration_ms = 0`… added and changed nodes are then held in the existing highlight set… for 2 s so the *set* that changed is legible without any motion; the Runs panel's selected-run section lists the same ids as **chips** so the information is also available as text"; three stores — `loading_store("runs", …)`, `loading_store("runs-upcoming", …)`, `loading_store("runs-scrub", …)` with a measured `update(Some(fetched as f64 / k as f64), Some(format!("{fetched}/{k} deltas")))` — two `LoadingGate`s, and the `GlobalLoadingBar` aggregation; and the node body moved to `loading_store("inspector", "loading node…")` + `LoadingGate`, with "`Spinner` stays only for the tag-browser search box" (b1:593).
Further from `score_4`: the node-body replacement is cited to `inspector.rs:37`, which is `use panel_kit::Spinner;` — the bare panel-level `Spinner { label: "loading node…" }` is at `panels/nodes.rs:754` in `focused_node`, which B1 never touches, so as written the change would not remove it. Placement: **strictly better than score_4 on the same axis** — the respect is that B1 is the only document that gives a store-shaped path to **both** surfaces the axis names (the run list *and* the node detail on click), and it supplies the non-animated equivalent twice over (highlight set **and** text chips), so the run state survives motion removal as text. → **5**.

**A1** — closer to `score_4`. Anchor: the reduced-motion equivalent + store-shaped loading. Artifact (a1:117, 136): `loading_store("runs", …)` and `loading_store("runs-frame", …)` with `begin`/`update`/`succeed`/`fail`, `LoadingGate`, `GlobalLoadingBar`, "**No hand-rolled `loaded` signals**"; and `REDUCED_MOTION` driving "a **static** 4 s emphasis via `render::push_focus_set(None, &members)`… scrub playback (▶) is disabled — step ◀ ▶ only… union-frame fade-in/out becomes fixed alpha", plus the app.css rule. A1 is additionally the only document that states ARIA: "the strip is `role=\"listbox\"` with `aria-activedescendant`; markers are `role=\"option\"`… the countdown is `aria-live=\"polite\"` updated at 1 Hz" (a1:574).
Further from `score_4`: the second named surface. A1 adds three sections to the node detail on click — "`section.nodes-facts`… `section.nodes-provenance`… `section.nodes-runs`" (a1:593) — and says nothing whatever about how that surface hydrates; the bare `Spinner` at `nodes.rs:754` remains, which is the pole's literal `score_2` symptom ("node detail: `Spinner { label: \"loading node…\" }` // reused as-is"). Lean: none — one named surface matches the pole, the other matches `score_2`. Placement: **past score_2, short of score_4**; **CK-024's `NO` caps D8 at 2** → **2 (cap; uncapped 4)**.

**C1** — closer to `score_4`. Anchor: the reduced-motion equivalent + store-shaped loading. Artifact (c1:613-625, 651-658): `loading_store("runs", "loading runs…")` with `begin`/`update`/`succeed`/`fail`, `LoadingGate`, the "already-mounted `GlobalLoadingBar`", a second `loading_store("run-transition", …)`, and "`Some(f)` is shown as a percentage; `None` stays indeterminate. Elapsed time is never converted into a fake percentage"; plus §10.4's snap-to-endpoint / static-highlight variant and the app.css rule.
Further from `score_4`: the same absent second surface — C1's node work is a pure refactor of existing Inspector markup with no statement about that surface's loading path — and, uniquely to C1 among the three, **no ARIA roles or accessibility contract anywhere** for the new Runs panel, where the axis's own question asks "are the ARIA roles and the hierarchy stated" (C1 gives a four-level prose hierarchy and nothing else, c1:604-609). Lean: none. Placement: **past score_2, short of score_4**; **CK-024's `NO` caps D8 at 2** → **2 (cap; uncapped 3)**.

D8: **B1 5 | A1 2 (cap) | C1 2 (cap)**

### D9 — Landmine risk accounting and groundedness (w 0.05)
`contrast`: "This repository's documented landmines, each paired with a countermeasure that exists in
this codebase — not generic worry words."

**B1** — closer to `score_4`. Anchor: "tvix has no fuel/step limit and eval is inline CPU-bound → spawn_blocking plus a per-run node/edge budget… wall-clock ceiling recorded as a failed run. addNode is an attrset copy so foldl' is O(n^2) → genList + one listToAttrs (the soupGen pattern). closures across 3+ import levels break → combinators take `{ graph }` explicitly". Artifact: B1 names all three, each with a real in-tree countermeasure — the op budget via `RuntimeObserver::observe_execute_op` "(`tvix/eval/src/vm/mod.rs:458-461`; the default `NoOpObserver` is what runs today)" with `observer.rs:42-76` and `EvaluationBuilder::runtime_observer` at `lib.rs:282` (b1:447) — **all three citations verified in the pinned checkout**; "no `foldl'` over `addNode` (O(n²), see soupGen…)" with a stated cost caveat (b1:189-190, 217); the closure workaround with an import-depth derivation (b1:179-186); the caps before materialisation (b1:449); `diff_snapshots`' per-node JSON cost (b1:310); `readFile` is impure (b1:148); the single-snapshot swap; `HashMap::new()` frontmatter. It is the most complete landmine list of the three.
Further from `score_4`: truthfulness. Three confident claims about this tree are false, all verified: (a) `[[schema.fields]]` written as `name = "weight"` / `type = "number"` (b1:133-137) — the real keys are `key` / `field_type`; (b) the node-load `Spinner` attributed to `inspector.rs:37` (b1:593) — that line is `use panel_kit::Spinner;`; the Spinner is at `nodes.rs:754`; (c) "`VaultEdge` carries no attributes (`vault-data/src/graph.rs:10`…)" (b1:143) — `graph.rs:10` is the `edges: Vec<VaultEdge>` field; the struct is at `edge.rs:5-7`. The dimension's instruction makes truthfulness the discriminator, so the coverage that says *4* and the three false claims that say *2* pull in opposite directions, and the evidence is genuinely mid-interval. Placement: **past score_2, short of score_4, no clear lean** → **3**.

**A1** — closer to `score_4`. Anchor: the pole's three landmines with in-tree countermeasures. Artifact (a1:64, 147, 151): "tvix has no fuel or preemption (`lib.rs:25-31`)" — verified, that exact sentence is in the file; "`spawn_blocking`… `timeout(eval_ms, …)`" plus the orphan discipline; "never a `foldl'` over `addNode` (O(n²), confirmed by the judges)… the `soupGen` pattern"; "`{ graph }`… imports nothing itself… composes at two import levels max"; "`default-features = false`… `readFile`/`readDir`/`getEnv` live in `eval/src/builtins/impure.rs`". Beyond the pole, A1 finds and fixes the biggest memory landmine in this space by itself: "the first draft's \"2 keyframes\" at 1 M nodes ≈ 2 × ~500 MB — **not acceptable**. **Fixed** with `JC_RUNS_KEYFRAME_BYTES`" (a1:653), and its §6 corrects two of its own earlier errors in print. Its `max_nodes()/max_edges()` citation (`lib.rs:54-83`) is verified exact.
Further from `score_4`: one false claim — `field_type = "integer"` in the manifest example (a1:277) is not a deserialisable `DiscoveryFieldType` (`text|keyword|date|url`, `pest.rs:224-230`). Lean: toward `score_4` (a single example-literal error against a verified-complete landmine list, plus explicit `[INFERENCE]` marking and printed self-corrections, which the dimension's instruction rewards above a document that asserts something wrong). Placement: **matches score_4** → **4**.

**C1** — closer to `score_4`. Anchor: the pole's three landmines with in-tree countermeasures. Artifact (c1:41-53, 273, 445): "repeated `addNode`/`addEdge` copies growing attrsets (`graph.nix:16-50`)" — verified, `addNode` is at `graph.nix:20-21` — with the countermeasure "one `builtins.map`/`builtins.filter` and one `builtins.listToAttrs`… following `soupGen`, `random`, and `clustered`" (verified at `graph.nix:178, 220`); "`graph-combinators.nix` already accepts `{ graph }` because tvix closures fail across three or more imports" (verified); the pure evaluator + embedded VFS + `JC_MAX_NODES = 1_000_000` / `JC_MAX_EDGES = 4_000_000` (verified at `lib.rs:43-44`); `eval_seed` at `lib.rs:402-450` (verified `let n = {n}; in (…)` at 416-419); `RawNode`/`RawEdge` discarding keys at `lib.rs:219-252` (verified `GenNode` at 221); `diff_snapshots` without the store at `merge.rs:202-234` (verified at 203); and `BudgetObserver` "installed through tvix's runtime-observer hook" (verified: `observe_execute_op` at `vm/mod.rs:461`, trait at `observer.rs:42`). Every claim I checked is true.
Further from `score_4`: the pole *leads* with "no fuel/step limit" as a named risk; C1 answers it by construction (an op budget) and by an honest-progress note — "Evaluation reports `None`/indeterminate because no reliable denominator exists" (c1:759) — but never names the absence of fuel/step limits as a landmine, and its §2 is a constraints list rather than a risk section. It also never mentions the `[[schema.fields]]` rejection at all. Lean: toward `score_4`. Placement: **past score_2, short of score_4** → **4**.

D9: **B1 3 | A1 4 | C1 4**

### D10 — Phase slicing and kill criteria (w 0.05)
`contrast`: "Phases that each land on an existing seam and are usable the day they merge, each with a
stated kill criterion — not one phase that contains the whole feature."

**B1** — closer to `score_4`. Anchor: five phases, each with a parenthetical kill, including "P5 knowledge-map update in the same change as P2 and P4". Artifact (b1:636-666): "Each phase lands on an existing seam, is usable the day it merges, and has a kill criterion that returns the team to the previous phase's behaviour **without a revert of shared types**" — Phases 0-5, each with **Ships** / **Acceptance** (a measurable threshold: "if the flattened parse exceeds 1.3× on the 1M demo"; "if injecting the 10k-node knowledge corpus costs > 2 s or > 2× snapshot memory per run"; "the other 9 950 do not move (positions_cpu diff < 1 wu)"; "no `tokio` task count growth over 1 000 runs") / **Kill**; and §11 "Knowledge-map update — **same change as the code**" with a per-note table.
Further from `score_4`: nothing on the axis. Placement: **strictly better than score_4 on the same axis** — the respect is *killability*: six phases instead of five, every acceptance criterion stated as a number that can be measured the day the phase lands, and an explicit guarantee that a fired kill returns to the prior phase's behaviour without reverting shared types. → **5**.

**A1** — closer to `score_4`. Anchor: the pole's phased list with kills. Artifact (a1:616-643): Phases 0-4 with **Lands** / **Seam** / **Acceptance** / **Kill**, plus — and this is the item's own explicit requirement, "with an explicit dependency on the types an earlier phase introduces" — a dependency table `S1…S11` with a "Depends on" column and a mermaid DAG (a1:161-185). Node content is deliberately **Phase 0**, "approach-independent, smallest, immediately useful (the judges' own hint)" (a1:684), so the reported symptom is fixed in the first merge. Knowledge map: "Landed in the **same change** as each phase (§5)" across 13 notes (a1:98-113). Phase 4's kill is honestly "`none` (documentation and read-only surfaces)".
Further from `score_4`: nothing on the axis. Placement: **strictly better than score_4 on the same axis** — the respect is the *explicit* dependency articulation the item names, which neither B1 nor C1 provides as a table, and an acceptance set that names the actual verification commands ("a 1 000-run soak in a `runs.rs` unit test… holds `records ≤ 256` and `delta bytes ≤ 64 MiB`", "browser test toggles the media emulation", "`nix flake check`"). → **5**.

**C1** — closer to `score_4`. Anchor: the pole's phased list with kills. Artifact (c1:806-846): Phases 1-4, each with **User-visible value** / **Work** / **Acceptance** / **Kill criterion**, each naming its seam ("`/node/*id` and the Nodes panel"; "the importer chain; the Runs panel from Phase 1"; "the existing watcher seam"; "`render::mount_canvas` + the Runs panel"), backed by §3's eight-step dependency list.
Further from `score_4`: nothing on the axis. Placement: **strictly better than score_4 on the same axis** — the respect is that C1 is the only document whose *phase list itself* carries the knowledge-map edits inline in each phase's work ("update `Nodes Search and Documents.md`, `Frontend.md`, and browser coverage" in Phase 1; "update `Importer Runtime.md`, `Backend API.md`, and the new `Transformations and Runs.md`" in Phase 2; c1:812, 822), which is the most faithful reading of "the same change that triggers it", and its kill criteria are gates on the design itself ("do not proceed if metadata is still lost on either native/WASM wire", "stop rollout if any configuration creates a task per transform"). → **5**.

D10: **B1 5 | A1 5 | C1 5**

---

## 3. Score calculation

| Dim | w | B1 | A1 | C1 | B1 wtd | A1 wtd | C1 wtd |
|---|---|---|---|---|---|---|---|
| D1 | 0.14 | 4 | 3 | 4 | 0.56 | 0.42 | 0.56 |
| D2 | 0.14 | 5 | 4 | 4 | 0.70 | 0.56 | 0.56 |
| D3 | 0.09 | 5 | 4 | 4 | 0.45 | 0.36 | 0.36 |
| D4 | 0.14 | 5 | 4 | 5 | 0.70 | 0.56 | 0.70 |
| D5 | 0.09 | 5 | 4 | 5 | 0.45 | 0.36 | 0.45 |
| D6 | 0.08 | 4 | 3 | 2* | 0.32 | 0.24 | 0.16 |
| D7 | 0.14 | 3* | 5 | 3* | 0.42 | 0.70 | 0.42 |
| D8 | 0.08 | 5 | 2* | 2* | 0.40 | 0.16 | 0.16 |
| D9 | 0.05 | 3 | 4 | 4 | 0.15 | 0.20 | 0.20 |
| D10 | 0.05 | 5 | 5 | 5 | 0.25 | 0.25 | 0.25 |
| **total** | 1.00 | | | | **4.40** | **3.81** | **3.82** |

`*` = dimension-local cap from a checklist NO, per `scoring.essential_fail_handling`. No global cap
was applied. No pitfall fired anywhere, so no −0.25 subtractions.

**Sensitivity of the ranking to the two contestable gate calls.** CK-024 is the one call that changes
the order: A1 = 3.97 if the node-detail clause is read as satisfied, which puts A1 second.
CK-024 for **B1** is equally contestable (its node-body answer cites the wrong file): B1 = 4.08 if
ruled NO, and B1 still ranks first. The B1-vs-C1 gap (0.58) and B1-vs-A1 gap (0.59) are far outside
the range either call can move, so the *win* is robust; the *2nd/3rd* ordering is a 0.01 artifact
decided by the specification's tie-break chain, which reaches D4 (A1 4, C1 5) before D2 (4 = 4).

---

## 4. Per-solution summary (the specification's `per_solution_report_format`)

**B1 — "Transform Packages + Run Ledger in graph-api" — total 4.40. Rank 1.**
Carried by **D2 (5)** and **D4 (5)** — the two highest-weight dimensions, both at the top of their
bands — with D3, D5, D8 and D10 also at 5. Weakest: **D9 (3)**, then D7 (3, cap). The single change
that would raise it: add "regenerate the checked-in prost types via `just app-proto`" to Phase 4 and
correct the `[[schema.fields]]` example to `key` / `field_type` (and the `Spinner` citation to
`panels/nodes.rs:754`) — that lifts D7 off its cap to its uncapped 4 and clears the D9 penalty,
≈4.7. Run-state home: a bounded `RunLedger` inside graph-api's `AppState` beside the single
`ArcSwap<GraphSnapshot>`, default-source-only, persisted under the importer packages directory with
a memory-only fallback. Non-omp worked example: `transform-relabel-hubs.toml` — retype every node of
total degree ≥ `threshold` as `hub` — on the default **Obsidian** source, with acceptance naming
Kubernetes, httpjson and `generate-clusters`.

**C1 — "transform-package importer decorator with a bounded run ledger" — total 3.82. Rank 2.**
Carried by **D4 (5)** and **D5 (5)**. Weakest: **D6 (2, cap)**, with D7 and D8 also capped. The
single change: give each of the three new endpoints a justification against a *named existing route*
— e.g. "not `/progress?since=<seq>` because a ledger sequence is not a progress sequence and carries
no stage/phase payload; not `/graph/ids` because run history is control-plane rows, not buffers" —
which lifts D6 off its cap to its uncapped 4 (≈4.14); naming `just app-proto` would add a further
0.14. Run-state home: a per-source-keyed, durable ledger under `run-ledger/v1/<encoded-source-key>/`
(JSONL + `checkpoint.json` + per-delta CBOR), finalised through `LoadResult::publication` metadata,
with the decorator holding exactly one `SourceBaseline`. Non-omp worked example: `example.annotate-nodes`
— add one declared fact to every node — run unchanged against markdown, Kubernetes and generated
fixtures.

**A1 — "transform packages + run journal in graph-api" — total 3.81. Rank 3** (a 0.01 tie with C1,
resolved by the specification's tie-break chain at D4).
Carried by **D7 (5)** and D2/D4/D5 (4 each). Weakest: **D8 (2, cap)**, then D1 (3). The single change:
give the node-detail-on-click surface the store-shaped path — replace the bare
`Spinner { label: "loading node…" }` in `nodes::focused_node` (`panels/nodes.rs:754`) with
`loading_store("node-meta", "loading node…")` + `LoadingGate` aggregated by `GlobalLoadingBar` — which
lifts D8 off its cap to its uncapped 4 (≈3.97, second place). Run-state home: an in-memory, bounded
`RunJournal` inside the per-source `AppState` holding `graph_vcs::Snapshot` keyframes and
`Vec<GraphOp>` deltas, with a records-only `runs.jsonl` under the importer overlay directory. Non-omp
worked example: `collapse-by-facet.toml` — collapse every node sharing one metadata value into a
group node carrying `member_count`, re-targeting edges — bound to `generate-clusters`.

**Disqualified: none.** No solution returned NO on CK-001, CK-002, CK-003 or CK-004.

---

## 5. Comparative analysis

### Matches
All three ride `format_version = 3` + the tvix engine, all three take `{ graph }` explicitly to dodge
the 3-import closure bug, all three build with one `listToAttrs` rather than `foldl'` over `addNode`,
all three evaluate on `spawn_blocking` next to `POST /generate`, all three keep run state out of the
single `ArcSwap`, all three state miss/overlap/coalesce, all three give a reduced-motion variant, all
three name the knowledge-map notes, and all three reach `convert_generated_graph`. The specification's
two round-1 universal blind spots (terminal failure path, coalesce semantics) are closed by all three.

### Gaps
- **`JC_RUN_WALL_MS`-style timeout does not stop a synchronous tvix eval.** Only B1 solves this
  (op counter on the VM's own `observe_execute_op`, unwind caught in the blocking closure). A1 and C1
  both disclose the residual orphan and both ship a kill criterion for it — the disclosure is
  exemplary, the mechanism is incomplete.
- **Nobody uses the existing `/progress?since=<seq>` event stream for run events**, which the
  specification's D6 anchor names. B1 adds a parallel `GET /runs?since=<seq>` with its own ledger
  `seq` without acknowledging the existing one; A1 and C1 add an independent poll.
- **Only B1 replaces the per-node JSON diff.** A1's `diff_graphs` pays the cost B1 names as
  "unaffordable at 10⁵–10⁶ nodes per run" and bounds it with a cutoff; C1 avoids the question with
  compressed stable-ID deltas and an explicit `Omitted` state.
- **Only A1 puts node-content work in Phase 0**, making the user's reported symptom ("if you click on
  a node its not much info") the first thing that merges — the cheapest possible validation of the
  whole thesis.
- **Only C1 makes the multi-transform chain all-or-nothing** (`RolledBack { cause_run_id }`) and only
  C1 states a truthful non-timestamp upcoming row ("on next source change", "waiting for trigger").

### Deviations (each defensible, but they change what gets built)
- **A1 gives a transform no cadence of its own.** A transform runs on source refresh or manual
  trigger; the "upcoming" surface shows the *source's* next refresh. This is narrower than the request
  and it is the reason A1's D5 sits at 4 — but it is coherent, and A1 says so out loud.
- **A1's input is a fresh re-import, not the live snapshot**, chosen for idempotence: "a transform is
  a *view* recomputed from the source on every run — never a fold over its own previous output"
  (a1:195). B1 takes the live published snapshot and composes transforms as `g_{t+1} = T(g_t)`
  (b1:486). C1 re-derives from a retained `SourceBaseline`. All three are defensible; they differ in
  what a periodic transform costs.
- **B1's `afterImport` chain runs *inside* `rebuild_snapshot`**, before the snapshot swap, so a
  transform's output is the import's own graph. That is the tightest coupling to the import path of
  the three, and it is what makes B1's "import is run zero" invariant work.
- **C1 cuts over `ImporterDescriptor.watch` → `watches: Vec<WatchPlan>`** — a breaking change every
  importer must adopt, in exchange for per-transform plans in the catalog.

### Mistakes (verified against the working tree)
| # | Doc | Claim | Reality |
|---|---|---|---|
| 1 | B1:133-137 | `[[schema.fields]] name = "weight"`, `type = "number"` | real keys are `key` / `field_type`; enum is `text\|keyword\|date\|url` (`packages/chembl-pharmacology.toml:52-58`, `pest.rs:216-230`). B1's example package would not validate. |
| 2 | B1:593 | the panel-level node-load `Spinner` is at `inspector.rs:37` | `inspector.rs:37` is `use panel_kit::Spinner;`. The `Spinner { label: "loading node…" }` is at `panels/nodes.rs:754` in `focused_node`. |
| 3 | B1:143 | "`VaultEdge` carries no attributes (`vault-data/src/graph.rs:10`)" | `graph.rs:10` is `edges: Vec<VaultEdge>`; the struct is `edge.rs:5-7` (the *conclusion* is right, the citation is not). |
| 4 | B1:587 | `.runs-strip .run { transition: r 200ms, fill 200ms }` | malformed CSS — `r` is not a property. Cosmetic, but it is in a spec the implementer would copy. |
| 5 | A1:277 | `field_type = "integer"` | not a deserialisable `DiscoveryFieldType`. |
| 6 | A1 §1.2 vs §4.9.3-4 | "In-place animation when node sets match, remount otherwise" | the design remounts on every publication and rebuilds a whole union `GraphData` for every scrub step. The decision table does not describe the design. |
| — | C1 | — | no factual claim I checked is false; its gap is omission (no `[[schema.fields]]` discussion, no ARIA contract, no named risk section). |

---

## 6. Ranked vote

**1. B1 (4.40)** — wins D2, D3, D4, D5, D6, D8 and D9 outright or narrowly, and is the only document
that (a) actually terminates a runaway tvix expression rather than recording that it did not, (b)
replaces the O(n)-JSON diff with an O(n+e) presence+fingerprint delta, (c) answers the node-detail
loading path at all, and (d) verifies its engine-level claims against the pinned tvix checkout to the
line. Its losses are two mechanical gates: the missing prost-regen step (D7 → 3) and three
mis-citations (D9 → 3), both cheap to fix.

**2. C1 (3.82)** — best D4 and D5 of the three on the strength of `RolledBack` chain atomicity and a
persisted schedule anchor with truthful non-timestamp upcoming rows. Loses on three gates (D6, D7,
D8), two of them for silence rather than error.

**3. A1 (3.81)** — best D7 of the field (derived, always-populated node facts/provenance on the
existing route, with the regen step named) and the most honest self-verification in the set, and it
solves the reported symptom first. It ranks third by 0.01, decided by the tie-break at D4; on the
single CK-024 reading it ranks second at 3.97. **I flag this explicitly: A1 and C1 are
indistinguishable at this resolution, and the C1-over-A1 order is a tie-break artifact, not a
substantive finding.**

---

## 7. Diversity audit (required before the ranking is reported as final)

The ranked set must contain at least two distinct run-state homes. It contains **three**:

| | Home | Durable? | Per-run payload | Keyed by |
|---|---|---|---|---|
| **B1** | process-wide ledger in `AppState`, **beside** the single `ArcSwap<GraphSnapshot>`, default-source-only | yes — `runs/` with compaction, 4096-line rotation, memory-only fallback when the packages dir is read-only | `RunDelta` (ids + kinds + edge pairs) + `Keyframe` (ids, kinds, edge index pairs) every 16 runs, ≤ 200 k nodes | `u64` `RunId`, chain-validated by `graph_hash` |
| **A1** | **in-memory journal** with the snapshot, per source | records only — `runs.jsonl`; "A1's deliberate no-durability-of-graph-data decision, kept" (a1:76) | `Vec<GraphOp>` deltas + `Arc<graph_vcs::Snapshot>` keyframes every 16 runs, byte-gated | `u64` `RunId`, opaque across process |
| **C1** | ledger **owned by the source** — per-source key, per-source directory, finalised via `LoadResult::publication` | yes — JSONL + `checkpoint.json` + atomic compaction + per-delta CBOR.zst | stable-ID deltas with tombstones and property patches; no keyframes | deterministic BLAKE3/base32 `TransformRunId` over source key + transform + sequence + schedule + package digest |

**The top two (B1, C1) resolve to different homes** — B1's is a single process-wide ledger beside the
swap; C1's is per-source, durably owned, and finalised through the decorator's publication metadata.
**The ordering is therefore CLEAN, not advisory**, and no collision needs reporting. A1's home is
closest to C1's but differs in kind (memory-only graph data vs a durable per-source format), so all
three are worth carrying into synthesis rather than one.

---

## 8. Synthesis seams

Extracted as (a) where its run state lives, (b) which file it lands in, (c) what it costs to adopt in
isolation. Each is a thing the other two do not do.

**B1 — the budget that actually terminates, plus a delta that is cheap enough to keep.**
(a) Run state in a bounded `RunLedger` in `AppState`, beside the swap, persisted under
`JUMP_CANNON_IMPORTER_PACKAGES_DIR`. (b) `crates/tvix-wasm/src/budget.rs` (`DeadlineObserver`,
`catch_unwind` in the blocking closure) and `crates/tvix-wasm/src/input.rs` (`eval_graph_with_input`
via `EvaluationBuilder::env`); `crates/graph-transform/src/delta.rs` (`presence_delta`).
(c) **Two new files, ~150 LoC, zero coupling to the package model or the UI** — the observer hook is
the engine's own (`vm/mod.rs:461`) and `env` is a builder method. Adoptable on its own, and it is the
only piece that makes a periodic unattended transform safe. Also unique to B1: the suspension/resume
lifecycle driven by `consecutive_budget_failures`, and the post-restart hash-chain validation that
stops a retained delta being applied to the wrong graph. **Lift: §5.2 and §7.2.**

**A1 — the transform as a decorator, and node facts that cannot go stale.**
(a) Run state in an in-memory `RunJournal` in the per-source `AppState`, records-only on disk.
(b) `crates/data-loader/src/lib.rs` (`pub trait GraphTransform`, `pub struct TransformedImporter`) —
wrapping an `Importer` so `HostedImporter`, capability preflight, `watch_is_authorized` and
`load_with_progress` are all untouched, which is what makes it adoptable against an importer this
repository has not written yet. (c) **One trait plus one ~40-line wrapper in an existing crate**;
the node-content half (Facts / Provenance / Runs + `just app-proto`) is independently adoptable *and is
Phase 0*, so it is the only seam in any of these documents that fixes the user's reported symptom in
the first merge. Also unique to A1: the four-gate budget with the explicit "timeout is not
cancellation" orphan discipline, and always-*derived* node facts/provenance — no new stored state, so
nothing is stale and nothing is empty. **Lift: §4.4.3 and §4.10.**

**C1 — the durable per-source ledger that is honest about what it dropped, and one Facts component.**
(a) Run state in a per-source-keyed ledger at `run-ledger/v1/<encoded-source-key>/` with a
`SourceBaseline` inside the decorator. (b) `run-ledger/v1/<key>/{ledger.jsonl, checkpoint.json,
deltas/<run-id>.cbor.zst}`; `app/ui/src/panels/node_facts.rs` (a pure extraction of existing
Inspector markup, used by both `nodes::focused_node` and `inspector::node_view`).
(c) **The ledger format is self-contained** — one writer, one compactor, one recovery path — and
adoptable without the transform model; `NodeFacts` is a *move* of code that already exists, so it is
near-zero cost with immediate user-visible effect, and it is the only seam that makes a
metadata-only generated node show its full fact set in the Nodes panel rather than only in the
Inspector. Also unique to C1: all-or-nothing chain semantics with `RolledBack { cause_run_id }`,
`DeltaAvailability::Omitted { SizeLimit }` (410) instead of a silent gap, `ProcessInterrupted`
recovery for a non-terminal run found on boot, and the only client-side ring bound that admits its
own limit (32 transitions / 64 MiB). **Lift: §7 and §11.2.**

**What none of the three supplies, and synthesis must add:** ride the existing `/progress?since=<seq>`
stream for run events instead of adding a second cursor; cite `app-proto` and the checked-in prost
field numbering in every plan that adds a `NodeMeta` field; state ARIA roles for any new panel; and
close the runaway-eval gap on the A1/C1 path (B1's `DeadlineObserver` is the only mechanism in the set
that converts a hang into a recorded `TimedOut` rather than a recorded one plus a burning thread).

---

## 9. Rules generated

### Observed issues → root cause → rule candidacy

| Issue | Five-whys root cause | Class | Recurrence test | Decision |
|---|---|---|---|---|
| B1 cites `Spinner` at `inspector.rs:37` (a `use` line) when the site is `nodes.rs:754`; cites `graph.rs:10` for a struct in `edge.rs:5-7`; writes `[[schema.fields]]` with keys that do not exist | (1) three confident citations are wrong → (2) they were written from recall, not from the file → (3) nothing in the workflow forces a read before a citation → (4) no rule requires that a `file:line` in a design or PR body be opened in the current session → (5) the repo convention of citing line numbers is undocumented and unenforced | **Systemic pattern** | cross-task: YES (any design doc, PR, or doc update in this repo); cross-project: YES; frequency: HIGH (3 of 3 documents cite lines, 5 errors total); actionable: YES; token justification: YES — these errors read as verified fact and mislead the implementer | **CREATE** |
| A1 and C1 add content to the node-detail-on-click surface and leave `Spinner { label: "loading node…" }` in `focused_node`, silently re-creating the "node has no info" bug at the loading layer | (1) loading path unaddressed → (2) the agent scoped "the new surfaces" to surfaces it created → (3) no rule says extending an existing surface's content extends its loading contract → (4) the panel-kit store path is documented in `AGENTS.md` but not bound to the "you touched this surface" trigger | **Missing convention** | cross-task: YES (any panel work in this repo); cross-project: partial; frequency: MEDIUM-HIGH; actionable: YES; token justification: MEDIUM | **CREATE** (path-scoped) |
| Retention bound unstated; terminal failure path unspecified | already covered | Already covered | — | **NO** — `.claude/rules/per-event-store-retention-bound.md` and `.claude/rules/transform-terminal-failure-path.md` already exist; do not duplicate |

### Rules created

1. `.claude/rules/verify-file-line-citations.md` — every `file:line` assertion must be opened in the
   current session before it is written; name the symbol instead when it is not.
2. `.claude/rules/async-surfaces-use-panel-kit-stores.md` — extending a surface's content extends its
   loading contract: `loading_store` + `LoadingGate` + `GlobalLoadingBar`, never a panel-level
   `Spinner` (path-scoped to `app/ui/src/**`).

---

## 10. Self-verification

| # | Question | Answer | Adjustment |
|---|---|---|---|
| 1 | **Evidence completeness** — did I read all three documents and every relevant repo file? | YES. All three read end to end (720 / 686 / 885 lines). Verified in-tree: `vault-data/src/node.rs`, `edge.rs`, `graph.rs:10`, `graph.proto` NodeMeta field numbering, `tvix-wasm/src/lib.rs` caps + `max_nodes`/`max_edges` + `check_caps_against`, `importer/src/tvix.rs:285-315`, `pest.rs:215-230`, a real `[[schema.fields]]` package, `graph-vcs/src/merge.rs:203`, `graph-api/src/watcher.rs:263-285`, `server.rs:54-120,733-742`, `render/pipelines.rs` patch API, `graph_canvas.rs:53-100`, `panels/nodes.rs:744-760`, `panels/inspector.rs:33-40`, `assets/app.css:58-66`, `panels/timeline.rs` (768 lines), `data-loader/src/lib.rs` schema types, and the pinned tvix checkout (`eval/src/lib.rs:261,282`, `eval/src/vm/mod.rs:461`, `eval/src/observer.rs:42`). | No change. Two verifications I could not perform are named as such in the report: `panels/inspector.rs:628-758` and `state.rs:49-130,209-241` (C1's citations) were not opened, and `crates/tvix-wasm/src/nix/graph-combinators.nix:112-123`'s `soupGen` body was confirmed only by name, not by line. |
| 2 | **Bias check** — am I rewarding length, tone, or confidence? | The three documents are 67 KB / 69 KB / 52 KB; B1 (longest) ranks first, C1 (shortest) second, A1 (longest of the two prose-heaviest) third — so length is not the ordering variable. I penalised B1 for three confident-but-false citations (D9 → 3) *despite* its coverage being the best, which is the opposite of an authority bias. I capped two dimensions for documents that are *silent* rather than *wrong*, and I disclosed both calls as contestable with their sensitivity. | No change. |
| 3 | **Anchor fidelity** — was every `anchor_comparison` written on the contrast axis, with both sides evidenced, before the number? | YES. Each of the 30 placements names the pole it sits nearer and the pole it sits further from, quotes the anchor text for that side, and quotes the artifact's `file:line` for that side. Two placements are explicitly cap-governed (C1 D6, A1/C1 D8, B1/C1 D7) and each states the uncapped value. | I tightened C1 D2: my first placement was 3, but the quoted evidence (four independent bounds against the pole's one, deterministic addressable ids, an explicit `Omitted` state) puts it clearly nearer `score_4` than `score_2`, and nothing in the artifact resembles the `score_2` pile — so the lean clause applies and the score is 4, with the absent keyframes named as the shortfall. |
| 4 | **Comparison integrity** — is my reference result correct, and did I introduce errors in my own analysis? | The reference was written from the spec before reading the artifacts, and every one of its ten requirements is independently checkable in the tree. The one place I could have been wrong is CK-030 for C1: I first read C1's "checked-in app proto types are regenerated" as sufficient, then applied the item's explicit "**and** name the `just app-proto` regen as a step" clause under the strictness rule. Both readings are recorded in §1, and the C1 D7 placement is reported as cap-governed with its uncapped value. | No change to the rulings; the two readings are disclosed rather than silently resolved. |
| 5 | **Proportionality** — are the scores proportional, or am I uniformly harsh/lenient? | The spread is 3.81-4.40 with per-dimension scores spread 2-5, and the dimension means are D1 3.7, D2 4.3, D3 4.3, D4 4.7, D5 4.7, D6 3.0, D7 3.7, D8 3.0, D9 3.7, D10 5.0. D10 at 5.0 across all three reflects a genuinely saturated axis (all three ship multi-phase plans with measurable acceptance criteria and kill criteria) and the specification's own round-1 evidence that this field has never done so; D6 and D8 at 3.0 reflect the two items that all three leave incomplete (the `/progress?since` stream, the node-detail loading path). I did not inflate any document on a dimension it did not earn. | No change. |

---

**Confidence: High** on D2-D5, D7-D10 and on the fatal-tier resolutions (every claim rests on a
verified file or line); **Medium** on the single gate call that decides second place (CK-024 for A1
and C1), where a defensible reader can land on the other answer and reorder A1/C1.
