# Timeline solution evaluation — B1 vs A1 vs C1

**Date:** 2026-09-25
**Judge:** TimelineEvalJudge2
**Specification:** `.specs/research/jc-timeline-eval-spec.yaml` (34 checklist items, 5 tiers; 10 weighted dimensions, `total = Σ(w_d × s_d)`, `Σw = 1.00`)
**Artifacts:**
- B1 — `.specs/research/jc-timeline-2026-09-25.solution.b1.md` ("Transform Packages + Run Ledger in graph-api")
- A1 — `.specs/research/jc-timeline-2026-09-25.solution.a1.md` ("transform packages + run journal in graph-api")
- C1 — `.specs/research/jc-timeline-2026-09-25.solution.c1.md` ("transform-package importer decorator with a bounded run ledger")

---

## Executive summary

**B1 wins (4.33), A1 second (4.01), C1 is disqualified on CK-003 (advisory total 3.95).**

All three are strong documents; the field is not weak. B1 and A1 both pass 33–34 of 34 checklist items with no dimension caps. The separation is real, not a tie-break artefact: B1's lead comes from the only in-VM fuel budget in the field, the largest terminal-outcome table, and a groundedness record I could verify line-by-line against the tree. C1 is eliminated by a single compound fatal gate, not by low scores — it is the best document on D4, D5 and node-content reuse, and I have named the one paragraph that would repair it so the synthesizer is not hobbled by a gap that is about silence rather than design.

The ranking is robust to my single most debatable call: B1's D7 cap (`just app-proto`) costs it 0.14, and B1 still leads 4.33 → 4.19 → 4.01.

---

## Stage 2 — Reference result (generated from the specification, before scoring)

A correct artifact under this spec must contain, as *checkable gates*:
1. A `format_version = 3` package carrying the transformation, with `[[parser.variables]]` bound at apply time. No new crate-per-source, no `SourceKind` variant, no `graph-api` match arm, no bespoke `--flags`.
2. Zero eval-time FS/network. Any external state as a declared, typed, bounded variable — **and** a named workaround for periodic external input.
3. A terminal-failure story covering node cap, edge cap, wall clock, thrown eval, and empty result, each a *recorded run with a reason*, cap-checked **before** admission, previous graph retained.
4. Run state outside the `ArcSwap<GraphSnapshot>`, as deltas + periodic keyframes under a stated bound, addressable per run.
5. A serialisable plan evaluated by the existing `spawn_poll` tick, with miss/overlap/coalesce written down.
6. Graph injection following the `eval_seed` idiom; Nix built with `genList` + one `listToAttrs`; `{ graph }` taken as a parameter to dodge the 3+-import closure bug.
7. Per-frame animation patching changed ranges into existing GPU buffers, with topology change distinguished.
8. Node content reaching the **generated** path (`convert_generated_graph`) and served generically through the existing `GET /node/*id` — including the checked-in prost regen.
9. Reduced-motion equivalent, measured-or-indeterminate progress, `loading_store` + `LoadingGate` + `GlobalLoadingBar`, styling in `app.css`.
10. A knowledge-map update in the same change, and a phase list where each phase lands on an existing seam with a kill criterion.
11. Zero requester vocabulary in shared core types, routes, and panels.

Anti-patterns that must be absent: a second scheduler runtime; a per-tick whole-scene refetch; dense revision-relative indices in durable deltas; a ledger that is an append-only pile of whole-graph copies; a panel section gated on a source prefix; a fabricated progress percentage; a `cron` thread.

---

## Stage 3 — Checklist results

### B1 — 33 YES / 1 NO

| CK | Tier | Ans | Deciding quote | Consequence |
|---|---|---|---|---|
| CK-001 | fatal | **YES** | §9.5 "a *new* `crate::motion::prefers_reduced()` (`web_sys::Window::match_media(\"(prefers-reduced-motion: reduce)\")`…)" over an "inline `svg` in rsx" (§9.2). No JS, no bundler, no three.js anywhere in the document. | — |
| CK-002 | fatal | **YES** | §3.1 `format_version = 3` / `[parser] engine = "tvix"` / `input = "graph"` / `[[parser.variables]] name = "threshold" type = "integer" default = "12" min = 1`. | — |
| CK-003 | fatal | **YES** | §3.4 "Per-run external state that a transform legitimately needs… is bound as a **declared, typed variable** whose catalog value is a host reference" + the `$run.index` / `$run.started_at_ms` / `$run.input_revision` / `$run.trigger` table, and "Substitution happens in `runs::executor::bind_variables` **before** `resolve_variables`… **re-binding the stored map reproduces the output**". The re-bind-per-run workaround is named and mechanised. | — |
| CK-004 | fatal | **YES** | §10 lists the permitted vocabulary and forbids the rest. A whole-document regex finds exactly 3 hit classes (§13 Q5): `session-manager` at §8 (the *existing* host that mounts `api_routes()`, not an introduced type), §10's prohibition text, and §13's own audit. `OMP Auto-Loop Topos.md` is a knowledge-note filename, not a core identifier. No type, field, route, env var, or panel string carries forbidden vocabulary. | — |
| CK-005 | essential | **YES** | §5.3 is an 11-row terminal table; every row names detection site, record status, snapshot disposition, progress row, and schedule effect. Covers input-over-budget, variable failure, eval throw, ops cap, wall cap, output-over-cap, empty output (both polarities), projection, snapshot build, arbitrary panic, and ledger disk failure. | — |
| CK-006 | essential | **YES** | §5.2 "The output caps are enforced where they already are: `check_caps_against` in `parse_graph_json`… **before any `GenNode` is materialised**", with `max = min(JC_MAX_NODES / JC_MAX_EDGES, package [limits])`. §5.1 "the previous `GraphSnapshot` is untouched unless the status is `Succeeded`". | — |
| CK-007 | essential | **YES** | §7.1 "**The bound, in one sentence**": 512 records; newest 64 deltas *and* 64 MiB; keyframe every 16 runs below 200 000 nodes; newest 4 keyframes; plus worst-case memory arithmetic (≈ 115 MiB). | — |
| CK-008 | essential | **YES** | §4.5 `RunLedger { records: VecDeque<RunRecord>, deltas: BTreeMap<RunId, Arc<RunDelta>>, keyframes: BTreeMap<RunId, Arc<Keyframe>>, delta_bytes, touched, seq }`; §7.1 keyframe "— node ids, node kinds, and edge index pairs, **never metadata or positions**". | — |
| CK-009 | essential | **YES** | Header: "a bounded `RunLedger` **beside the `ArcSwap<GraphSnapshot>` in `AppState`**"; §4.4 "`AppStateInner` gains `pub runs: Arc<runs::RunHost>`". | — |
| CK-010 | essential | **YES** | §5.1 "Every outcome is a `RunRecord` with a terminal status… the previous `GraphSnapshot` is untouched unless the status is `Succeeded`"; §6.2 a failed transform in the chain "records its failure and passes its *input* through unchanged, so one bad transform never blocks the import or the transforms after it". | — |
| CK-011 | essential | **YES** | §6.2 "This design adds **one `select!` arm** to those loops and nothing else", with the `tokio::select!` body; a `WatchPlan::Static` source with a scheduled transform gets a runs-only driver task. | — |
| CK-012 | essential | **YES** | §6.1 table: missed interval "**Coalesced**: however many intervals elapsed… exactly **one** run fires, then `next_due_at` re-arms"; overlap "**Impossible by construction**"; daily grace 1 h with `Skipped { MissedWindow }`; manual `202`/`409` with queue depth 1. | — |
| CK-013 | essential | **YES** | §4.4 `Trigger` is `#[serde(tag = "kind", rename_all = "camelCase")]` in the persisted catalog, Helm-rendered and `validate_source`-checked; §6.1 "`next_due_at = last_finished_at + intervalMs` (**from the ledger on boot**)". B1 argues the plan belongs in the catalog rather than the package and gives the in-tree precedent (`pollIntervalMs`, verified at `importer_catalog.rs:99-101`). | — |
| CK-014 | essential | **YES** | §3.3 `graph-transforms.nix` header comment: "the same tvix closure-across-3+-imports workaround graph-combinators.nix uses, `graph-combinators.nix:3-12`" plus an explicit import-depth analysis (expr→transforms = 1; expr→graph.nix→birds.nix = 2). | — |
| CK-015 | essential | **YES** | §3.2 "`EvaluationBuilder::env` (`tvix/eval/src/lib.rs:261`) binds names into the top-level scope… which is precisely what `eval_seed`'s `let n = …; in (…)` does textually (`tvix-wasm/src/lib.rs:418`)". Both citations verified exact. Input is built from the live `Arc<GraphSnapshot>`, not a re-derived copy. | — |
| CK-016 | essential | **YES** | §5.1 "the expression string, the `Rc<InputGraph>`… and the budget cross into `tokio::task::spawn_blocking`; only the `Result<GeneratedGraph, EvalFailure>` crosses back", wrapped in `catch_unwind`. | — |
| CK-017 | essential | **YES** | §9.3 "the rAF loop in `render/mod.rs` scales the **sizes** of the affected subset each frame (a *new* `GraphPipelines::write_sizes_subset(queue, &[(u32, f32)])` beside `update_sizes`, `pipelines.rs:1270`… writing only contiguous runs with `queue.write_buffer`)". The 2 s probe → `reload_graph` (verified epoch-guarded at `main.rs:1006-1017`) is the reload, not the frame. | — |
| CK-018 | essential | **YES** | §9.3 draws the split: the probe reloads the scene on revision advance (`reload_graph`, `graph_canvas::load`), and `begin_transition` is the per-frame subset patch. §12 Phase 3 acceptance "under `prefers-reduced-motion: reduce` no size interpolation frames are submitted". | — |
| CK-019 | essential | **YES** | §8 gives every route a "Why it exists" cell and defers run progress to the existing `ProgressLog`/`GET /progress` — "Progress fractions for a running run come from `/progress` events the Progress panel already polls — **no third poll**" (§9.1) — and reuses the existing revision-probe cadence. | — |
| CK-020 | essential | **YES** | §4.3 `convert_generated_graph` gains a `schema: &ImporterSchema` and writes "`frontmatter` ← every remaining metadata key (**this is the fix for \"frontmatter empty, content_readable false\"**)", `title`, `tags = [type] ++ metadata.tags`, `doctype`, `path`. §12 Phase 0 ships it. | — |
| CK-021 | essential | **YES** | §8 "`GET /node/*id` (extended) … additive protobuf fields, no second round-trip, no new store"; §12 Phase 4 acceptance "a pest node, a tvix node, and an Obsidian node show the same four sections with **no per-source branch** in `inspector.rs`". | — |
| CK-022 | essential | **YES** | §9.5 three named degraded behaviours, incl. "added and changed nodes are then held in the existing highlight set… for 2 s so the *set* that changed is legible without any motion; the Runs panel's selected-run section lists the same ids as **chips** so the information is also available as text"; CSS disabled alongside the existing rule at `app.css:62-64` (verified). | — |
| CK-023 | essential | **YES** | §9.7 table: countdown from server clock; "`Evaluating` is **indeterminate** (`fraction: None`)… no fraction is invented for tvix"; "`k/N deltas` | counted requests"; "store pending → indeterminate bar; **never a number**"; exact retention counters. | — |
| CK-024 | essential | **YES** | §9.6 `loading_store("runs")`, `loading_store("runs-upcoming")`, `loading_store("runs-scrub")` with `begin`/`succeed`/`fail`, bodies in `LoadingGate`, `GlobalLoadingBar` (verified at `main.rs:1686`); **and** it converts the node-detail wait: "The Inspector's node fetch moves from its `Spinner`… to `loading_store(\"inspector\", \"loading node…\")` + `LoadingGate`". | — |
| CK-025 | important | **YES** | §3.3 `nodesFromList = ns: builtins.listToAttrs (builtins.map (n: { name = n.id; value = n; }) ns);` with "Every builder produces its attrset in ONE listToAttrs (O(n)); no `foldl'` over addNode (O(n²), see soupGen, `graph-combinators.nix:112-116`)" (verified). `degrees` is a `//`-merge fold and B1 says so rather than hiding it. | — |
| CK-026 | important | **YES** | §15 is a 9-bullet file:line reference list; §4.5 names a new 6-file `crates/graph-api/src/runs/` module. | — |
| CK-027 | important | **YES** | §11 is a per-note edit table over 10 notes plus `docs/importer-architecture.md`, headed "same change as the code". | — |
| CK-028 | important | **YES** | §12, six phases, each with **Ships / Acceptance / Kill**, under "Each phase lands on an existing seam, is usable the day it merges, and has a kill criterion that returns the team to the previous phase's behaviour **without a revert of shared types**". | — |
| CK-029 | important | **YES** | §3.1 `transform-relabel-hubs.toml` (retype high-degree nodes as `hub`); §12 Phase 1 acceptance uses the Obsidian source, Phase 0 uses `generate-clusters.toml`, Phase 4 names "a pest node, a tvix node, and an Obsidian node". §13 Q5: "Every acceptance criterion in §12 names Obsidian, Kubernetes, httpjson, or the shipped generator packages." | — |
| CK-030 | important | **NO** | B1 adds `properties`, `provenance`, `edges`, `history` to `proto::NodeMeta` (§8, §12 Phase 4) and cites `crates/graph-api/proto/graph.proto:19-62` (verified = the `NodeMeta` message), but the document contains **no** reference to the checked-in `app/ui/src/proto/` prost types and **no** `just app-proto` step. A whole-document grep returns zero hits for either. The item's own rationale names the consequence: "A backend-only field addition is invisible in the UI — the exact class of 'node has no info' bug the user reported, re-introduced by the fix." | **caps D7 at 3** |
| CK-031 | important | **YES** | §9.5 "The strip's CSS transitions (`.runs-strip .run { transition: r 200ms, fill 200ms }`, `app.css`)… alongside the existing rule at `app.css:62-64`" (verified); §9.1 status badges via `panel_kit::badge`; no inline styles, no new visual vocabulary. | — |
| CK-032 | **pitfall** | **NO** | B1 registers a **new** `Panel::Runs` (§9.1) and never repurposes the position ring; `panels/timeline.rs` appears once, at §9.3, as a *precedent citation* for what "held" means ("the Timeline panel's paused semantics, `panels/timeline.rs:225-235` `ScrubState::Paused`" — verified exact). The defect (treating the keyframe ring as the run timeline) is absent. | — |
| CK-033 | **pitfall** | **NO** | §6.2 "This design adds **one `select!` arm** to those loops and nothing else"; §14's change table maps B1's original "ticker per transform" to "One `select!` arm on the source's existing driver task". No per-transform task, queue, or cron. | — |
| CK-034 | **pitfall** | **NO** | ~20 sampled citations verified against the tree, including four unguessable ones: `builtins/mod.rs:568` (groupBy), `eval/src/lib.rs:261` (`env`), `vm/mod.rs:461` (`observe_execute_op`), `impure.rs:70` (`readFile`) — all exact. `JC_MAX_NODES`/`JC_MAX_EDGES` = 1M/4M ✓; `NodeMeta` 11 fields ✓; caps at `check_caps_against` before materialisation ✓. **One nitpick, not a contradiction:** §9.6 cites `inspector.rs:37` for the node-fetch `Spinner`; line 37 is `use panel_kit::Spinner;` and the usage is at `inspector.rs:651`. Same file, same construct, right substance, off-by-body line number. | — |

### A1 — 34 YES / 0 NO

| CK | Tier | Ans | Deciding quote | Consequence |
|---|---|---|---|---|
| CK-001 | fatal | **YES** | §2.8 "read once at boot from `window.matchMedia(\"(prefers-reduced-motion: reduce)\")` through `web_sys` (**Rust, no JS**)". | — |
| CK-002 | fatal | **YES** | §4.3 `format_version = 3` / `[parser] engine = "tvix" role = "transform" input = "graph"` / two `[[parser.variables]]` with types and bounds. | — |
| CK-003 | fatal | **YES** | §2.11 "All other external state enters as typed `[[parser.variables]]` bound at apply time… A transform therefore has exactly two inputs — the graph and its declared variables — and is a pure function of them." The workaround is stated in §6 Q4: "Transform variables change through `PUT /importers/:id/variables`-style overlay edits, which drop and rebuild the alternate (existing behaviour) — the rebuild is a `Boot` run for that alternate. For the default source a variable change needs a restart today." | — |
| CK-004 | fatal | **YES** | §2.12 enumerates the new names, all graph vocabulary. Three hit classes only: `audience: […agent]` in a note's frontmatter (verified an existing repo convention — 10 shipped notes use it), `graph_session` at §4.9.4/§4.9.1 (an **existing** Dioxus app symbol, `main.rs:912-928 reset_graph_session`, being referenced not introduced), and `omp-auto-loop` in two *acceptance-test fixture* names (§5 Phases 0 and 2). A1's own claim is scoped to "any type, endpoint, package field, or panel string" and that scope holds. | — |
| CK-005 | essential | **YES** | §2.1 four-gate table + throw, each with a "Snapshot" column reading "previous retained", plus a boot rule (fatal for the deployment default, `503 {"status":"failed"}` for an alternate). | — |
| CK-006 | essential | **YES** | §2.1 G2 forces only the list spine: "G2 matters because `builtins.length` forces only the list spine, not each node's metadata, so a runaway output is rejected **before** `toJSON` materialises a multi-hundred-MB string** — the cheap check the current `/generate` path lacks". G3 re-checks post-parse. | — |
| CK-007 | essential | **YES** | §2.2 "The journal retains, per source, **at most `JC_RUNS_RETAIN` (default 256)**… at most `JC_RUNS_DELTA_BYTES` (default 64 MiB)… at most `JC_RUNS_KEYFRAMES` (default 2)… and only while one keyframe's estimated size fits `JC_RUNS_KEYFRAME_BYTES` (default 256 MiB)". | — |
| CK-008 | essential | **YES** | §2.2 "**Per-run full-graph copies do not exist.** Deltas age out first (byte budget), then records"; §4.6.1 keyframe every 16 runs. | — |
| CK-009 | essential | **YES** | §4.4.4 "`AppStateInner` gains `pub runs: Arc<RunJournal>`"; §2.2 "the live graph is never copied because it is already the `Arc<GraphSnapshot>` in the `ArcSwap`". | — |
| CK-010 | essential | **YES** | `RunRecord { revision_before, revision_after, … }`; §4.5 orders every failure return before `snapshot.store`; §2.1 "**Never a crash, never a hang** (the driver loop awaits the timeout, not the thread), **never a silently truncated graph** (a graph that trips a cap is discarded whole)". | — |
| CK-011 | essential | **YES** | §2.6 "Runs execute **inside the existing driver loops**; no new tokio task per transform." | — |
| CK-012 | essential | **YES** | §2.6: "**Missed intervals: skip, never replay**" (citing `MissedTickBehavior::Skip` at `watcher.rs:283`, verified); "**Filesystem bursts: coalesce**"; "**Overlap: never**" via the RAII run slot, with poll/rescan recording `Skipped { InFlight }` and filesystem/push setting `pending_change` for exactly one trailing follow-up. | — |
| CK-013 | essential | **YES** | The plan *is* the declared, serialisable `WatchPlan` on the package; §4.7 maps each driver to who writes `next_due_ms`; §2.6 Q5 "`next_due_ms` is written only by the driver that owns the timer, from the same `tokio::time::Interval` that will fire" — so the list is not recomputed at read time, and gated alternates carry `gated: true` with caveat text. | — |
| CK-014 | essential | **YES** | §2.10 "`graph-transform.nix` takes `{ graph }` explicitly (`{ graph }:` header) **exactly like `graph-combinators.nix:12`**" (verified: line 12 is `{ graph }:`), "imports nothing itself (not `birds.nix`, not `graph.nix`), and inlines its own `Y` where needed (`graph-combinators.nix:26` precedent)" (verified: line 26 is `Y = f: let x = f x; in x;`). | — |
| CK-015 | essential | **YES** | §4.2 renders `let graph = builtins.fromJSON "<json, escaped>"; in …`; §1.1 "`eval_seed` already injects host state into the expression as a `let` binding (`crates/tvix-wasm/src/lib.rs:416-419`: `let n = {n}; in (…)`), so injecting the live graph is the same idiom" (verified exact). A1 uses the anchor's idiom literally. | — |
| CK-016 | essential | **YES** | §2.1 "which runs on the same `tokio::task::spawn_blocking` path `POST /generate` and `TvixImporter::import` already use". | — |
| CK-017 | essential | **YES** | §4.9.3 live: probe → `reload_graph` (existing) → `render::pulse(added ∪ changed, 600)`, whose rAF loop "scales `sizes_base` for members by `1 + 0.6·(1 − t)` and writes via `pipes.update_sizes` (`pipelines.rs:1270-1285`)" (verified). §4.9.4 scrub fades "via `pipes.update_colors` (`pipelines.rs:1231-1245`)" (verified). | — |
| CK-018 | essential | **YES** | §4.9.3 is headed "Live: arrival pulse (**no remount beyond the one the app already does**)"; §4.9.4's `mount_canvas` is reserved for the union topology, a genuine node-set change. | — |
| CK-019 | essential | **YES** | §4.8 "Why this endpoint" column: the delta route "needs dense indices against the mounted buffers — the same shape `/search/matches` uses (`server.rs:2027-2064`)"; `/runs/:id/frame` "mirrors `/graph/ids`"; `/frame/edges` "mirrors `/graph/edges`"; `/node/*id` "**No new endpoint: the node the user clicked already has one**"; progress rides the existing `GET /progress`. | — |
| CK-020 | essential | **YES** | §4.4.2 "`convert_generated_graph` (generate role) changes in one place: `frontmatter` = `metadata` minus the promoted keys… `title` = `metadata.title` or id; `doctype` = `metadata.doctype` or `\"generated\"`". §5 Phase 0 ships it. | — |
| CK-021 | essential | **YES** | §4.8 "`GET /node/*id` | existing protobuf + `facts`, `provenance`, `touched_runs`"; §4.10.2 makes `facts` "a Pure function of snapshot + node; no new state" for every importer. | — |
| CK-022 | essential | **YES** | §2.8 four degraded behaviours: pulse → static 4 s emphasis via `push_focus_set`; "scrub playback (▶) is disabled — step ◀ ▶ only"; union fade → fixed alpha (added 1.0, removed 0.18); strip markers static, with the verified `app.css:62-64` precedent. | — |
| CK-023 | essential | **YES** | §2.9 "fraction only when `ProgressLog::latest()` reports one… the tvix eval stage has no fuel and reports **indeterminate**… **Never a synthetic percentage for eval**"; countdown maps server→client clock; past due shows "due · waiting for the driver" "rather than a negative number". | — |
| CK-024 | essential | **YES** | §2.5 `loading_store("runs")` + `loading_store("runs-frame")` with `begin`/`update`/`succeed`/`fail`, `LoadingGate`, `GlobalLoadingBar` (`main.rs:1686`, verified), "**No hand-rolled `loaded` signals.**" (A1 does *not* convert the `nodes.rs:754` Spinner it enriches — a completeness gap, not a gate failure, since it adds no new node-detail fetch.) | — |
| CK-025 | important | **YES** | §2.10 "Every builder is one `builtins.genList` + one `builtins.listToAttrs` (the `soupGen` pattern, `graph-combinators.nix:112-123`), never a `foldl'` over `addNode` (O(n²), confirmed by the judges)." | — |
| CK-026 | important | **YES** | §3's S1–S11 table names crate/file/depends-on per subproblem, with a mermaid DAG; §15-style line citations throughout. | — |
| CK-027 | important | **YES** | §2.4, 13 bullets "Landed in the **same change** as each phase (§5)". | — |
| CK-028 | important | **YES** | §5, five phases, each with **Lands / Seam / Acceptance / Kill** — A1 is the only one that names the seam as a per-phase field — and each phase header names its S-numbers against §3's explicit "Depends on" column. | — |
| CK-029 | important | **YES** | §4.3 `collapse-by-facet.toml`, "Collapse every node sharing one metadata value into one group node carrying `member_count`", and §2.12 "meaningful for Kubernetes namespaces, Obsidian folders, generated cluster types, or any pest `kind` alike". Phase 2 acceptance binds it to `generate-clusters`. | — |
| CK-030 | important | **YES** | §4.4.6 "Regenerated into `app/ui/src/proto/` via `just app-proto` (checked-in prost types, AGENTS.md)." — both halves of the compound question. (Verified: the recipe is real, at `AGENTS.md:87` and `justfile:60`.) | — |
| CK-031 | important | **YES** | §2.8 places the rule in `app/ui/assets/app.css` citing the verified `:62-64` precedent; §4.10.4 reuses `badges::frontmatter_chips`; no inline styles, no new vocabulary. A1 never states the palette/font constraint or names `panel_kit::CSS` — an omission, not a violation. | — |
| CK-032 | **pitfall** | **NO** | §4.9.5 is a dedicated distinction: "**Unchanged:** it scrubs layout-position frames (`timeline.rs:1-27`); Runs scrubs graph states. **The two are different axes and stay separate panels (one hierarchy per surface).**" (citation verified exact.) The defect is absent. | — |
| CK-033 | **pitfall** | **NO** | §2.6 "no new tokio task per transform". A1's `POST /runs` spawns one task per *manual* trigger — no interval, no independent runtime, and `try_begin` returns `409` when the slot is taken — which is not the defect the item names. | — |
| CK-034 | **pitfall** | **NO** | ~12 citations verified exact or within ~4 lines: `graph-combinators.nix:12` and `:26`, `identity.rs:79-92` ("`/` and `:` are permitted" — verified), `vault-data/src/graph.rs:70-94` (no non-empty rule — verified), `data-loader/src/lib.rs:1454-1460` (force escape hatch — verified) and `:590-605`, `watcher.rs:491-500` ("reload failed; keeping last snapshot" — verified exact), `state.rs:140-144`, `main.rs:1686`, `nodes.rs:745` and `:789-814` (the exact `nodes-focus-head`/`nodes-focus-body` insertion point — verified), `pest.rs:216-224` (`CORE_KEYS` — verified), `graph-vcs/src/merge.rs:203-234`, `app.css:62-64`, `lib.rs:416-419`. A1 also **corrects four of its own draft errors in the text** (§6 Q1–Q5), including one real factual error (`validate_local_id` and `:`) and one real memory-sizing error (2 keyframes at 1M nodes ≈ 2 × ~500 MB "**not acceptable**"). | — |

### C1 — DISQUALIFIED (CK-003 NO)

| CK | Tier | Ans | Deciding quote | Consequence |
|---|---|---|---|---|
| CK-001 | fatal | **YES** | §10.4 "A Rust helper queries `window.matchMedia(\"(prefers-reduced-motion: reduce)\")` and listens for changes." | — |
| CK-002 | fatal | **YES** | §4.1 `format_version = 3` / `[parser] engine = "tvix" input = "graph"` / two `[[parser.variables]]`. | — |
| CK-003 | fatal | **NO** | First conjunct is satisfied: §4.2 "The build keeps `tvix-eval` with `default-features = false`; impure builtins and arbitrary host I/O remain unavailable"; §4.1 "There is no implicit `now`, environment lookup, file lookup, or network lookup"; §12.11 restates it. **The second conjunct fails.** C1's only value sources are `TvixValueSource::{Binding, RunSequence, ScheduledAtMs, InputRevision}` — one static catalog value and three host-internal counters. The document never asks or answers what a transform does when it needs a value that changes between runs. A whole-document grep for `re-bind`, `rebind`, `re-import`, `external input`, `external state` returns **zero hits**. §5.2's "Re-evaluate the complete configured transform chain… from the latest upstream baseline" re-derives from *the graph*, not from a refreshed external value, and is never framed as the workaround. | **DISQUALIFIED** |
| CK-004 | fatal | **YES** | A whole-document regex for `omp\|session\|agent\|goal\|tool call\|loop iteration\|/Users/\|/home/` returns **zero hits** — the cleanest vocabulary result of the three. | — |
| CK-005 | essential | **YES** | §6 "Exact terminal behavior" — nine bullets: evaluation error, node cap, edge cap, wall clock, VM op cap, task panic, invalid output, empty output, snapshot-build failure, each with a typed record append and a retained previous snapshot. | — |
| CK-006 | essential | **YES** | §6 "Node and edge output limits are enforced by a streaming `serde` visitor **before vectors are fully materialized**… stop decoding at `limit + 1`… **never truncate and publish**." | — |
| CK-007 | essential | **YES** | §7 "retains at most 256 terminal records, at most 30 days, and at most 64 MiB per source—whichever bound prunes first", with `RunRetention` naming all four bounds. | — |
| CK-008 | essential | **YES** | §7 "it never stores per-run full graph snapshots"; §15 Q2 "The only full graphs are one source baseline, one active snapshot, and transient evaluation output—**not one per run**". | — |
| CK-009 | essential | **YES** | §7 "graph-api introduces one **source-owned** `RunLedger`"; §5.1 "The active `GraphSnapshot` remains graph-api's publication copy". | — |
| CK-010 | essential | **YES** | §5.2 — strongest of the three: "If any transform fails, times out, or returns an empty graph, **discard the whole working chain**. Earlier staged records become `RolledBack { cause_run_id }`; no partial graph is published." | — |
| CK-011 | essential | **YES** | §8.1 "graph-api still starts **one** watcher task per hosted source. It does not start a Tokio task per transform." | — |
| CK-012 | essential | **YES** | §8.2 states all three: `MissedTickBehavior::Skip` / "missed slots are not replayed" / anchored re-arm; "**There is no per-source overlap**"; filesystem events "coalesced into exactly one trailing source refresh"; push/manual "coalesce into one trailing trigger per transform"; restart "skipped; there is no catch-up storm". | — |
| CK-013 | essential | **YES** | `ImporterTransformBinding { …, watch: WatchPlan, … }` in the catalog; §8.1 one clean cutover to `watches: Vec<WatchPlan>`; §7 "`checkpoint.json` preserves the next sequence and **schedule anchors**" — the only persisted schedule anchor of the three. | — |
| CK-014 | essential | **YES** | §4.4 `graph-iter.nix` opens `{ graph }:`; §12.10 "there is no three-level captured closure". | — |
| CK-015 | essential | **YES** | §4.2 "binds it as the top-level `graph` value through `EvaluationBuilder::env`"; §2 names `eval_seed` and explains the deliberate divergence ("although graph input will use a tvix environment value rather than source-text interpolation"). The question's parenthetical — "a wrapper that binds the incoming state into scope before the expression body" — is met; only the mechanism differs, and C1 gives the reason. | — |
| CK-016 | essential | **YES** | §6 "inside the same native `spawn_blocking` boundary used by `POST /generate`"; §5.1 "The evaluator itself remains synchronous inside `spawn_blocking`, as in `POST /generate`." | — |
| CK-017 | essential | **YES** | §10.3 "Internally they use `mount_canvas` for the union topology and `GraphPipelines::set_positions`, `update_colors`, `update_edge_colors`, and `update_sizes` for frames" — `set_positions` and `update_edge_colors` are named by **no other document**. | — |
| CK-018 | essential | **YES** | §10.3 splits `mount_canvas` (union topology) from the per-frame buffer hooks, and "Leaving preview remounts the live `Ctx.graph` scene." | — |
| CK-019 | essential | **YES** | §9 gives each of its three routes a "Justification:" line (control-plane vs. bulk numeric; bounded hydration; "reusing the watcher channel preserves single-flight ordering") and preserves "The existing `X-Graph-Revision` header… after successful publication". C1 names the fewest existing routes of the three and never considers `/progress?since=<seq>` for run progress. | — |
| CK-020 | essential | **YES** | §2 "`convert_generated_graph` builds empty frontmatter"; §4.3 `GenNode.metadata` preserved and mapped; §14 Phase 1 "map it in `convert_generated_graph`". | — |
| CK-021 | essential | **YES** | §11.2 extracts `NodeFacts` into `app/ui/src/panels/node_facts.rs` and calls it from **both** `nodes::focused_node` and `inspector::node_view`; §13 "Clicking a metadata-only transformed node shows all frontmatter, metrics, lineage, and neighbors in **both Nodes and Inspector**." | — |
| CK-022 | essential | **YES** | §10.4 four degraded behaviours incl. "selecting 0 or 1 snaps immediately to the corresponding transition endpoint" and "no pulsing, tweening, or CSS transition runs". | — |
| CK-023 | important→essential | **YES** | §10.2 "Elapsed time is **never** converted into a fake percentage"; "`Some(f)` is shown as a percentage; `None` stays indeterminate"; §12.9. | — |
| CK-024 | essential | **YES** | §10.2 a literal `loading_store("runs", …)` / `begin()` / `update(None, …)` / `succeed()` / `fail(message)` block, `LoadingGate`, `GlobalLoadingBar`, plus a distinction the others lack: "A run's own failure is **data, not a hydration failure**". | — |
| CK-025 | important | **YES** | §4.4 "Every mapper/filter builds its result with one `builtins.map`/`builtins.filter` and one `builtins.listToAttrs`; none folds `graph.addNode` or `graph.addEdge` over a growing attrset." | — |
| CK-026 | important | **YES** | §2 is a 12-bullet grounded-constraint list with file:line; §4.4, §5.1, §7, §9, §11, §14 all name files and symbols. | — |
| CK-027 | important | **YES** | §12.4 names `Transformations and Runs.md` plus 8 affected notes; each phase's "Work" tail names the notes updated in that phase. | — |
| CK-028 | important | **YES** | §14, four phases each with **User-visible value / Work / Acceptance / Kill criterion**; §3 carries an 8-item ordered dependency list. | — |
| CK-029 | important | **YES** | §4.1 `example.annotate-nodes.toml`; §14 Phase 2 acceptance "the same annotation package runs unchanged against **markdown, Kubernetes, and generated fixtures**". C1's worked example is the thinnest of the three (it annotates rather than consolidates), but it is non-omp and the acceptance names three source families. | — |
| CK-030 | important | **NO** | §11.1 "The protobuf `NodeMeta` gains new fields after the existing field numbers; **checked-in app proto types are regenerated**." Accounts for the fact; names neither `app/ui/src/proto/` nor `just app-proto`. Compound question, second conjunct absent. | caps D7 at 3 (moot) |
| CK-031 | important | **YES** | §10.4 "`app/ui/assets/app.css` also disables transition/pulse animations inside the existing reduced-motion media query." No inline styles. Never states the palette/font constraint or `panel_kit::CSS`. | — |
| CK-032 | **pitfall** | **NO** | §10.1 "**do not repurpose `Panel::Timeline`, whose `FrameRing` remains layout-position history**"; §2 "It remains intact." The defect is absent. | — |
| CK-033 | **pitfall** | **NO** | §8.1 "It does not start a Tokio task per transform"; §9 `POST /transforms/:id/run` "never evaluates in the request handler and **never creates a task**"; §16 "cron is intentionally excluded". | — |
| CK-034 | **pitfall** | **NO** | Every C1 claim I sampled was true: `graph.nix:16-50` attrset copies (confirmed via `soupGen`'s own O(n²) comment), `graph.nix:151-261` `genList`+`listToAttrs` at 258-260, `graph-combinators.nix:3-26`, `lib.rs:25-44` ("tvix has no fuel / step limit"), 1M/4M caps, `eval_seed` at 402-450, `data-loader/src/lib.rs:781-795` (`ImportOutcome`), `watcher.rs` debounce + `Skip`, `nodes.rs:744-816`, `inspector.rs:628-758`, `timeline.rs:1-27`, `AGENTS.md:194-200`. No contradiction found. C1's citations are broader ranges where B1/A1 cite the exact line, but every one checked out. | — |

---

## Stage 5/6 — Rubric scores

Weights: D1 .14 · D2 .14 · D3 .09 · D4 .14 · D5 .09 · D6 .08 · D7 .14 · D8 .08 · D9 .05 · D10 .05

### D1 — Core abstraction and external implementability (w 0.14)

**B1 — score 4**
- `contrast_axis`: "The run is described as a transformation applied to a graph revision, and the transform contract is implementable from outside the repo — not a field list named after one application's loop."
- *Closer to* **score_4** — anchor: "`pub struct RunRecord { pub id: RunId, pub transform: TransformId, pub input_revision: u64, pub output_revision: Option<u64>, pub status: RunStatus, pub delta: RunDelta, }` … `fn apply(g: &Graph, bound: &Vars) -> Result<Graph>`". B1 §4.5: "`pub struct RunRecord { pub run_id: RunId, pub transform_id: String, … pub status: RunStatus, pub input_revision: Option<u64>, pub output_revision: Option<u64>, … pub delta: DeltaStats, … }`" and §4.1: "`pub trait Transform: Send + Sync { fn id(&self) -> &str; fn descriptor(&self) -> TransformDescriptor; fn apply(&self, input: &VaultGraph, ctx: &RunCtx) -> Result<TransformOutput, TransformError>; }`" in a **new wasm-clean crate** where "the trait + types build on wasm32".
- *Further from* **score_4** — anchor: `fn apply(g: &Graph, bound: &Vars) -> Result<Graph>`, a two-argument pure signature. B1 §4.1: `apply(&self, input: &VaultGraph, ctx: &RunCtx)`, where `RunCtx` carries `budget: RunBudget` **and** `progress: &'a dyn data_loader::ImportProgress`. A third party must supply a progress sink and a budget before it can be called — a strictly larger surface than the anchor's.
- *lean*: toward score_4 (the record is the anchor's field list and the contract is a real trait in a separately buildable crate; the `RunCtx` burden is the only shortfall, and it is a small one).
- *placement*: matches score_4 → **4**

**A1 — score 4**
- *Closer to* **score_4** — same anchor text. A1 §4.4.3: "`pub trait GraphTransform: Send + Sync { fn descriptor(&self) -> TransformDescriptor; fn apply<'a>(&'a self, input: LoadResult, progress: &'a dyn ImportProgress) -> ImportFuture<'a, Result<LoadResult, ImportError>>; }`" with "`pub struct TransformDescriptor { pub id, name, version, transform_id, fields, edge_types, limits }`", in `crates/data-loader` — the shared source-neutral contract crate, so an external implementer depends on one contract crate.
- *Further from* **score_4** — the anchor's run is "a transformation applied to a graph revision". A1's run is a **source rebuild**: §7's change table is explicit — "**Runs = transform applications** | Every rebuild is a run (`transform_ids` may be empty)", and `RunRecord { …, transform_ids: Vec<String>, … }` is plural. A run that transforms nothing is still a run.
- *lean*: toward score_4. The contract half matches the anchor; the run-unit half is half a step off it.
- *placement*: matches score_4 → **4**

**C1 — score 3 (advisory)**
- *Closer to* **score_2** — anchor: `struct RunRecord { id: String, session: String, tool: String, graph: serde_json::Value }`, a bare struct with no contract. C1 §5.1: "`pub struct GraphTransform { pub id: TransformId, pub package: ValidatedPackage, … }`" and §7: "`pub struct RunRecord { pub id: TransformRunId, pub transform_id: TransformId, pub sequence: u64, …, pub input_revision: u64, pub output_revision: Option<u64>, …, pub delta: DeltaAvailability, }`". C1's record is the field list — a concrete struct, exactly the *form* score_2 has, and what the axis calls "a field list".
- *Further from* **score_4** — the anchor's second clause: "a third party implements `Transform`". C1 has **no trait**. `TransformedImporter { transforms: Vec<GraphTransform> }` takes concrete structs, and `apply` is described only in prose: "`GraphTransform::apply` is asynchronous only because it awaits the native blocking task." An external implementer cannot supply a Rust transform at all — the sole extension point is a tvix package. The contrast line is a conjunction; C1 satisfies the run half and fails the contract half.
- *lean*: none. The record half is at score_4, the contract half is nearer score_2.
- *placement*: past score_2, short of score_4 → **3**

### D2 — Run-state location, storage shape, and bound (w 0.14)

**B1 — score 5**
- *Closer to* **score_4** — anchor: `RunLedger { cap: 512, keyframe_every: 32, runs: VecDeque<RunRecord> }`, `RunDelta { added, removed, changed }`, "ring retention of 512". B1 §7.1: "retains at most **512 run records**… keeps the full `RunDelta` only for the **newest 64 runs and at most 64 MiB of deltas**… writes a **`Keyframe`**… after every import run and after every 16th run… retaining the **newest 4 keyframes**", and §4.5's `RunLedger` carries all four bounds as fields, "beside the `ArcSwap<GraphSnapshot>` in `AppState`".
- *Further from* **score_4** — the anchor's last clause: "output admitted as its own revision, **never merged into the live snapshot**". B1 does the opposite: §5.1 "the previous `GraphSnapshot` is untouched unless the status is `Succeeded`" implies a successful run *becomes* the live snapshot, and history is reconstructed by walking deltas back from live or from a keyframe (§7.2, §9.3).
- *lean*: toward score_4, and past it. Three named bounds become four, the worst-case memory arithmetic is stated (≈ 115 MiB), and §7.2 adds a rule the anchor lacks entirely: after a restart a delta chain is **live-scrubbable** (links to the live hash), **keyframe-scrubbable** (links to a retained keyframe), or **dropped** with `delta_retained = false` — "nothing is misapplied to the wrong graph, and the panel says which runs can be scrubbed."
- *placement*: strictly better than score_4 on the same axis → **5**

**A1 — score 4**
- *Closer to* **score_4** — same anchor. A1 §4.4.4: `RunJournal { budget: JournalBudget, …, inner: Mutex<JournalInner> }` with `JournalBudget { max_records: 256, max_delta_bytes, keyframe_every: 16, max_keyframes: 2, keyframe_bytes: 256 MiB, diff_max_nodes: 500_000, big_run_fraction: 0.25 }`, and §2.2 "**Per-run full-graph copies do not exist.**"
- *Further from* **score_4** — the anchor's `RunDelta { added, removed, changed }` and its id/kind-only keyframe. A1's deltas are `Vec<graph_vcs::GraphOp>` (carrying full `VaultNode` clones) and its keyframes are `Arc<graph_vcs::Snapshot>` — full graph copies, which is why A1 needs the `keyframe_bytes` gate at all, and why §6 Q2 had to retract them: "the first draft's '2 keyframes' at 1M nodes ≈ 2 × ~500 MB — **not acceptable**." Above `diff_max_nodes` the delta is lossy (`nodes_changed: None`).
- *lean*: toward score_4. A1 adds a decision the anchor lacks — the **big-run rule** (§4.6.1: a run touching >25 % of nodes takes a keyframe *instead of* a delta, "the ops list would cost about as much as a keyframe") — which is a genuine advance along the bound axis, but its keyframe shape is a step behind the anchor's and it buys correctness with an extra gate.
- *placement*: matches score_4 → **4**

**C1 — score 4 (advisory)**
- *Closer to* **score_4** — anchor: "A bounded, addressable run ledger of deltas with periodic keyframes, living outside the single-snapshot swap". C1 §7: `RunRetention { max_terminal_records: 256, max_age: 30 days, max_bytes: 64 MiB, max_delta_bytes: 8 MiB }` and a delta that is "stable IDs, minimal node tombstones (`id`, `type`, and fact hash), changed-property patches, and added/removed edge endpoint pairs. It is **O(changes), not O(graph) by construction**" — the best delta shape of the three — plus the only **age** bound and the only **durable delta** store (`deltas/<run-id>.cbor.zst`).
- *Further from* **score_4** — the anchor names "periodic keyframes". C1 has **none**. Its answer to an oversized delta is `DeltaAvailability::Omitted { SizeLimit }` — the delta is dropped and the UI "states that the transition detail was not retained instead of pretending to animate it". That is more honest than a keyframe at 1M nodes, but it means C1 cannot scrub arbitrarily far back on a large graph, which is a retreat from the anchor along this axis.
- *lean*: toward score_4, short on the keyframe half.
- *placement*: matches score_4 → **4**

### D3 — Package model and tvix feasibility (w 0.09)

**B1 — score 5**
- *Closer to* **score_4** — anchor: `format_version = 3`, `[parser] engine = "tvix"`, `expr = "let graph = import /jc/src/graph.nix {}; in gcl: …"`, "host binds the live graph the way eval_seed does", "nodes built with genList + one listToAttrs, not foldl' over the attrset copy". B1 §3.1–3.3 matches all five, and §3.2 replaces the anchor's textual `let` with the *native* form: "`EvaluationBuilder::env` (`tvix/eval/src/lib.rs:261`) … which is precisely what `eval_seed`'s `let n = …; in (…)` does textually" — **both citations verified exact**.
- *Further from* **score_4** — the anchor does not answer termination. B1 does, and better: §5.2 implements tvix's `RuntimeObserver::observe_execute_op` (`vm/mod.rs:461`, verified) as a `DeadlineObserver` that "counts ops, and every 4 096 ops (on native) reads `Instant::now()`", panics with a typed payload, and is caught by `catch_unwind` in the blocking closure — "**No orphaned thread, no hung driver task, no partial graph.**" B1 also keeps laziness the anchor's eager `genList` form gives up (lazy `nodes`/`edges` thunks, one `NixAttrs::from_iter` each).
- *lean*: strongly toward score_4, and past it.
- *placement*: strictly better than score_4 on the same axis → **5**

**A1 — score 4**
- *Closer to* **score_4** — A1 §4.2 renders the anchor's idiom *literally*: `let graph = builtins.fromJSON "<json, escaped>"; in …`, and §2.10 uses the anchor's exact construction rule with the in-tree precedent at the verified line.
- *Further from* **score_4** — the anchor's graph arrives as a binding; A1's arrives as a **graph-sized string literal embedded in the expression source**. Both B1 (§3.2: "either a JSON-in-`''`-string escape layer or a giant Nix literal to parse") and C1 (§4.2: "The graph is not serialized into that string") identify this as the thing to avoid, and A1 does not address the cost. A1's `[limits] nodes` bounds it, so it is bounded rather than unbounded — but "would it finish?" at 1M nodes is a real question the document leaves open.
- *lean*: toward score_4.
- *placement*: matches score_4 → **4**

**C1 — score 4 (advisory)**
- *Closer to* **score_4** — matches all three of the anchor's conjuncts and adds the same native `env` binding B1 uses, with §4.4 stating the construction rule explicitly.
- *Further from* **score_4** — the anchor prints its Nix; C1 writes `graph-iter.nix` as `mapNodes = f: g: …;` elisions. The construction *rule* is stated ("one `builtins.map`/`builtins.filter` and one `builtins.listToAttrs`") but the reader cannot check the bodies, where A1's table of typed signatures and B1's full listing both let them.
- *lean*: toward score_4.
- *placement*: matches score_4 → **4**

### D4 — Run lifecycle and terminal outcomes (w 0.14)

**B1 — score 5**
- *Closer to* **score_4** — anchor: `RunFailure::NodeCap{..} | EdgeCap{..} | Timeout{..}`, "eval under spawn_blocking; node/edge count checked … BEFORE admission; a wall-clock ceiling converts a hang into `Failed { Timeout }`; an empty result is a recorded, addressed state". B1 §5.2's cap check is "**before any `GenNode` is materialised**" (verified: `check_caps_against` lives in `parse_graph_json`), and §5.3's `RunStatus` carries `Failed { reason: FailureReason }` / `TimedOut { ops, elapsed_ms, kind: BudgetKind }` / `Rejected { reason: RejectReason }` / `Skipped { reason: SkipReason }` with `snapshot: kept` on every failure row. The hang is genuinely converted: `DeadlineObserver` → typed payload → `catch_unwind` → `TimedOut { kind: Ops | Wall }` and the thread returns to the pool.
- *Further from* **score_4** — the anchor's empty result is "a recorded, addressed state, not a blank canvas"; B1 makes empty output a **configurable policy**: `allow_empty = false` → `Failed { EmptyOutput }`, `allow_empty = true` → a published empty snapshot plus the UI line "run #N emptied the graph — scrub to #N−1 to view the previous state". Two behaviours where the anchor states one.
- *lean*: strongly toward score_4, and past it. Eleven named terminal paths against the anchor's three; plus a schedule effect per path (three budget failures in a row → `Suspended`, with a **visible** `Resume` and a `Skipped { Suspended }` marker record).
- *placement*: strictly better than score_4 on the same axis → **5**

**A1 — score 4**
- *Closer to* **score_4** — §2.1's four-gate table, each with "Snapshot: previous retained", pre-admission by construction, and §2.3's nine-row UI table. G2 is an addition the anchor lacks: forcing only the list spine "before `toJSON` materialises a multi-hundred-MB string — the cheap check the current `/generate` path lacks."
- *Further from* **score_4** — the anchor requires that "a wall-clock ceiling converts a hang into `Failed { Timeout }`". A1 states the opposite about the underlying evaluation: "**Timeout is not cancellation.** tvix has no fuel or preemption (`lib.rs:25-31`), so on G4 **the blocking thread keeps running**." The ceiling converts the *wait*; the hang persists, quarantined. A1 is honest about this and bounds it (one orphan per transform, ever) and adds a kill criterion to move eval to a subprocess — but the contrast clause is not met. Its empty-output policy also runs the other way: an empty graph **is** published (justified from `VaultGraph::validate` having no non-empty rule — verified at `graph.rs:70-93`).
- *lean*: toward score_4 for three of the four named terminal paths; clearly short on the fourth.
- *placement*: matches score_4 → **4**

**C1 — score 5 (advisory)**
- *Closer to* **score_4** — anchor: "an empty result is a recorded, addressed state, **not a blank canvas**". C1 §6: "**Empty output:** append `EmptyGraph`; treat it as a terminal, **non-publishing** outcome rather than replacing a useful graph with an accidental blank canvas." That is the anchor's clause, verbatim in intent. The cap check is "a streaming `serde` visitor before vectors are fully materialized" with "stop decoding at `limit + 1`", and the op cap is an in-VM `BudgetObserver` on the same hook B1 uses, with `TimedOut`, `OperationLimit`, `TaskPanic` and `Publication` outcomes.
- *Further from* **score_4** — the anchor's single `RunStatus` enum; C1 splits it across `RunState`/`RunOutcome`/`RunFailureKind`/`RunPhase`. More machinery for the same axis, not less — no shortfall, but no advance either.
- *lean*: strongly toward score_4, and past it. Two terminal paths neither other document has: **chain rollback** (§5.2 "Earlier staged records become `RolledBack { cause_run_id }`; no partial graph is published") and **cross-restart recovery** (§7 "any recovered nonterminal run becomes `Failed { kind: ProcessInterrupted }`").
- *placement*: strictly better than score_4 on the same axis → **5**

### D5 — Scheduling model and the upcoming-runs surface (w 0.09)

**B1 — score 4**
- *Closer to* **score_4** — anchor: `enum RunPlan { Manual, Every(Duration) }` "declared in the package, survives restart", "evaluated by the existing spawn_poll tick … `MissedTickBehavior::Skip` … no run overlaps itself; the UI renders the same plan the backend evaluates". B1 has a `Trigger { AfterImport, Every{interval_ms}, Daily{at_utc}, Manual }` that is `#[serde]`-serialised in the persisted catalog and `validate_source`-checked (§4.4), re-armed "from the ledger on boot" (§6.1), driven by a `select!` arm on the existing task (§6.2), and rendered by `GET /runs/upcoming` returning the same `next_due_at_ms` the backend evaluates (§8).
- *Further from* **score_4** — the anchor is one plan, one policy. B1 is three trigger kinds × three driver kinds × a manual channel × a suspension state machine, and its re-arm is **finish-time** ("`next_due_at = last_finished_at + intervalMs`"), so cadence drifts under a slow run where an anchored re-arm would not. B1 states the consequence honestly ("A run that overruns its interval simply delays the next by its overrun"), but it is a larger surface to keep correct than the anchor needs.
- *lean*: toward score_4.
- *placement*: matches score_4 → **4**

**A1 — score 3**
- *Closer to* **score_4** — anchor: "a serialisable plan evaluated by the existing poll tick, with coalesce and no-overlap semantics written down". A1 §2.6 delivers all three semantics with the in-tree precedent cited at the verified line, an RAII run slot for no-overlap, and a driver/`next_due_ms` ownership table (§4.7).
- *Further from* **score_4** — the anchor requires the plan to survive a restart. A1's *plan* does (it is the package's `WatchPlan`), but the *derived due instant* is an in-memory `AtomicU64` written after each tick with **no persisted anchor** and no statement of what the panel shows across a restart — which is precisely the "cannot show a missed run" case the item's rationale names. Second shortfall: A1's transform has **no plan of its own**; its cadence *is* the source's cadence, so a periodic "dream consolidation" on a different tick than the source's has no expression in the model.
- *lean*: toward score_4, with two quoted shortfalls against it.
- *placement*: past score_2, short of score_4 → **3**

**C1 — score 5 (advisory)**
- *Closer to* **score_4** — all three of the anchor's clauses, and past them. Per-transform `watch: WatchPlan` in the serialised catalog; one task; the full miss/overlap/coalesce set written down with the verified `MissedTickBehavior::Skip` precedent; and an upcoming surface that never invents a time ("Source refresh: 'on next source change,' with no invented timestamp. Push: 'waiting for trigger.'").
- *Further from* **score_4** — the anchor's plan is "declared in the package"; C1's is declared in the catalog and realised by a `watch` → `watches: Vec<WatchPlan>` cutover of `ImporterDescriptor`, a breaking trait change touching every importer. That is more machinery for the same property, and a real migration cost.
- *lean*: toward score_4, and past it on the same axis. Two same-axis respects the anchor lacks: (a) **anchored** re-arming — "next_run_at advances to the first future point on the original schedule anchor, so cadence does not drift" (B1's finish-time re-arm does drift); (b) a **persisted** anchor — "`checkpoint.json` preserves the next sequence and schedule anchors" — so the upcoming row is truthful across a restart, which neither other document achieves.
- *placement*: strictly better than score_4 on the same axis → **5**

### D6 — Serving and animation integration (w 0.08)

**B1 — score 4**
- *Closer to* **score_4** — anchor: "per frame: `graph_canvas::set_positions` patches the changed ranges into the existing buffers; the x-graph-revision guard already in `graph_canvas::load` admits each frame; only a topology change reloads the scene". B1 §9.3: per-frame work is `GraphPipelines::write_sizes_subset(queue, &[(u32, f32)])` "writing only contiguous runs with `queue.write_buffer`", beside `update_sizes` at the verified `pipelines.rs:1270`; the scene reload is the separate 2 s probe → `reload_graph` (verified epoch-guarded at `main.rs:1006-1017`). §12 Phase 3 makes frame cost testable: "after a run that changes 50 of 10k nodes, the other 9 950 do not move (positions_cpu diff < 1 wu)".
- *Further from* **score_4** — the anchor's last clause: "run events ride the existing `/progress?since=<seq>` stream". B1 does not; it adds `GET /runs?since=<seq>` with its own ledger `seq` cursor — a second cursor over an existing mechanism. B1's justification (the seq scopes to run records, and a running record is re-sent on completion) is real, but it is a second clock by another name.
- *lean*: toward score_4.
- *placement*: matches score_4 → **4**

**A1 — score 4**
- *Closer to* **score_4** — §4.9.3 live: `render::pulse(added ∪ changed, 600)`, whose rAF loop "writes via `pipes.update_sizes` (`pipelines.rs:1270-1285`)" (verified); §4.9.4 fade "via `pipes.update_colors` (`pipelines.rs:1231-1245`)" (verified). §4.9.3 is explicitly headed "no remount beyond the one the app already does", and the remounts it does perform are topology changes.
- *Further from* **score_4** — the anchor's clause is "frame cost proportional to the delta **or** to the graph". A1's *frames* are delta-proportional in both paths, but each **scrub step** is a `mount_canvas` of a union topology rebuilt from `/runs/:k/frame` + `/frame/edges` — O(graph) per step — and above `keyframe_bytes` (256 MiB) A1 disables scrub entirely (`scrub: "deltas_only"`). A1's delta wire is also **dense `u32` indices** into `revision_after`'s ordering with a 409 when stale, which is the durable-index hazard C1 and B1 both design out.
- *lean*: toward score_4.
- *placement*: matches score_4 → **4**

**C1 — score 4 (advisory)**
- *Closer to* **score_4** — §10.3 names the anchor's patch primitives and one more: "`GraphPipelines::set_positions`, `update_colors`, `update_edge_colors`, and `update_sizes` for frames", with `mount_canvas` reserved for "the union topology" (an explicit topology case) and "The revision poll in `main.rs` continues to call `reload_graph` after real publication". C1 is the only document that names `set_positions`.
- *Further from* **score_4** — the anchor's "run events ride the existing `/progress?since=<seq>` stream"; C1 carries per-run `progress: Option<f32>` inside `/runs` and never mentions `/progress`. And C1's honest limitation is a retreat in reach: "it does not claim to materialize every historical full graph."
- *lean*: strongly toward score_4; it misses one anchor clause (the event stream) while exceeding on another (honest labelling of unavailability). Not a clean 5.
- *placement*: matches score_4 → **4**

### D7 — Node content depth and cross-source generality (w 0.14)

**B1 — score 3 (capped by CK-030)**
- *Closer to* **score_4** — anchor: "`NodeMeta { .., first_seen_at, last_seen_at, provenance { first_seen_run, changed_in } }` … every importer gains it, and `convert_generated_graph` threads the Nix node's attributes into frontmatter/mtime instead of `HashMap::new()`". B1 §4.3 does the conversion clause — "`frontmatter` ← every remaining metadata key (**this is the fix for \"frontmatter empty, content_readable false\"**)" — and adds `properties`/`provenance`/`edges`/`history` on the shared metadata served by the existing `GET /node/*id`; §12 Phase 0 is node content **first** and also lifts the `[[schema.fields]]` rejection (verified at `tvix.rs:293-307`) so generated metadata can be faceted and searched, which is what keeps `properties` non-empty.
- *Further from* **score_4** — the anchor's **last clause**: "prost types regen via `just app-proto`". B1 has no such step; a grep for `app-proto` and `app/ui/src/proto` returns zero hits across the document. This is the anchor clause B1 misses, and it is the clause that makes the other three real.
- *lean*: toward score_4 on substance; the cap is the specification's, not mine.
- *placement*: past score_2, short of score_4 on the regen clause → **3** (CK-030 NO caps at 3)

**A1 — score 4**
- *Closer to* **score_4** — A1 matches every clause, including the one B1 misses: §4.4.6 "Regenerated into `app/ui/src/proto/` via `just app-proto` (checked-in prost types, AGENTS.md)." (Recipe verified real at `AGENTS.md:87` / `justfile:60`.) Phase 0 ships node content **first**, §4.10.2 makes `facts` "a Pure function of snapshot + node; no new state" for every importer, and §5 Phase 0's kill criterion targets the one risk B1's design does not name: "if for Obsidian ≥ 90 % of Facts rows duplicate the Inspector frontmatter grid, merge Facts into the Inspector's grid header instead of a second section."
- *Further from* **score_4** — the anchor's `first_seen_at: i64, last_seen_at: i64`. A1 carries `first_seen_run: Option<u64>, last_changed_run: Option<u64>` — run ids, not instants. Run ids are strictly more useful for the scrub-to-run UI A1 builds (§4.10.4 makes each id a button calling `scrub_to`), but you cannot render "last changed 3 days ago" from them without a lookup, and there is no `last_seen_at` at all.
- *lean*: toward score_4, and past it on the axis. `Provenance.history_bounded: bool` is the same-axis respect the anchor lacks: it makes the *incompleteness* of per-run provenance a first-class wire state ("true when the retained deltas do not reach the node's first appearance") rather than a field that silently lies for some nodes. A shared field set that is wrong for some sources is not "new fields on the shared node metadata **every importer serves**" in the useful sense.
- *placement*: strictly better than score_4 on the same axis → **4**

**C1 — score 3 (capped; would be 3 on merit)**
- *Closer to* **score_4** — the anchor's structural clause: shared optional `NodeMeta` field, one component used by both surfaces. C1 §11.2 extracts `NodeFacts` out of `inspector::node_view` and calls it from **both** `nodes::focused_node` and `inspector::node_view`; §11.1's `NodeLineage` is `Option` on the shared `NodeMeta`; Phase 1's kill criterion names the anti-pattern directly ("do not proceed if … **Facts duplicates rather than reuses** Inspector behavior"). C1 is the only document that answers the duplication risk by construction rather than by a kill criterion.
- *Further from* **score_4** — depth. One optional lineage struct cannot express declared facts, an edge list, or per-run history; and `first_seen_revision: u64` is a process-local counter that resets on restart, so it is not stable identity. Plus the regen clause is absent ("checked-in app proto types are regenerated" names neither the path nor the command).
- *lean*: none decisive; capped at 3 by CK-030 regardless.
- *placement*: past score_2, short of score_4 → **3**

### D8 — Impeccable surface discipline (w 0.08)

**B1 — score 4**
- *Closer to* **score_4** — anchor: "`@media (prefers-reduced-motion: reduce)` in `app/ui/assets/app.css` switches the playback to a stepped scrubber conveying the identical run state; `let runs = loading_store(\"runs\", \"Runs\"); <LoadingGate store=runs> … </LoadingGate>`; determinate bar shows its measured %, otherwise explicitly indeterminate; styling stays in app.css + `panel_kit::CSS` on the existing palette". B1 hits every clause: §9.5's `crate::motion::prefers_reduced()` with a `change` listener, three named degraded behaviours, and text chips so "the information is also available as text"; §9.6's three `loading_store`s + `LoadingGate` + `GlobalLoadingBar` (verified at `main.rs:1686`); §9.7's measured/indeterminate table.
- *Further from* **score_4** — the anchor's last clause, "`panel_kit::CSS` on the existing palette": B1 never states the palette/font constraint and never names `panel_kit::CSS`. It does place styling in `app.css` (with the verified `:62-64` precedent) and reuses `panel_kit::badge` for status, so the operative requirement holds — but the strip's `.runs-strip .run` class is the one place a bespoke look could appear, and B1 does not say it stays on-palette.
- *lean*: toward score_4. B1 exceeds the anchor on the store path (three stores, and it converts the node-detail `Spinner` the anchor shows as "reused as-is"), and falls short only on an unstated palette constraint the anchor also does not demonstrate.
- *placement*: matches score_4 → **4**

**A1 — score 4**
- *Closer to* **score_4** — all of §9.5's clauses, via §2.8/§4.9: `REDUCED_MOTION: GlobalSignal<bool>` from `web_sys` "(Rust, no JS)", four named degraded behaviours incl. the substitute control ("scrub playback (▶) is disabled — step ◀ ▶ only"), §2.5's store path, §2.9's progress rule. A1 also states a complete ARIA hierarchy the anchor is silent on: "the strip is `role=\"listbox\"` with `aria-activedescendant`; markers are `role=\"option\"`; ◀ ▶ are buttons with labels; the countdown is `aria-live=\"polite\"` updated at 1 Hz."
- *Further from* **score_4** — the anchor's store-shaped path. A1 leaves the `nodes.rs:754` `Spinner { label: "loading node…" }` (verified present) in place, so the exact wait CK-024 names survives on the surface A1 is enriching. A1 also never states the palette constraint.
- *lean*: toward score_4. The ARIA statement and the explicit "one hierarchy per surface" rule are beyond the anchor; the un-converted Spinner is a concrete miss on the axis. Mixed, so not a clean 5.
- *placement*: matches score_4 → **4**

**C1 — score 4 (advisory)**
- *Closer to* **score_4** — both anchor clauses, concretely: §10.2 a literal `loading_store` block with all four actions plus `LoadingGate`/`GlobalLoadingBar`, and §10.4 a matchMedia helper with a change listener and four named degraded behaviours ("selecting 0 or 1 snaps immediately to the corresponding transition endpoint"). §10.2's distinction — "A run's own failure is **data, not a hydration failure**: a successful `/runs` response calls `store.succeed()` and renders the terminal row" — is a hydration-design idea neither other document has.
- *Further from* **score_4** — the anchor requires a *substituted* equivalent; C1 "hides" play/auto-advance rather than substituting for it, and states no ARIA roles or labels anywhere, and no palette constraint. Those are dimension sub-clauses rather than contrast-line clauses, so they cap the merit below a clean 5 but do not move the placement.
- *lean*: toward score_4.
- *placement*: matches score_4 → **4**

### D9 — Landmine risk accounting and groundedness (w 0.05)

**B1 — score 5**
- *Closer to* **score_4** — anchor: "tvix has no fuel/step limit and eval is inline CPU-bound -> spawn_blocking plus a per-run node/edge budget … wall-clock ceiling recorded as a failed run. addNode is an attrset copy so foldl' is O(n^2) -> genList + one listToAttrs. closures across 3+ import levels break -> combinators take `{ graph }`". B1 pairs **every** named landmine with an in-tree countermeasure *and a verified citation*: §3.4 the pure build (`impure.rs:70` — verified exact) → env binding; §5.2 no fuel (`vm/mod.rs:461` — verified exact) → `DeadlineObserver`; §3.3 O(n²) (`graph-combinators.nix:112-116` — verified) → one `listToAttrs`; §3.3 closures (`graph-combinators.nix:3-12`) → `{ graph }`; §1 the single-snapshot swap → `RunHost` beside it; §5.2 the 1M/4M caps (verified) → `min(JC_MAX_NODES/JC_MAX_EDGES, package [limits])`.
- *Further from* **score_4** — the anchor is three compact risks; B1 is a 12-row matrix plus a five-question self-verification. More volume is not more axis, so this is not the shortfall. The one place B1 knowingly leaves a landmine live is named: "`degrees` uses `//` per edge on a flat integer map; at 1M edges that is 1M small merges — acceptable for the Nix tier."
- *lean*: strongly toward score_4, and past it. The same-axis respect: B1's statements about this tree are **verifiably true at a rate I could not approach with any other document** — four citations (`builtins/mod.rs:568`, `eval/src/lib.rs:261`, `vm/mod.rs:461`, `impure.rs:70`) could not have been guessed and all landed exactly. And B1 raises a landmine neither other document names, with a countermeasure: "Remaining risk: a `Drop` impl in tvix aborting on unwind" → §12 Phase 2's kill criterion supplies a degraded mode.
- *placement*: strictly better than score_4 on the same axis → **5**

**A1 — score 5**
- *Closer to* **score_4** — same anchor, same three landmines, same in-tree countermeasures, verified citations: `graph-combinators.nix:12` and `:26` (verified exact), `watcher.rs:491-500` "reload failed; keeping last snapshot" (verified exact), `vault-data/src/graph.rs:70-94` no non-empty rule (verified), `state.rs:140-144` (verified).
- *Further from* **score_4** — the anchor is confidently correct; A1 is correct *after four self-corrections*. The item's own rule is "admitted uncertainty scores above a document that asserts something wrong", and A1 is the only document that both makes a real error and fixes it in the open: "the first draft claimed `validate_local_id` rejects `:`; it does not. **Corrected**" (I verified the doc comment — A1's correction is right and its original was wrong); "the first draft's '2 keyframes' at 1M nodes ≈ 2 × ~500 MB — **not acceptable**"; "One gap found: a Filesystem change arriving during a manual run was `Skipped` in the first draft and therefore lost. **Fixed**".
- *lean*: strongly toward score_4, and past it on the axis the item names.
- *placement*: strictly better than score_4 on the same axis → **5**

**C1 — score 4 (advisory)**
- *Closer to* **score_4** — every landmine the spec names is paired with an in-tree countermeasure, and every claim I sampled was true (verified list in the CK-034 row).
- *Further from* **score_4** — the anchor is *risks paired with countermeasures*; C1 §2 "Grounded constraints" is a list of **asserted constraints** ("the evaluator is pure, uses an embedded VFS, forces results with `deepSeq`"), and the risk/countermeasure pairing happens later, dispersed across §6, §8 and §14's kill criteria rather than in a risk section. C1's citations are broad ranges where B1/A1 cite the line of the construct, so each claim asks for more trust. C1's one self-found limitation ("the scrubber previews a run transition, not an invented full historical graph") is an honest scope correction, not a factual error like A1's.
- *lean*: toward score_4.
- *placement*: matches score_4 → **4**

### D10 — Phase slicing and kill criteria (w 0.05)

**B1 — score 5**
- *Closer to* **score_4** — anchor: five phases each with a parenthesised kill criterion, "P5 charts/jump-cannon/knowledge/ update in the same change as P2 and P4". B1 §12 has six phases, each with **Ships / Acceptance / Kill**, and §11 puts the whole map in the same change as the code. Acceptance criteria are numeric and falsifiable ("exceeds 1.3× on the 1M demo"; "costs > 2 s or > 2× snapshot memory per run"; "positions_cpu diff < 1 wu"; "the touched index exceeds 10 % of snapshot memory").
- *Further from* **score_4** — the anchor's phase list encodes dependency by numbering; B1 encodes it by order plus §1.3's ordered gap list, with no per-phase "depends on" line.
- *lean*: strongly toward score_4, and past it on the same axis. B1's kill criteria name the **degraded mode to ship**, not just the stop condition: "fall back to fuel-only with `wall_ms` enforced by `tokio::time::timeout` plus a hard cap of one orphaned evaluation per source (transform `Suspended` until it returns) — bounded, visible, documented"; "keep the strip and transition, disable scrubbing to non-live runs, and ship 'compare with #N' as a highlight set instead".
- *placement*: strictly better than score_4 on the same axis → **5**

**A1 — score 5**
- *Closer to* **score_4** — all clauses. §5's five phases each name a **Seam** as a field ("`/node/*id` and the Nodes panel"; "`rebuild_snapshot`, the revision probe, `/progress`"; "the importer chain; the Runs panel from Phase 1"; "`render::mount_canvas` + the Runs panel"), which is the anchor's "each land on an existing seam" made explicit. A1 is the only document with a real **dependency** statement: §3's S1–S11 table with a "Depends on" column and a mermaid DAG, referenced from each phase header ("(S1, S2 without runs)").
- *Further from* **score_4** — the anchor gives P5 a kill criterion; A1's Phase 4 reads "**Kill: none** (documentation and read-only surfaces)", and Phase 4 is the one that carries *all* of §2.4's knowledge-map edits. The map therefore lands in the least de-risking phase.
- *lean*: strongly toward score_4, and past it. The S-level DAG is more than the anchor's implicit numbering, and Phase 1's acceptance is a bound only the storage design can satisfy: "a 1 000-run soak in a `runs.rs` unit test (a synthetic 10 k-node graph mutated between `complete` calls) holds `records ≤ 256` and `delta bytes ≤ 64 MiB`".
- *placement*: strictly better than score_4 on the same axis → **5**

**C1 — score 4 (advisory)**
- *Closer to* **score_4** — four phases, each opening with "**User-visible value:**" (the clearest statement of the anchor's first clause of any of the three), each with Acceptance and a **prohibitive** kill criterion: "do not enable transformation bindings if the timeout path can accumulate blocking workers, any terminal failure changes the revision, ledger growth is unbounded, or a source-specific branch is required."
- *Further from* **score_4** — granularity. C1's Phase 2 is the anchor's score_2 shape in miniature: "add `TvixInput`, pure graph value injection, `graph-iter.nix`, `TransformBudget`, `GraphTransform`, `TransformedImporter`, publication metadata, `RunLedger`, `GET /runs`, and `POST /transforms/:id/run`" — the entire Rust core in one merge. C1's dependencies are prose (§3's ordered list), not per-phase, and its phases carry no S-numbers.
- *lean*: toward score_4. Phase 2's kill is the best safety gate of the three; the bundling is the coarsest.
- *placement*: matches score_4 → **4**

---

## Stage 6 — Score calculation

| Dim | w | B1 | w·s | A1 | w·s | C1 | w·s |
|---|---|---|---|---|---|---|---|
| D1 core abstraction | .14 | 4 | .56 | 4 | .56 | 3 | .42 |
| D2 run state / shape / bound | .14 | **5** | .70 | 4 | .56 | 4 | .56 |
| D3 package + tvix feasibility | .09 | **5** | .45 | 4 | .36 | 4 | .36 |
| D4 run lifecycle + terminal | .14 | **5** | .70 | 4 | .56 | **5** | .70 |
| D5 scheduling + upcoming | .09 | 4 | .36 | 3 | .27 | **5** | .45 |
| D6 serving + animation | .08 | 4 | .32 | 4 | .32 | 4 | .32 |
| D7 node content | .14 | 3 (cap) | .42 | **4** | .56 | 3 (cap) | .42 |
| D8 impeccable discipline | .08 | 4 | .32 | 4 | .32 | 4 | .32 |
| D9 landmines + groundedness | .05 | **5** | .25 | **5** | .25 | 4 | .20 |
| D10 phases + kill criteria | .05 | **5** | .25 | **5** | .25 | 4 | .20 |
| **Total** | 1.00 | | **4.33** | | **4.01** | | *3.95* |

- Raw weighted sum B1 = 4.33 · A1 = 4.01 · C1 = 3.95 *(advisory only)*
- Checklist penalties: **none applied.** The specification supplies its own gate mechanism (`essential_fail_handling` / `fatal_handling` / `pitfall_handling`) and explicitly forbids a global cap; the only consequence applied was dimension-local (CK-030 → B1's D7 capped at 3) plus the fatal disqualification of C1.
- **Gate source: specification `fatal_handling` / `essential_fail_handling`.** No judge built-in cap was invoked.
- **Sensitivity:** if CK-030 were judged YES for B1, B1's D7 becomes 4 and B1 totals **4.47** — still first. The ranking does not depend on my most debatable call.

## Per-solution report (spec format)

**B1** — total 4.33. Carried by **D2** (multi-bound delta+keyframe ledger with a restart scrubbability rule) and **D4** (11 terminal paths, in-VM hang conversion, per-path schedule effect). Weakest: **D7**, held at 3 by CK-030; the single change that would raise it is one line naming `just app-proto` and `app/ui/src/proto/`. Run-state home: *a bounded `RunLedger` in `runs::RunHost` beside the `ArcSwap<GraphSnapshot>` in graph-api's `AppState`, persisted under `JUMP_CANNON_IMPORTER_PACKAGES_DIR`.* Non-omp example: `transform-relabel-hubs.toml`, retyping high-degree nodes, exercised against the Obsidian source and `generate-clusters.toml`.

**A1** — total 4.01. Carried by **D7** (Phase 0 node content, `facts` as a pure function of the shared schema, `history_bounded` honesty, and the only document that names the regen) and **D10** (the only explicit S-level dependency DAG). Weakest: **D5** at 3 — the due instant is an in-memory `AtomicU64` with no persisted anchor, and a transform has no cadence of its own. Single change to raise it: persist the schedule anchor in the journal's boot-seeded records and let `upcoming` read it. Run-state home: *a bounded in-memory `RunJournal` inside each source's `AppState`, plus a records-only `runs.jsonl`; dropped with the source's `AppState` when an idle alternate is evicted.* Non-omp example: `collapse-by-facet.toml`, "meaningful for Kubernetes namespaces, Obsidian folders, generated cluster types, or any pest `kind` alike."

**C1 — DISQUALIFIED (CK-003).** Reported separately, per `fatal_handling`. **Deciding quote (the absence):** `TvixValueSource { Binding, RunSequence, ScheduledAtMs, InputRevision }` (§4.1) plus "There is no implicit `now`, environment lookup, file lookup, or network lookup" (§4.1) — and, across the whole 885-line document, **no** statement of what a transform does when it needs a value that changes between runs. Repair: one paragraph, in §4.1 or §12.11, saying that a per-run external value is delivered either as a re-bound catalog variable (the existing `PUT /importers/:source_id/transforms/:transform_id/variables` overlay path, whose edit already drops and rebuilds the source and therefore re-runs the chain) or as an upstream source refresh, and that a static `Binding` cannot be refreshed per run. That paragraph satisfies the gate without changing C1's design.

## Comparative analysis

**Where all three agree** — and this is the real finding about the field: the hard part of this task is not the run timeline, it is the five things the pruning rounds kept missing, and all three of these documents have them. Every one states a bound in the sentence that introduces its store; every one names miss/overlap/coalesce against the verified in-tree `MissedTickBehavior::Skip`; every one fixes `convert_generated_graph` as Phase 0; every one distinguishes the 768-line position-keyframe ring from run history in a dedicated section, and **CK-032 fires for none of them**; every one reuses the existing driver task and **CK-033 fires for none of them**. Round-1's universal caps (D5≤3 for all 18, D7≤3 for all 18) are gone — A1 and C1 both clear D5, and A1 clears D7.

**Where they diverge — three real forks, not cosmetic ones.**

1. **What a run *is*.** B1: a transform application; the input is "always the **live published snapshot** at the moment it starts", so `every` transforms compose as `g_{t+1} = T(g_t)` and an import resets the base (§6.1). A1: a source rebuild — "**Runs = transform applications** | Every rebuild is a run (`transform_ids` may be empty)" — where the input is "the **fresh source import**… never a fold over its own previous output — so runs are idempotent and drift-free" (§4.1). C1: a re-derivation from a decorator-owned `SourceBaseline`, "exactly one latest successful upstream graph, not one graph per run" (§5.1), with chain rollback if any link fails. This is the deepest disagreement in the field and it is invisible in the totals: B1's choice accumulates state across runs and must therefore be told the non-idempotence rule in the docs; A1's is idempotent by construction; C1's sits between, getting determinism without losing the upstream baseline.
2. **What the timeline's per-step cost is.** A1's scrub is a `mount_canvas` of a union topology per step, O(graph), and is disabled entirely above 256 MiB. B1's and C1's deltas are O(changes) with stable ids, so scrubbing is delta-sized — but B1's hold-scene remount means "the hold scene remount is judged too disruptive by users (sim restart on scrub)" is a live risk B1 names and answers with a kill criterion, while C1 simply refuses the claim: "it does not claim to materialize every historical full graph."
3. **Whether "not retained" is a state or a mode.** A1 has a mode (`scrub: "deltas_only"`, controls disabled with an explanation). B1 has a per-run boolean (`delta_retained = false`, "counts only", and the record survives as history). C1 has a typed per-run state (`DeltaAvailability::Omitted { SizeLimit }`, HTTP 410, "the panel keeps exact counts and explains why animation is unavailable"). C1's is the right one and is the most transferable thing in any of the three.

**Mistakes found.** B1: one citation offset (`inspector.rs:37` is the `use` line; the usage is `:651`) — same file, right construct, no contradiction, not CK-034. A1: none surviving; its draft errors were corrected in the open. C1: none factual; its omission (CK-003) is a silence, not an error.

**Length bias check.** The three are 68 895 / 70 862 / 52 971 bytes. C1 is the shortest and was disqualified on an *absence* — the shortest document is not the one that skipped the most required content, since C1 also carries the field's best D4, best D5 and best node-content reuse. B1 and A1 are within 3 % of each other in length and 0.32 apart in score, so length explains nothing here.

---

## Diversity audit

**The three differ in run-state home. The audit passes; the ranking is clean, not advisory.**

| | Home | Mechanism | Durability | Who emits the run event |
|---|---|---|---|---|
| **B1** | **beside the snapshot** | `AppStateInner.runs: Arc<runs::RunHost>`, one process-wide ledger, one `next_run_id`, one `seq` | `runs/` on disk with deltas + keyframes, compaction at 1 024 lines | graph-api (`runs::executor::run_one`), *above* the importer |
| **A1** | **with the snapshot, per source** | `AppStateInner.runs: Arc<RunJournal>`, one per source; "Alternates evicted on idle TTL drop their journal with their `AppState`" | **records only** — "deltas and keyframes are never written" | graph-api (`run_rebuild`), *above* the importer |
| **C1** | **owned by the source** | source-owned `RunLedger { source_key, retention, state, store }`; decorator `TransformedImporter { recorder: Arc<dyn TransformRunRecorder>, baseline }` | **deltas** `cbor.zst` + `checkpoint.json` + checksummed JSONL with atomic compaction | the **importer decorator**, via `PublicationMetadata { transform_run_ids }` on `LoadResult` |

The distinctions are structural, not cosmetic: A1's journal *dies with its source*, B1's does not; A1 persists no graph data at all while C1 is the only one that persists deltas; and C1 is the only one where the run event originates in the importer rather than above it, which is what gives it "the same finalization path" for startup, default reload, alternate reload and nested hosts "without downcasting trait objects" (§5.2).

Top two (B1 beside / A1 with) are **distinct** homes, so no collision is reported. The spec's tie-break chain (D2 → D4 → D7 → D1) is **not reached**: 4.33 vs 4.01 is not a tie.

---

## Synthesis seams

**B1 — the in-VM fuel budget.** Neither A1 nor C1 converts a hang into a typed terminal state: A1 concedes "**Timeout is not cancellation**… the blocking thread keeps running", and C1 keeps a watchdog plus a quarantined slot for the same reason. B1 implements tvix's `RuntimeObserver::observe_execute_op` as a `DeadlineObserver`, counts ops, panics with a typed payload, and `catch_unwind` in the blocking closure returns the thread to the pool — "**No orphaned thread, no hung driver task, no partial graph.**"
(a) run state: beside the ArcSwap in `AppState`; (b) `crates/tvix-wasm/src/budget.rs` (new, implementing an existing trait hook) + the `catch_unwind` at `crates/graph-api/src/runs/executor.rs`; (c) **cheapest of the three** — one new tvix-wasm file, one existing seam, and it hardens `POST /generate` for free. B1 names the case it does not cover (a `Drop` impl aborting on unwind) and supplies the degraded mode, so the seam is liftable without adopting B1's unwind strategy wholesale.

**A1 — the importer decorator.** B1 puts the transform executor in graph-api's run module, above the importer. C1 also uses a decorator, but with a concrete `GraphTransform` struct and its own `SourceBaseline` semantics. A1 alone makes the transform an `Importer` in `crates/data-loader/src/lib.rs` — `GraphTransform` + `TransformedImporter` — so "`HostedImporter` wraps the `TransformedImporter` exactly as it wraps any importer" and capability preflight, `watch_is_authorized` and `ImporterDescriptor::validate` are untouched, which is what makes the catalog binding uniform across `build_alternate`'s three arms via one `wrap_transforms` helper. That is also the structural reason A1 gets runs for sources with zero transforms, and the reason its per-source journal is the right home for a per-source ledger.
(a) run state: in-memory per-source journal with the snapshot; (b) `crates/data-loader/src/lib.rs` (`GraphTransform`, `TransformedImporter`) + `crates/importer/src/tvix.rs` (`TvixTransform`); (c) **medium** — it edits the shared contract crate, and `apply(LoadResult) -> ImportFuture` is the signature that must be re-shaped if the executor stays in graph-api as in B1. Lift the decorator wrapper and the run slot without the trait, and it costs one type plus one call site.

**C1 — typed omission states and a persisted schedule anchor.** B1 persists to `runs/` and derives its schedule from the ledger's `last_finished_at`; A1 persists records only and recomputes `next_due_ms` in memory. C1 alone writes `deltas/<run-id>.cbor.zst` + `checkpoint.json` "preserves the next sequence and **schedule anchors**", recovers a truncated JSONL tail, converts a recovered nonterminal run into `Failed { kind: ProcessInterrupted }`, and models a delta it *cannot* keep as a first-class per-run state (`DeltaAvailability::Omitted { SizeLimit }`, HTTP 410 with a typed body) so the UI "states that the transition detail was not retained instead of pretending to animate it". It is also the only document whose upcoming row is truthful across a restart.
(a) run state: ledger owned by the source, durable under `run-ledger/v1/<source-key>/`; (b) `crates/graph-api/src/runs/ledger.rs` + the `checkpoint.json` / `deltas/` layout, and `crates/importer/src/transform.rs` for the `TransformRunRecorder` hook; (c) **medium and the most orthogonal of the three**, because both halves are independent of where the run executes. **C1 is disqualified, so this must be lifted as a pattern, not merged as a document** — and the synthesizer should note that `Omitted { SizeLimit }` strictly improves on B1's `delta_retained = false` (same idea, less typed) and on A1's `scrub: "deltas_only"` (a *mode* rather than a per-run state).

Two secondary lifts worth naming because they are cheap and the winner lacks them: **A1's 1000-run soak acceptance** ("a synthetic 10 k-node graph mutated between `complete` calls holds `records ≤ 256` and `delta bytes ≤ 64 MiB`") is a test only the storage design can pass, and **B1's id-keyed position carry** replaces an index-based carry that "only fires on equal counts and would scramble a reorder" — B1 makes the success criterion testable with "positions_cpu diff < 1 wu" and A1 has no equivalent.

---

## Stage 7 — Rule candidacy (Five Whys, all issues)

**Issue 1 — Adding a `proto::NodeMeta` field without the client regen step (B1, C1; A1 correct).**
Why 1: the design stops at `crates/graph-api/proto/graph.proto`; Why 2: the author treats `.proto` → generated server type as the whole path; Why 3: nothing in the design flow forces thinking about the client's *copy* of the type; Why 4: the regen recipe lives in a crate-map table row (`AGENTS.md:87`) and a justfile target (`justfile:60`), not anywhere a "add a field" task looks; Why 5: no behavioural constraint states that changing the wire means regenerating *both* copies.
*Classification*: missing convention, repo-wide. **Passes all five recurrence tests** — any task adding a protobuf field (most backend work) can hit it; it is not project-specific; 2 of 3 documents hit it in this very round; it is trivially actionable; the damage is "the field is invisible in the UI", which is the exact bug class the user reported, reintroduced by the fix.
*Overlap check*: neither `.claude/rules/per-event-store-retention-bound.md` nor `.claude/rules/transform-terminal-failure-path.md` covers this. `AGENTS.md:87` states the law, so this is a **routing** rule — pointing at the law from the situation where it gets missed — not a new law.
**Action: one rule created**, `.claude/rules/proto-field-client-regen.md`, path-scoped to the two proto paths.

**Issue 2 — Per-run stores without a bound (none; all three pass).** Already covered by `per-event-store-retention-bound.md`, which cites this exact round ("11 of 18 timeline proposals proposed one"). **No rule — already covered.**

**Issue 3 — Unspecified terminal failure path (none; all three pass).** Already covered by `transform-terminal-failure-path.md`, which cites this round ("15 of 18 timeline proposals did"). **No rule — already covered.**

**Issue 4 — Citing an import line instead of a use site (B1's `inspector.rs:37`).** Why 1: the line number came from a grep of the symbol; Why 2: the import is the first hit; Why 3: no practice of citing the use site. Cross-task: yes. Actionable: yes. **Token justification: NO** — a one-line offset in a correct citation does not justify a rule in every session. **No rule.**

**Issue 5 — C1's silence on per-run external input.** Root cause is a *task-specific* gap against a compound gate in this specification, not a general anti-pattern. **No rule.**

---

## Stage 8 — Self-verification

| # | Question | Answer | Adjustment |
|---|---|---|---|
| 1 | **Evidence completeness** — did I examine all relevant files, or miss something? | I read all three documents in full, the full 674-line spec, and verified ~45 codebase facts across `tvix-wasm/src/lib.rs`, `builtins/impure.rs`, `vm/mod.rs`, `graph.nix`, `graph-combinators.nix`, `importer/src/tvix.rs`, `importer/src/pest.rs`, `data-loader/src/{lib,identity}.rs`, `vault-data/src/{node,graph}.rs`, `graph-vcs/src/{merge,model}.rs`, `graph-api/src/{server,watcher,state,importer_catalog}.rs`, `graph-api/proto/graph.proto`, `app/ui/src/{main,graph_canvas}.rs`, `app/ui/src/render/pipelines.rs`, `app/ui/src/panels/{timeline,inspector,nodes}.rs`, `app/ui/assets/app.css`, and `AGENTS.md`/`justfile`. I did **not** verify every A1 and C1 citation (I sampled ~12 and ~12 respectively); the unverified ones are named as unverified and none was found to be contradicted. | No adjustment. C1's `graph-iter.nix` Nix bodies remain elided in the document itself — a gap in the artifact, recorded on D3, not in my sampling. |
| 2 | **Bias check** — am I influenced by length, tone, or formatting? | Checked explicitly. C1 is the shortest document (52 971 B vs 68 895 / 70 862) and I gave it the field's best D4 (5), best D5 (5) and a unique node-content reuse pattern. B1 is longer than C1 by 16 KB and I scored it 0.38 higher — I checked the drivers: D2 (four bounds vs none, plus a restart rule), D3 (in-VM budget), D4 (11 paths vs 9, plus a genuinely different hang conversion), D9 (verifiable citations). None is volume. B1's dense §-numbering and confident register did not move a score; A1's self-corrections earned D9=5 on the spec's own explicit rule ("admitted uncertainty scores above a document that asserts something wrong"), not on tone. | No adjustment. |
| 3 | **Anchor fidelity** — was every `anchor_comparison` written on both sides, before any number, and on the contrast axis? | Yes for all 30 placements. I re-verified during writing that each `closer_to` and `further_from` carries its own quoted anchor text *and* its own quoted artifact evidence, and that no number appears before the comparison in any dimension. I stayed on the axis: D2 contrasts only shape-and-bound, so the "lives beside the ArcSwap vs. inside it" difference is reported in the diversity audit rather than smuggled into D2; D7's missing ARIA/palette comments sit on D8; D9's "volume vs truthfulness" is explicitly named as *not* the axis and is not used. | No adjustment. Two placements I initially drafted as 3s (C1/D8, C1/D2) were revised to 4 after checking that the missing sub-clauses (ARIA roles; keyframes) are in the dimension *description* rather than in the *contrast line* — the contrast line's own two clauses are met. Recorded here because it is a placement change, not a score adjustment. |
| 4 | **Comparison integrity** — is my reference result correct, and did I introduce errors of my own? | My Stage 2 reference is derived from the spec's own gates, and I re-checked my three *codebase* facts that carry the most weight against the tree rather than trusting the spec: `NodeMeta` has 11 fields ✓, caps are 1M/4M ✓, `validate_package_schema` rejects `[[schema.fields]]` ✓, `timeline.rs` is 768 lines ✓, `set_positions`/`update_colors`/`update_sizes`/`set_selected` live in `render/pipelines.rs` not `graph_canvas.rs` (the spec's CK-017 names them as `graph_canvas::*` — the spec is loose; I graded against the substance, "patch what changed into the existing buffers", and said so). One error of mine to declare: I initially attributed the node-detail `Spinner` to `inspector.rs` because B1 did; the tree has it in **both** `nodes.rs:754` and `inspector.rs:651`. That correction is why B1's slip is one import-line offset rather than a wrong-file error, and it is why I noted A1's non-conversion of `nodes.rs:754` as a completeness gap. | No further adjustment; the `graph_canvas` module attribution is noted in the report so the synthesizer is not misled by the spec's shorthand. |
| 5 | **Proportionality** — are the scores proportional, or am I uniformly harsh/lenient? | Spread check: B1 {5,5,5,4,4,4,3,4,5,5}, A1 {4,4,4,4,3,4,4,4,5,5}, C1 {3,4,4,5,5,4,3,4,4,4}. I used all five scale points; the only 1s are absent because no document is worse than the failing anchor on any axis, and the two 5s in D6/D8 sit at score_4 because the anchors are already full passes there and "better" must be on the same axis (C1 naming `set_positions` is a D6 detail, not a same-axis advance; a document naming extra buffer functions is not "more frame-cost-proportional"). Proportionality test: the single largest score gap is B1 vs A1 on D2/D3/D4 (+1 each = 0.32 total), each traceable to a specific named mechanism, not to a gestalt. | No adjustment. One honest downgrade after self-check: B1's D7 would be 4 on merit (four new fields reaching the generated path beats the anchor's two), and the 3 is the CK-030 cap. I have flagged that in the sensitivity line so the reader can see the cap is the spec's, not my judgement of the work. |

---

## Strengths

1. **The field learned.** Across three rounds, none of these documents commits the failure the pruning rounds found universal: 0/3 miss retention bounds, 0/3 miss terminal failure paths, 0/3 conflate the position-keyframe ring with run history (CK-032), 0/3 introduce a second scheduler (CK-033), 0/3 assert a false fact about the tree (CK-034). Every one of the 34 items' hard targets is met by at least two of the three.
2. **B1's in-VM budget is the field's single best technical idea.** Implementing `RuntimeObserver::observe_execute_op` (verified present at `tvix/eval/src/vm/mod.rs:461`) and unwinding out of the VM is the only proposal in the field that actually answers the landmine the spec names first — and B1 cites the observer hook, the `env` builder, `groupBy`, and `readFile`'s impurity at four lines I could independently confirm to the exact line.
3. **A1's honesty is load-bearing, not decorative.** Four draft errors are corrected in the open, including one that matters for the design (`validate_local_id` and `:` — A1's original claim was wrong and its correction is right, which I verified) and one that matters for the budget (2 keyframes at 1M nodes ≈ 1 GB "not acceptable"). A document that retracts is worth more than one that never had to.
4. **C1's atomicity model is the most complete in the field** and is orthogonal to the executor's location: chain-wide rollback (`RolledBack { cause_run_id }`, "no partial graph is published") and cross-restart recovery (`Failed { kind: ProcessInterrupted }`) are terminal states neither other document has, and `PublicationMetadata { transform_run_ids }` on `LoadResult` gives every host the same finalization path "without downcasting trait objects".
5. **The verdict is robust.** B1 leads under both readings of its most debatable checklist call (4.33 with the CK-030 cap, 4.47 without).

## Issues

| # | Pri | Description | Evidence | Impact | Suggestion |
|---|---|---|---|---|---|
| 1 | **High** | C1 is eliminated by silence on per-run external input | CK-003; `TvixValueSource::{Binding, RunSequence, ScheduledAtMs, InputRevision}` (C1 §4.1); zero hits for re-bind/re-import/external-input language across 885 lines | Removes the field's best D4, best D5 and unique node-content reuse from the ranked set, on a gap that is about silence rather than design | Synthesizer: lift C1's `DeltaAvailability::Omitted { SizeLimit }` and the persisted `checkpoint.json` schedule anchor as patterns, independent of C1's document. If the orchestrator wants C1 ranked, the repair is one paragraph in §4.1 or §12.11. |
| 2 | **High** | B1 adds four `proto::NodeMeta` fields and never names the client regen | B1 §8/§12 Phase 4 vs. zero hits for `app-proto` or `app/ui/src/proto` (rule created) | The exact bug the user reported — "click a node, not much info" — reintroduced by the fix, because the fields never reach the checked-in client types | Add one line to B1 §12 Phase 4: "regenerate `app/ui/src/proto/` via `just app-proto`". A1 §4.4.6 already has it; lift it verbatim. |
| 3 | **Medium** | A1's transforms have no cadence of their own | A1 §2.6: the plan is the source's `WatchPlan`; `ImporterTransformBinding` (§4.4.5) has no schedule field | The user's "periodic 'dream' consolidation" cannot run on a tick different from the source's, and A1's `upcoming` is the source's next tick | Add a per-transform `watch` to the binding, as B1's `Trigger` and C1's `watch: WatchPlan` both do, and persist the schedule anchor so the upcoming row survives a restart. |
| 4 | **Medium** | A1's per-scrub-step `mount_canvas` makes the animated timeline unavailable on large graphs | A1 §4.9.4 + §2.3 "scrub controls disabled" above `JC_RUNS_KEYFRAME_BYTES` (256 MiB) | The centrepiece feature silently disappears exactly where a 1M-node consolidation run lives | Adopt B1's or C1's O(changes) stable-id delta so the scrub step is delta-sized; the union mount is then only for the keyframe-mount case. |
| 5 | **Medium** | B1 re-arms schedules from finish time, so cadence drifts under a slow run | B1 §6.1 "`next_due_at = last_finished_at + intervalMs`" | A run that overruns pushes every subsequent run out, compounding on a large graph | Adopt C1's anchored re-arm (§8.2: "next_run_at advances to the first future point on the original schedule anchor, so cadence does not drift"). |
| 6 | **Medium** | All three add a second seq cursor instead of riding `/progress?since=<seq>` | B1 §8 `GET /runs?since=<seq>`; A1 §4.8; C1 §10.2 carries `progress: Option<f32>` inside `/runs` | Two event streams over the same server, which is the "two clocks over the same graph" scope bomb in a different costume | Either reuse `/progress` for run progress, or state in the risk section why a run-record seq is a different cursor — B1's justification is close; it should also say why the existing seq cannot carry it. |
| 7 | **Low** | C1 states no ARIA roles or labels anywhere | C1 §10.1–10.4; contrast with A1's `role="listbox"` + `aria-activedescendant` + `aria-live` | AGENTS.md:199 requires "correct ARIA roles"; the strip is a keyboard-navigable control surface | Copy A1's §4.9.2 role/label sentence. |
| 8 | **Low** | B1's one citation offset | B1 §9.6 `inspector.rs:37` (the `use` line) vs. the usage at `inspector.rs:651` | Cosmetic; the claim is true and the file is right | Cite the use site. |
| 9 | **Low** | B1 and C1 never state the palette/`panel_kit::CSS` constraint | B1 §9.5, C1 §10.4 place CSS in `app.css` but neither names `panel_kit::CSS` or the palette | AGENTS.md:197-198; the runs panel is the most likely place for the "AI-slop chrome" the rule names | One line: "styling stays in `app.css` + `panel_kit::CSS` on the existing panel-kit theme." |

## Rules generated

- `.claude/rules/proto-field-client-regen.md` — path-scoped to `**/proto/*.proto` and `app/ui/src/proto/**`; HIGH. For issue 2. Covers the one systemic, cross-task, high-frequency, actionable gap found in this round that the two existing rules do not cover, and routes to the law at `AGENTS.md:87` / `justfile:60` from the situation where it is missed.

## Confidence

**Medium-High.**
- *Evidence strength*: **Strong** for B1 and A1 — ~45 codebase facts independently verified, including four citations in B1 that could not have been guessed and all landed exactly. **Moderate** for C1 — every claim sampled was true, but C1's broader citation ranges (`lib.rs:25-44,85-120,263-310`, `watcher.rs:148-285,313-535`, `state.rs:49-130,209-241`) were verified for the *facts* they support, not line-by-line, so a line-offset exists somewhere in C1 that I did not find.
- *Criterion clarity*: **Clear** — all ten dimensions carry a single-axis contrast line, and in every case the axis is distinguishable from the dimension's other sub-questions.
- *Specification quality*: **Complete** — 34 items with consequences, 10 anchors, a repaired aggregation, an explicit diversity audit and a synthesis-seam instruction. Two imprecisions found: CK-017 names `graph_canvas::set_positions`/`update_colors`/`update_sizes`/`set_selected`, but those live on `GraphPipelines` in `app/ui/src/render/pipelines.rs:1231/1270/1429/1743` (verified) — the substance is unambiguous, so I graded the substance. And CK-030 cites AGENTS.md:161 for the regen recipe, which is actually at `AGENTS.md:87` plus `justfile:60`. Neither changes any outcome.
- *Known soft spots in my own verdict*: (a) the CK-003 conjunct is a judgement call, and it is the call that eliminates C1 — C1 satisfies the first clause emphatically and its §5.2 baseline re-derivation is arguably the mechanism the second clause is reaching for, but it never frames it that way and provides no way to refresh a *non-graph* value; (b) CK-030's verdict on B1 costs B1 0.14 and would be the second-place call if read the other way — though not the *first*-place call, which is why I ran the sensitivity; (c) C1/D2 and C1/D8 were each revised upward from 3 to 4 during self-verification when I confirmed the missed sub-clauses sit in the dimension description rather than in the contrast line.
