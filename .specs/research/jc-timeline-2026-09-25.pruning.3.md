# Judge 3 — Pruning Report: jump-cannon transformation runs / live timeline

- **Judge**: `TimelinePruneJudge3`
- **Date**: 2026-09-25
- **Specification** (read verbatim, unmodified): `.specs/research/jc-timeline-pruning-spec.yaml` (root `/Users/casazza/Repositories/ocasazza/jump-cannon`)
- **Artifacts judged**:
  - `jc-timeline-2026-09-25.proposals.a.md` → **a.md** (A1–A6)
  - `jc-timeline-2026-09-25.proposals.b.md` → **b.md** (B1–B6)
  - `jc-timeline-2026-09-25.proposals.c.md` → **c.md** (C1–C6)
- **18 approaches**: A1–A6, B1–B6, C1–C6

> **PROCESS FINDING (file-location defect in the upstream inputs).**
> The three proposal files were **not** written to the jump-cannon repo. The explorers ran
> with `~/.config/nixos-config` as their working directory, so the files are at
> `/Users/casazza/.config/nixos-config/.specs/research/jc-timeline-2026-09-25.proposals.{a,b,c}.md`
> while the spec was written to `/Users/casazza/Repositories/ocasazza/jump-cannon/.specs/research/`.
> Their own completion payloads claim the jump-cannon-relative path (e.g. `".specs/research/jc-timeline-2026-09-25.proposals.a.md"`),
> which is why the files are missing from the expected location. **Every explorer report
> overwrote nothing and lost nothing, but all three wrote to the wrong tree.** This is an
> upstream finding, not a defect of the proposal content; it is scored against content only.
> Recommendation to the orchestrator: relocate the three files into the jump-cannon
> `.specs/research/` directory before Phase 2 reads them, and pin explorer `cwd` explicitly.

---

## 1. Specification interpretations (decided before scoring, recorded for audit)

Three points in the specification admit more than one reading. Each was resolved **against the
specification's own text**, and the resolution is disclosed here because two of them change
rankings materially.

### 1.1 `important`-level checklist NOs do **not** cap the total

The specification's `scoring` block defines exactly two consequences: `essential_fail_cap`
("Any `essential` checklist item answered NO caps the total at 2.0") and `pitfall_handling`
(a YES caps *the affected dimension's score* at 2). It assigns `importance: important` to ten
items and attaches **no** consequence to an `important` NO.

My judge defaults would cap an `important` NO at 1.0. **The specification governs where it
speaks, so I applied the specification: no cap for `important`.** Two reasons this is the
correct reading rather than a convenience:

1. **A built-in 1.0 cap would make the exercise vacuous.** CK-013 (name the
   `charts/jump-cannon/knowledge/` update) fails for **all 18** proposals — verified by grep
   over all three files; the only hit is `b.md:158` citing the note as *grounding*, not as a
   change. A 1.0 cap there would tie all 18 at 1.0 and destroy the ranking the specification
   exists to produce.
2. **The specification names its cap threshold explicitly.** It chose 2.0 and wrote it down.
   An unspecified third cap would be stricter than the specification and is barred.

Every `important` NO is nevertheless recorded per-proposal below, and CK-013 in particular is
promoted to a High-priority issue (§9).

### 1.2 The aggregation formula is `Σ(weight × score)`, not `Σ(weight × score)/5`

The specification writes `total = sum(weight_d * score_d) / 5, giving 1.0-5.0`. These two
clauses contradict each other: with `Σweight = 1.000` and scores in 1–5, `Σ(w·s)` is already
in [1, 5], and dividing by 5 yields [0.2, 1.0]. The stated output range **and** the
`essential_fail_cap: 2.0` (which is meaningless on a 0.2–1.0 scale) both confirm the intended
reading. I used `total = Σ(w_d × s_d)`. Flagged as a specification defect; the typo is in the
specification, not in the proposals, and I did not edit the spec.

### 1.3 The essential cap collapses 16 of 18 proposals into an exact tie at 2.0

`essential_fail_cap` is applied *before* ranking, per the specification's own procedure
("Any essential NO caps the total at 2.0" → "Rank survivors by total; break ties on D1, then
D3"). Only **B1 and C1** pass every essential item, so 16 proposals land on exactly 2.000 and
the tie-break chain (D1, then D3, then D6) does the entire ordering work for slots 3+.

I applied the chain as written. **Where the chain is exhausted** (A1 vs A3 tie on D1=4, D3=3,
D6=4) I fell back to the pre-cap weighted total, and I say so explicitly at each such point.
I did **not** soften the cap to preserve a quality gradient — the cap is the specification's
central quality lever and 16 proposals tripping it is the single most important finding in
this report.

---

## 2. Stage 2 — Reference result (written before reading the artifacts' designs in detail)

What a correct, high-scoring proposal for this brief must contain, derived from the
specification's checklist and anchors alone:

**MUST**
- Transformation carried as a `format_version = 3` TOML package on the existing `engine = "tvix"`,
  one new `[parser]` field for the graph input, variables bound per application. No new
  `SourceKind`, no per-source `--flags`, no per-transformation compiled dispatch.
- Run record in graph terms: `{ id, transform_id, input_revision, output_revision, status,
  delta }` — never `session` / `agent` / `tool` / `goal`.
- A named, externally implementable `Transform` interface (`apply(g, bound) -> Graph`) so the
  requester's "topos" can implement and consume it from outside the repo.
- A **hard bound on per-run retention** stated in the same sentence that introduces the store.
- A **specified terminal failure path**: cap breach (`JC_MAX_NODES`/`JC_MAX_EDGES`) and runaway
  eval both resolve to a *recorded failed run with a visible reason* — never a crash, a hang,
  or a silently truncated graph. This is the gate the field most often misses.
- A declared, serialisable run plan evaluated by the **existing** `WatchPlan`/watcher tick, with
  stated miss/overlap semantics.
- A reduced-motion variant for the animation, and panel-kit loading stores for any new async panel.
- Per-frame patching of what changed into the existing GPU buffers, guarded by the existing
  revision check.
- A worked non-omp example, concrete file paths, a probability, and a risk section pairing
  this repo's documented landmines with countermeasures that already exist in the codebase.

**MUST NOT**
- A new crate/scheduler runtime; a per-transform interval task or job queue.
- tvix evaluation on the UI thread.
- Run history loaded into the single live `ArcSwap<GraphSnapshot>`.
- Transform output folded into the served graph with no addressable artifact.
- The `app/ui/src/panels/timeline.rs` position-frame ring conflated with run history.
- Code, patches, or claims that code is written.
- Missing `charts/jump-cannon/knowledge/` note updates.

The correct artifact is a **document**: approach, seams, phases, trade-offs, risks — with the
structure above filled in, not approximated.

---

## 3. Stage 3 — Comparative analysis (set level)

### 3.1 Matches (all three sets)

Every one of the 18 approaches independently discovered the same four load-bearing facts, and
all three grounded them with file:line citations that resolve to the constructs claimed:

| Fact | a.md | b.md | c.md |
|---|---|---|---|
| tvix projection drops node `metadata` | `a.md:25-26` ("frontmatter: HashMap::new(), mtime: 0, content_readable: false") | `b.md:13` ("`RawNode` ignores extra fields") | `c.md:10` ("`RawNode` ignores extra fields, so **Nix-side node metadata is dropped on the wire**") |
| `timeline.rs` is a layout scrubber, not a run timeline | `a.md:36-38` ("A **Timeline panel already exists** — it scrubs a `FrameRing` of *layout positions*, not graph states") | `b.md:24` (listed as a seam: "the Timeline panel's `FrameRing` scrub UI") | `c.md:15` ("it is a layout scrubber, not a graph-state timeline") |
| No scheduler exists; the tick is `WatchPlan` + the debounced watcher | `a.md:28` | `b.md:24` ("`WatchPlan::{Static,Filesystem,Poll,Push}` and `watcher.rs` drivers") | `c.md:13` ("`Poll { interval_ms }`, `Push`, `Static`") |
| `graph-combinators.nix` already holds `graph→graph` rewrite rules with no host | `a.md:18-22` | `b.md:24` | `c.md:9` ("it already has rewrite rules (`subdivideRule`, `hubRule`, `reverseEdgesRule`, `contractRule`)") |

This convergence is real evidence that the exploration was grounded rather than speculative,
and it is why **CK-018 is a NO (no conflation) for all 18**: the conflation pitfall is
universally avoided, and two sets say so in the artifact's own words.

### 3.2 Gaps (present in all three sets)

1. **CK-013 fails for all 18.** No proposal names a `charts/jump-cannon/knowledge/` map update.
   Verified by grep across all three files. This is a cheap repo convention (`AGENTS.md`) that
   an upstreamable change must satisfy in the same commit.
2. **CK-009 fails for 15 of 18.** Only B1, C1, C2 specify a terminal failure path. This is the
   highest-impact gap in the field.
3. **CK-008 fails for 11 of 18.** The VCS-commit designs (A2, B2, C2) and the in-graph designs
   (A4, B4, B5, C4) state no retention bound at all; C2 says retention is *needed* without
   specifying it.
4. **CK-005 is satisfied only by assertion.** No proposal specifies what the *in-flight* run
   shows. "honest countdown" (a.md:118), "honest `m:ss` until next" (b.md:71) and "the timer's
   next fire plus the cadence" (c.md:100) are all statements about time-until-next, not
   measured-versus-indeterminate progress. All three sets list "honest progress" as a binding
   constraint (`a.md:45`, `b.md:20`, `c.md:31`), so I scored YES on "never a fabricated number"
   — but the in-flight case is genuinely unaddressed across the field.
5. **No proposal phases its work.** All three sets' "approaches" are alternatives, not phases.
   D7 therefore tops out at 3 for all 18, and at 2–1 for the designs that additionally name
   their own blast radius (`b.md:111` "Blast radius across seven crates").

### 3.3 Deviations

- **b.md introduces a new crate** (`graph-transform`, `b.md:69`) where a.md (`a.md:103` "The
  Rust side is a `crates/importer` type, not a crate") and c.md (`c.md:72`
  `crates/importer/src/transform.rs`) use a type. This is the sharpest cross-set design
  disagreement, and it is *legitimate*: a.md:43 draws the line itself — "a new *mechanism* may
  be a crate, a new *source* may not" — which is the same boundary b.md is respecting. CK-001
  forbids the transformation becoming a crate, not the mechanism. Both pass.
- **c.md adds `schedule` cron parsing** (`c.md:79` "cron parsing scope creep — keep to
  `every <duration>` plus fixed clock times") where b.md uses a typed enum (`b.md:69`
  `{ every_ms | on_change | manual }`) and a.md uses the source's own `WatchPlan` untouched.
  b.md's enum is the closest match in the field to D5's `score_4` anchor.

### 3.4 Mistakes in the artifacts

- **a.md:110** `RunRecord` stores `added/removed/changed node & edge **counts**`, but
  `a.md:115` promises `/runs/:id/diff` returning "two LE `u32` index buffers (added, removed)".
  With only 8 retained snapshots and counts as the record, a diff older than the 8-snapshot
  window is not computable. The design is internally inconsistent about what the journal holds.
- **c.md:74** specifies `/runs` (JSON) while `c.md:64` acknowledges "protobuf `NodeMeta` is
  shared with session-manager and the browser" as a wire-growth axis, and never states why the
  run endpoint is exempt from the protobuf rule. Unexamined wire divergence.
- **b.md:95 vs b.md:98** are self-contradictory on execution site: "`TvixTransform::apply` runs
  identically in a worker" against "the browser thread limits graph size (the tvix worker with
  timeout is retired)". B3 therefore trips CK-020.
- **a.md:56-60** (success criteria) promises "scrub past runs and see what changed" for *any*
  source, but A3 (`a.md:218-220`) concedes "you cannot scrub *to* a past graph, only see what
  a run did; history is lost on reload". The set-level promise overstates A3.

### 3.5 Requester-specific vocabulary found (explicit disclosure)

**No core type, field, endpoint, or panel introduced by any of the 18 proposals carries
requester vocabulary** — CK-002 is YES for all 18. Every occurrence is in framing, grounding, or
risk commentary:

| Location | Quote | Assessment |
|---|---|---|
| `a.md:39-40` | "Requester's package: `omp-auto-loop.toml` (pest, N\|/E\| lines…)" | Grounding only, explicitly framed as "one instance of 'a process emits a graph over time'" |
| `a.md:159-160` | "which is exactly what the requester's **'dream consolidation'** wants: consolidation is a merge with recorded conflicts" | Requester metaphor inside a core design rationale; a field name would have been a CK-002 failure, a sentence of motivation is not |
| `a.md:297-298` | "the requester's 'periodic **dream consolidation**' wants one-click cadence from the UI" | Trade-off commentary |
| `b.md:44` | "Requester's loop (pest package + 'dream' consolidation transform) is one *configuration* of the feature, not a code path" | Success-criteria framing |
| `b.md:84` | "a '**dream**' transform can commit to `transform/dream`" | Requester metaphor used as a **branch name example** — the closest thing to vocabulary in a design surface, still an example string |
| `b.md:122` | "**the requester's loop** already emits `session`/`event` nodes, so the two vocabularies must be visibly distinct" | Risk commentary, and it is *proactively defensive*: B5 names a real namespace-collision landmine and resolves it toward generic naming |
| `b.md:131` | "the requester's 'dream consolidation' must be expressible in it" | Trade-off commentary |
| `b.md:148` | "bound to the `ocasazza.omp-auto-loop` source" | Explicit overfitting guard: the source id appears only as a *catalog binding value* |
| `c.md:151` | "The requester's 'dream' consolidation is one transform package… The pest package continues to be the only place the **omp projection** is named" | Overfitting guard |
| `c.md:15` | "`graph_session {epoch, graph_revision}`" | **Pre-existing** repo struct quoted as grounding, not introduced by C |

**Verdict on CK-002**: the overfitting discipline the requester asked for is real and
independently arrived at by all three explorers. The residual vocabulary is motivational
language, not schema. One flag worth carrying forward: `b.md:84`'s `transform/dream` branch
name is a precedent — if it survives into implementation it becomes a branch name a user sees,
and that is the first place requester vocabulary could leak into a user-visible identifier.

---

## 4. Stage 4 — Checklist results (all 22 items × 18 proposals)

Importance: **E** = essential · **I** = important · **O** = optional · **P** = pitfall
(a pitfall **YES** is a *defect*).

### 4.1 Per-item matrix

| # | Imp | A1 | A2 | A3 | A4 | A5 | A6 | B1 | B2 | B3 | B4 | B5 | B6 | C1 | C2 | C3 | C4 | C5 | C6 |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| CK-001 | E | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y |
| CK-002 | E | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y |
| CK-003 | E | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y |
| CK-004 | E | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y |
| CK-005 | I | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y |
| CK-006 | I | **N** | **N** | Y | Y | Y | **N** | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y |
| CK-007 | E | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | **N** | Y |
| CK-008 | E | Y | **N** | Y | **N** | Y | **N** | Y | **N** | Y | **N** | **N** | **N** | Y | **N** | Y | **N** | **N** | **N** | **N** |
| CK-009 | E | **N** | **N** | **N** | **N** | **N** | **N** | Y | **N** | **N** | **N** | **N** | **N** | Y | Y | **N** | **N** | **N** | **N** | **N** |
| CK-010 | I | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y |
| CK-011 | I | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y |
| CK-012 | I | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y |
| CK-013 | I | **N** | **N** | **N** | **N** | **N** | **N** | **N** | **N** | **N** | **N** | **N** | **N** | **N** | **N** | **N** | **N** | **N** | **N** | **N** |
| CK-014 | E | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y |
| CK-015 | I | **N** | **N** | Y | **N** | **N** | **N** | **N** | **N** | **N** | **N** | **N** | **N** | Y | Y | Y | Y | **N** | **N** |
| CK-016 | E | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y |
| CK-017 | O | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y |
| CK-018 | P | N | N | N | N | N | N | N | N | N | N | N | N | N | N | N | N | N | N | N |
| CK-019 | P | N | N | N | **Y** | **Y** | N | N | **Y** | **Y** | N | **Y** | N | N | **Y** | **Y** | **Y** | **Y** | **Y** |
| CK-020 | P | N | N | N | N | N | N | N | N | **Y** | N | N | N | N | **Y** | N | N | N | N |
| CK-021 | P | N | N | N | N | N | N | N | N | N | N | N | N | N | N | N | N | N | N | N |
| CK-022 | P | N | N | N | **Y** | N | N | N | N | N | **Y** | **Y** | N | N | N | **Y** | N | N | **Y** |

### 4.2 Deciding quote per non-trivial item

**CK-001 (E) — all YES.** Decisive for the two designs that push hardest on this gate:
- a.md:89-92 — "The package shape stays format_version=3 and `engine = \"tvix\"`; the only new field is `[parser] role = \"transform\"` (default `\"generate\"`)… `render_expression` renders `( <expr> ) { graph = <injected>; name = <literal>; … }`."
- b.md:66 — "The package gains `[parser] input = \"graph\"` (default `\"none\"`, preserving `generate-*.toml`). The rendered call becomes `( <expr> ) { graph = import /jc/input/graph.nix; <bindings> }`."
- a.md:103 — "The Rust side is a `crates/importer` type, **not a crate**" (b.md:69 adds the `graph-transform` crate; the *transformation* is still the package, so both pass — see §3.3).

**CK-002 (E) — all YES.** See §3.5. Decisive quote for the two designs that come closest:
- b.md:117 — "`kind = transform`, `kind = schedule`, `kind = run` nodes" and b.md:119 "No new endpoints, no new ledger type; provenance is edges."
- a.md:109 — `RunRecord { id, source_id, transform_id, trigger: Watch|Poll|Manual, … }` — pure infrastructure vocabulary.
The only agent-adjacent strings in any core type are the `jc:run:*` reserved namespace (b.md:117) and the branch-name example `transform/dream` (b.md:84).

**CK-003 (E) — all YES.** Grep for `JavaScript|three.js|OrbitControls|Cosmograph|TypeScript|npm|vite|webpack` across all three files returns **zero hits inside the jc-timeline proposals** (all hits are in unrelated `auto-loop-*` files in the same directory). Every UI element is named as a Rust/Dioxus seam: `update_colors` / `update_sizes` / `set_selected` / `set_positions` / `mount_canvas` / `graph_canvas::load` (a.md:120-121, c.md:15), `render::apply_delta` (b.md:71), `graph_data_from_snapshot` (c.md:88).

**CK-004 (E) — all YES.** a.md:122-123 — "`prefers-reduced-motion` swaps the pulse for a static highlight." · b.md:71 — "`prefers-reduced-motion` snaps." · c.md:74 — "reduced-motion snaps." All three sets also list reduced motion as a binding constraint (`a.md:45`, `b.md:20`, `c.md:31`).

**CK-005 (I) — all YES (weakly).** Each set names honest progress as a constraint (`a.md:45` "Honest progress", `b.md:20` "honest progress", `c.md:31` "Honest progress"), and no approach synthesises a number: a.md:118 "honest countdown from `due_ms`", a.md:203 "a real due time and **never invents one**", b.md:71 "honest `m:ss` until next", c.md:48 "the in-flight run with **measured progress**". **Caveat recorded for all 18**: the in-flight run's own progress is specified only in c.md.

**CK-006 (I) — A1, A2, A6 NO.** a.md's constraint block (`a.md:42-46`) names "Honest progress, reduced-motion variants, one hierarchy per surface" but **not** panel-kit loading stores, and A1 introduces a new async panel: "A new Runs panel (past strip · now marker · upcoming strip…)" (a.md:117-118). A2 inherits it ("the User view gets the same Runs panel as A1 but backed with commits", a.md:165-166); A6 inherits A1's UI (a.md:292 "The transform contract, Nix library, `eval_transform`, and the node-content work are exactly A1's"; a.md:312 "Animation | as A1"). A3/A4/A5 are YES **vacuously** — no new asynchronously-loaded panel: A3 "let the existing Progress + Timeline panels… animate from that stream" (a.md:191-192); A4 "evaluated by the existing `/search/matches` set primitive… with no remount, ever" (a.md:235); A5 names no new surface. b.md and c.md are YES on their set constraints: b.md:20 "panel-kit loading stores", and c.md:31 "the existing loading-store discipline in the UI"; c.md:100 makes it explicit ("`stage()` steps expressed through `panel_kit::loading` stores").

**CK-007 (E) — C6 NO.** c.md:139 requires "the timeline scrubber asks the server to evaluate the transform at an arbitrary `t` between two input revisions" — a new, unnamed server call with no reuse justification. **Everyone else YES**, the strongest cases: a.md:162-163 — "`GET /runs/:commit/graph` materialises a `Snapshot` and serves it through the same `/graph/*` buffers with `x-graph-revision` set to a commit-derived id, so the canvas needs no new wire."; a.md:196-197 (A3) adds **no** endpoints at all; b.md:97 (B3) "Zero server changes in the first increment".

**CK-008 (E) — 11 NO.** The failures split cleanly:
- *Unbounded append/commit (6)*: A2 "a commit per run means a `Snapshot` clone per run" with no retention clause (a.md:181-183); A6 "the CLI's append to `runs.jsonl`" (a.md:290); B2 "commit-per-run of a 1M-node snapshot is heavy (snapshot dedupe by content hash mitigates)" — a cost mitigation, not a bound (b.md:87); C2 "commit growth for high-cadence runs (**needs squash/retention**)" — retention named, not specified (c.md:93); C5 "graph-api swaps a snapshot and **appends a run record**" with no bound (c.md:127); C6 "no run ledger at all… requires the input revision to be retained" (c.md:142).
- *Unbounded in-graph growth (4)*: A4 "the graph grows monotonically (retention = a compaction transform, which is itself a run)" (a.md:245); B4 "unbounded growth needs compaction" (b.md:109); B5 no bound, and "a run touching 10k nodes is a giant star" (b.md:120); C4 "inflates edge counts by O(touched nodes)" (c.md:118).
- *Unnamed store (1)*: B6 — "run boundaries as checkpoints" (b.md:128) with no retention policy for the op-log it depends on.
**YES** for A1 "a ring of N (default 256) alongside the last K (default 8) full snapshots" (a.md:110-111), A3 "nothing beyond the last-N event window" (a.md:200-201), A5 "states 0..k are memoised server-side in a **bounded LRU**" (a.md:264), B1 "a `RunLedger` (ring in `AppState`…)" (b.md:69), B3 "each run stores `{record, delta}` in a **bounded ring**" (b.md:95), C1 "a ring of the last N runs in `AppState`" (c.md:74), C3 "persist to localStorage under `jc_runs_v1` with a **bounded count**" (c.md:100).

**CK-009 (E) — 15 NO. Only B1, C1, C2 YES.**
- b.md:76 — "**Risks.** `deepSeq` on a 1M-node attrset can stall the blocking thread — needs a **per-run time budget** and `JC_MAX_*` caps applied to input as well as output", combined with `RunRecord { …, status, … }` (b.md:69) → a budgeted, capped, status-bearing run.
- c.md:47 — "**a failed run never publishes a partial graph**", with `outcome: Ok{…}|Failed{msg}|Skipped{Unchanged}` (c.md:74).
- c.md:86 (C2) — "**A failed run commits nothing** and is recorded as a branch-scoped event in `/progress`."
**A1 is the consequential NO** (see §6): a.md:141-142 names "the caps in `lib.rs`" only as an *input-side performance mitigation* ("tvix eval of a big injected graph is slow — mitigate with the caps… and a per-package `[limits] input_nodes`"). It specifies no failed-run outcome for a cap breach, no visible reason, and no time budget — while conceding "full rebuild per run (metrics + Tantivy) caps cadence at tens of seconds" (a.md:138-139). A5 names the hazard precisely and mitigates nothing: "tvix has no fuel/step limit (`lib.rs` caps only output), so a divergent `step` **hangs the blocking thread**" (a.md:272). B3/C3 name it and leave it: "wasm eval blocks the UI thread above the caps (the retired tvix-worker would need restoring)" (c.md:105); "the browser thread limits graph size (the tvix worker with timeout is retired)" (b.md:98). C6 names the *consequence*: "error attribution for failed runs **vanishes**" (c.md:144).

**CK-010 (I) — all YES.** a.md:143 "the closure bug may bite `graph-transform.nix` if it imports `graph.nix` — keep the `{ graph }` parameter convention"; a.md:96 "taking `{ graph }` like the combinators"; b.md:21 "**tvix closure bug**: library files take `{ graph }` as a parameter (`graph-combinators.nix:3-12`)"; c.md:27 "`{ graph }` parameter passing, no closure across 3+ import levels"; c.md:72 "`graph-iter.nix { graph }:` provides `mapNodes/filterNodes/foldNodes/…` all `listToAttrs`-based and taking `{ graph }` like `graph-combinators.nix`".

**CK-011 (I) — all YES.** Citations resolve. Samples: a.md:9 `crates/importer/src/tvix.rs`, a.md:139-140 `convert_generated_graph`/`focused_node`/`nodes.rs`, a.md:144 `RunRecord`; b.md:66 `lib.rs:122-205`, b.md:69 `rebuild_snapshot` (`watcher.rs:476`), b.md:71 `positions_cpu()`; c.md:11 `tvix.rs:512-566`, c.md:13 `server.rs:1753-1824`, c.md:15 `render/pipelines.rs:1743`.

**CK-012 (I) — all YES.** a.md:320-326 ("An Obsidian user gets 'runs' for save bursts… A Kubernetes operator gets a `collapse-by-namespace` transform… An httpjson tenant (Hindsight memory bank)…"); b.md:75 ("Kubernetes source: hourly `ContractChains` collapses ReplicaSet→Pod fans; Obsidian: `LabelCommunities` writes a `community-label` tag from Louvain; OpenAlex: periodic citation-cluster relabelling"); c.md:80 ("an Obsidian user writes 'label every note with its folder depth and connect orphans to an inbox node' once… a Kubernetes user contracts ReplicaSet→Pod edges nightly").

**CK-013 (I) — all 18 NO.** Grep `knowledge|charts/jump-cannon|wikilink` over all three files: the only hit is b.md:158, a *grounding citation* — "docs/importer-architecture.md:178-212,625-640 … `charts/jump-cannon/knowledge/OMP Auto-Loop Topos.md`" — which reads the note, not updates it. No proposal names a note to write or a wikilink to add. **Uniform failure; see §9 issue 2.**

**CK-014 (E) — all YES.** No proposal's expression reads a file or a socket. a.md:93-95 "injects the current snapshot… as a top-level `let graph = builtins.fromJSON ''…''` binding — the same host-injection idiom `eval_seed` uses"; b.md:66 "the VFS already exists (`lib.rs:122-205`), and the import is lazy so an expression that only touches `graph.nodes` never forces `edges`"; b.md:130 "Nix is the configuration language, **Nix never sees the data**"; c.md:72 "served through the VFS as `/jc/input/graph.json` and read with `builtins.fromJSON`". a.md:268 (A5) is the closest to a violation — "Impure inputs (a vault reload) enter as a *second* stream" — but that is non-determinism of the *input*, not a filesystem/network read inside the expression, so it passes.

**CK-015 (I) — 12 NO.** YES: A3 (`run_finished { run_id, revision, counts, error? }`, a.md:198-199, rendered by the existing Progress panel); C1 ("`/runs` (JSON) lists past + in-flight + next… **Runs panel** (past/in-flight/upcoming, **outcome, counts, error**)", c.md:74, plus "a failed run never publishes a partial graph", c.md:47); C2 ("A failed run commits nothing and is recorded as a branch-scoped event in `/progress`", c.md:86); C3 (inherits C1's `Run.outcome`, c.md:100); C4 ("a `run:<id>` node with metadata `{started, finished, **outcome**, added, removed}`", c.md:115). **A1 is a consequential NO**: `RunRecord` carries `error` (a.md:110) but the panel specification (a.md:117-123) says nothing about a failed run, an empty graph, or whether the previous state is retained — a run whose node set differs triggers "a differing node set remounts through `graph_canvas::load`" (a.md:121-122) with no statement of what a *failed* run remounts to.

**CK-016 (E) — all YES.** a.md:3-4 "Phase 1 of Tree-of-Thoughts. Propose only." · b.md:4 "**Status:** Research only (Tree-of-Thoughts phase 1); nothing here is implemented" · c.md:4 "**Scope:** Design options only. Nothing here is implemented." No diffs, no patches, no completion claims anywhere.

**CK-017 (O) — all YES.** Every heading carries a probability: a.md:80 "probability 0.85", a.md:222 "0.08", b.md:62 "probability 0.90", c.md:78 "**Probability:** 0.85. **Complexity:** medium-high." Note b.md:7 correctly disclaims them — "Probabilities are sampling/selection likelihoods, not confidence."

**CK-018 (pitfall) — all 18 NO (no defect).** Nobody conflates the position ring with run history; two sets say so outright (a.md:36-38 "it scrubs a `FrameRing` of *layout positions*, not graph states"; c.md:15 "it is a layout scrubber, not a graph-state timeline"). Where the panel is reused it is reused *as a distinct track*: a.md:205-206 "The existing Timeline panel gets a second track: below the layout-frame scrub, a run track of markers"; b.md:95 "a second track: 'graph states' beside 'positions'"; c.md:100 `enum Frame { Positions(..), GraphState { run_id, ids, edges, colors } }` — two variants, not a replacement. A4/B4/B5/B6/C5/C6 do not reference `timeline.rs` at all.

**CK-019 (pitfall) — 10 YES (defects).** Triggered: A4 ("Upcoming runs are `run` nodes with `status = scheduled` **emitted ahead of time by the scheduler stage**" — an unnamed new producer, a.md:238); A5 ("the scheduler is **a clock that increments `k` on cadence**", a.md:265); B2 ("the standalone graph-api **runs the same scheduler struct in-process**", b.md:82, with its own duplication risk); B3 ("Runs are scheduled by a **`spawn_forever` ticker** (the Timeline panel's `ensure_ticker` pattern) with a per-transform interval", b.md:95); B5 ("The scheduler **is a Rust transform** that reads `schedule` nodes", b.md:117); C2 ("**A scheduler task** in `session-manager`… or graph-api… `TransformSchedule { world, branch, transform, cadence }`", c.md:86); C3 ("**schedule = browser timer**", c.md:104); C4 ("kept current by **the scheduler rewriting that one node**", c.md:115); C5 ("schedule them as Kubernetes CronJobs **admitted by Kueue**", c.md:125); C6 ("the Rust side **walks the sequence against the wall clock**", c.md:137). **Not triggered**: A1 ("on the source's existing `WatchPlan` cadence", a.md:84-85), A2 (WatchPlan + a `schedule.local.json` overlay, a.md:172-173), A3 ("emitted by `spawn_poll` / the filesystem rescan timer one tick ahead", a.md:202), A6 ("jump-cannon deliberately does not grow a scheduler", a.md:280), B1 ("the existing poll driver pattern (`spawn_poll`)", b.md:69), B4/B6.

**CK-020 (pitfall) — 2 YES (defects).** B3: self-contradictory on the point — "runs identically in a worker" (b.md:95) against "**the browser thread** limits graph size (the tvix worker with timeout is retired)" (b.md:98). C3: unambiguous — "the Dioxus app **evaluates transform packages in-browser** with `tvix-wasm` (as the Generate panel already does)" (c.md:98) with "wasm eval **blocks the UI thread** above the caps (the retired tvix-worker would need restoring, `lib.rs:39-41`)" (c.md:105) and the risk restated unmitigated: "eval on the main thread" (c.md:108). The other 16 all run server-side or in a CLI process; a.md:135 states it flatly: "**Key decisions.** Server-side eval".

**CK-021 (pitfall) — all 18 NO (no defect).** No proposal loads N historical runs into the live `ArcSwap<GraphSnapshot>`. A3 is the cleanest: "nothing beyond the last-N event window… **the past state is not retained**" (a.md:200-201, 218-219). Ledgers live beside it, not inside it: "a bounded in-memory run journal" (a.md:85), "a `RunLedger` (ring in `AppState`…)" (b.md:69), "a ring of the last N runs in `AppState`" (c.md:74). C5 names the swap explicitly as a *replacement*, not an accumulation: "graph-api swaps a snapshot and appends a run record" (c.md:127). **Flagged but not scored as this pitfall**: the in-graph designs (A4, B4, B5, C4) and the memo-key design (C6) do hold *all* history inside the one live snapshot — but not "as though runs were additional snapshots of it", so the gate does not fire; the unbounded-growth consequence is charged to **D3** instead, where A4/B4/B5/C4 score 1.

**CK-022 (pitfall) — 5 YES (defects).** Triggered where the transform's output *becomes* the live graph with no separately addressable artifact and no stated failure isolation: A4 "A run does not replace the graph; it **appends** to one graph" with "the graph grows monotonically" and no failure path (a.md:226-227, 245); B4 "one snapshot holds **all history**… `VaultNode` gains `intervals: Vec<(u32,u32)>`" (b.md:105-106); B5 "no new persistence… **provenance is topology**" (b.md:119) — the run is not an artifact; C4 "node content is enriched by traversal **rather than by new wire fields**" (c.md:113); C6 "**no run ledger at all**… a run's identity is (package hash, variables hash, input revision, t), which is a **memo key** rather than a record" (c.md:139) plus "error attribution for failed runs vanishes" (c.md:144). **Not triggered**: A1 (run record + `/runs/:id/diff` + K retained snapshots, `apply` returns `Result`, a.md:103-115), A2 ("consolidation is a merge with **recorded conflicts**, not a silent overwrite", a.md:158-160; branch `runs/<source_id>`), A3, A5 (states addressed by index `k`), A6 (inherits A1), B1 (`RunLedger` + `delta`), B2 (commits on `transform/<id>` while `main` tracks raw, b.md:84), B3 (client ring), B6 (op-log checkpoints), C1 ("a failed run never publishes a partial graph", c.md:47), C2 (commits), C3 (`jc_runs_v1` ring), C5 (record + swap).

### 4.3 Tally per proposal

| # | Essential Y/N | Important Y/N | Optional | Pitfall YES (defects) | Disqualified |
|---|---|---|---|---|---|
| A1 | 6Y / **2N** (CK-009) | 6Y / **4N** (006,013,015) | Y | 0 | — |
| A2 | 5Y / **3N** (008,009,015) | 6Y / **3N** (006,013,015) | Y | 0 | — |
| A3 | 5Y / **3N** (009,015) | 7Y / **2N** (013,015) | Y | 0 | — |
| A4 | 4Y / **4N** (008,009,015) | 6Y / **4N** (006,013,015) | Y | **2** (019,022) | — |
| A5 | 4Y / **4N** (008,009,015) | 7Y / **3N** (013,015) | Y | **1** (019) | — |
| A6 | 4Y / **4N** (008,009,015) | 6Y / **4N** (006,013,015) | Y | 0 | — |
| B1 | **7Y / 0N** | 7Y / **2N** (013,015) | Y | 0 | — |
| B2 | 5Y / **3N** (008,009,015) | 6Y / **3N** (013,015) | Y | **1** (019) | — |
| B3 | 5Y / **3N** (009,015) | 7Y / **2N** (013,015) | Y | **2** (019,020) | — |
| B4 | 4Y / **4N** (008,009,015) | 6Y / **4N** (013,015) | Y | **1** (022) | — |
| B5 | 4Y / **4N** (008,009,015) | 6Y / **4N** (013,015) | Y | **2** (019,022) | — |
| B6 | 4Y / **4N** (008,009,015) | 6Y / **4N** (013,015) | Y | 0 | — |
| C1 | **7Y / 0N** | 7Y / **2N** (013,015) | Y | 0 | — |
| C2 | 6Y / **2N** (008,015) | 6Y / **3N** (013,015) | Y | **1** (019) | — |
| C3 | 5Y / **3N** (009,015) | 7Y / **2N** (013,015) | Y | **2** (019,020) | — |
| C4 | 4Y / **4N** (008,009,015) | 6Y / **4N** (013,015) | Y | **2** (019,022) | — |
| C5 | 4Y / **4N** (008,009,015) | 6Y / **4N** (013,015) | Y | **1** (019) | — |
| C6 | 5Y / **3N** (008,009,015) | 6Y / **4N** (007,013,015) | Y | **2** (019,022) | — |

**Disqualification set (CK-001, CK-002, CK-003, CK-014, CK-016): empty — 0 of 18
disqualified.** No proposal is infeasible-as-written. That is a finding in itself: the
explorers all understood the hard laws.

---

## 5. Stage 5 — Rubric scores with anchor placement

**Placement legend**: `=2` matches score_2 · `2<·<4` strictly between (naming the lean) ·
`=4` matches score_4 · `>4` better than score_4 **on this axis** · `<2` worse than score_2.
Every row carries **both** sides: the closer anchor's text paired with the artifact text that
occupies that role, and the further anchor's text paired with the artifact text that falls short
(or a named absence).

Anchor shorthand used below (verbatim fragments from the specification):
- **D1** 2=`pub struct Run { pub session: String, pub kind: RunKind, pub next_event_at: i64, pub agent_tool: String }` · 4=`pub struct Run { pub id, transform, bound, input_revision, output_revision, status, delta } // a third party implements \`Transform\` (expr + variable schema + apply(g, bound) -> Graph)`
- **D2** 2=`[transform] engine = "omp-loop" / expr = "<read ~/.local/state/omp-auto-loop/graph.lines>" / cadence_ms = 5000` · 4=`format_version = 3 / [parser] / engine = "tvix" / expr = "gcl: graph: windowed {…}" / [[parser.variables]] name = "window"; type = "integer"; default = 50`
- **D3** 2=`runs: HashMap<RunId, serde_json::Value> // full { nodes, links } JSON per run, unbounded retention` · 4=`struct RunDelta { added, removed, changed } // one delta per run; full keyframe every K runs; ring retention of N runs`
- **D4** 2=`// per tick: re-fetch /graph/init, /graph/ids, /graph/edges and rebuild every buffer` · 4=`// per frame: patch the changed node/edge ranges into the existing GPU buffers; the revision guard already in graph_canvas::load is the admission check for every frame`
- **D5** 2=`"upcoming": [next_run_at(now+3000ms), …] // a tokio task per transform; no miss or overlap policy` · 4=`enum RunPlan { Manual, Every(Duration), Daily(TimeOfDay) } // evaluated by the existing watch-plan tick loop; missed runs coalesce to one; no overlap`
- **D6** 2=`// new panel tab "agent status": last tool call, session goal, loop iteration — shown only when source_id starts with "pest:omp"` · 4=`pub struct NodeMeta { .., provenance: Provenance { first_seen_run, last_seen_run, changed_in }, first_seen_at, last_seen_at } // served by the existing GET /node/*id; every importer gains it`
- **D7** 2=`Phase 1: build the transformation runtime + scheduler + animated timeline + node inspector (12 months)` · 4=`P1 manual one-shot transform + run record + runs list … P5 node provenance fields on NodeMeta`
- **D8** 2=`Risks: eval may be slow; scheduling could race; performance at scale.` · 4=`tvix has no fuel/timeout and eval is inline + CPU-bound -> per-run node/edge budget under JC_MAX_NODES/JC_MAX_EDGES, run on spawn_blocking as POST /generate does, wall-clock ceiling recorded as a failed run; foldl'-of-addNode is O(n^2) -> genList + one listToAttrs; closures across 3+ import levels break -> combinators take { graph } explicitly; /graph/* fetches are non-atomic -> the existing revision guard admits every frame`

**Scoring rules applied**: (i) a score of 4 claims the anchor behaviour for *every* instance;
(ii) `4` requires **both** halves of the score_4 excerpt — where one half is absent the score is
3 and the missing half is named as the further-from artifact text; (iii) a pitfall YES caps the
mapped dimension at 2 (mapping: CK-019→D5, CK-020→D4, CK-022→D3, CK-018/021→no instance fired).

---

### A1 — Transform packages + run journal in graph-api (p 0.85)

| Dim | Closer to | Further from | Placement | Score |
|---|---|---|---|---|
| D1 **.100** | 4 — `RunRecord { id, source_id, transform_id, trigger, started_ms, finished_ms, revision_before, revision_after, …, error }` (a.md:108-110) + `data_loader::Transform` trait `apply(&VaultGraph, &dyn ImportProgress) -> Result<VaultGraph>` (a.md:103-105) | 4's `bound: BTreeMap<String, Value>` — absent: bindings live as `variables` on the struct, not in `apply` | `=4` | **4** |
| D2 **.133** | 4 — "package shape stays format_version=3 and `engine = \"tvix\"`; the only new field is `[parser] role = \"transform\"`… `( <expr> ) { graph = <injected>; name = <literal>; … }`" (a.md:89-92) | 4's typed `[[parser.variables]]` table is never restated; `role` is a field the anchor does not have | `=4` (both use the existing engine; `role` is a parser field, not a new envelope) | **4** |
| D3 **.133** | 4 — "a ring of N (default 256) alongside the last K (default 8) full snapshots" (a.md:110-111) | 4's stored `RunDelta{added,removed,changed}` — absent: the record holds **counts** (a.md:110) and 8 full snapshots are retained | `2<·<4`, lean none (bounded + on-demand diff, but no stored id delta and 8 whole-graph copies) | **3** |
| D4 **.133** | 4 — "added nodes pulse via `update_colors` + `update_sizes`, removed nodes dim via `set_selected`" (a.md:120-121) | 4's "revision guard … is the admission check for **every frame**" — absent; `?revision=` appears only on remount (a.md:121-122) | `2<·<4`, lean toward 4 (in-place patch, no per-tick refetch; guard not named as a per-frame admission check) | **4** |
| D5 **.134** | 4 — "on the source's existing `WatchPlan` cadence" (a.md:84-85) → the run plan *is* the serialisable `WatchPlan` enum | 4's "missed runs coalesce to one; **no overlap**" — absent; `upcoming: [{ due_ms, transform_id }] derived from the `Poll` interval" (a.md:112) states no miss/overlap policy | `2<·<4`, lean none | **3** |
| D6 **.100** | 4 — "`NodeMeta` proto gains `provenance { source_id, transform_id, run_id, first_seen_run, last_changed_run, change }` and `facts: [{key, value, field_type}]`… Obsidian, Kubernetes, and pest nodes all get richer with **zero source-specific code**" (a.md:127-133) | 4's `first_seen_at`/`last_seen_at` timestamps and `changed_in: Vec<RunId>` — absent (only `first_seen_run`/`last_changed_run`) | `=4` | **4** |
| D7 **.133** | 4 — set-level landable first slice: "Node-content work is approach-independent and should **land first**: project Nix `metadata` → `frontmatter`… add schema-driven `facts` and a `provenance` block to `NodeMeta`, and render both in `focused_node`" (a.md:329-332) | 4's P1–P5 sequence — absent: A1 has no phase list of its own; its three subsystems are described as one design | `2<·<4`, lean none (one named first slice, no phase sequence) | **3** |
| D8 **.134** | 4 — "tvix eval of a big injected graph is slow — mitigate with **the caps in `lib.rs`**… the closure bug may bite `graph-transform.nix`… keep the `{ graph }` parameter convention" (a.md:141-143) | 4's foldl'-of-addNode O(n²)/soupGen and non-atomic `/graph/*` landmines — absent | `=4` (no passage reads like "eval may be slow; scheduling could race") | **4** |

**Total: 3.600** → **capped to 2.000** (CK-009 essential NO)

### A2 — Runs as graph-vcs commits; timeline is the branch log (p 0.82)

| Dim | Closer to | Further from | Placement | Score |
|---|---|---|---|---|
| D1 .100 | 4 — "every run is committed to a `graph-vcs` branch (`runs/<source_id>`)… `GET /runs` maps to `log(branch, n)`" (a.md:148-161): the run is a transformation applied to a graph revision | 4's structured `Run` struct — absent: the run record is a **commit message string** `transform:<id> trigger:<poll|watch|manual> vars:<…>` (a.md:156) | `2<·<4`, lean none | **3** |
| D2 .133 | 4 — "Same transform package and Nix library as A1" (a.md:147) | 4's variable table — inherited unstated | `=4` | **4** |
| D3 .133 | 4 — "the diff is `commit.ops`" (a.md:150-151): a per-run delta in `Commit{ops: Vec<GraphOp>}` | 2 — "a commit per run means a `Snapshot` clone per run" (a.md:181-182) **and** no retention clause on the log | `2<·<4`, lean none (delta present, whole-graph copy and unbounded retention present) | **3** |
| D4 .133 | 4 — "serves it through the same `/graph/*` buffers with `x-graph-revision` set to a commit-derived id, **so the canvas needs no new wire**" (a.md:162-163); "checkout a commit into the canvas (remount, or in-place when node sets match)" (a.md:166-167) | 4's per-frame revision guard — absent | `=4` | **4** |
| D5 .134 | 4 — "`schedule.local.json` overlay (`interval_ms` or a fixed `at: [ISO…]` **list**)" (a.md:173-174): a declared, serialisable plan | 4's miss/overlap semantics — absent | `2<·<4`, lean none | **3** |
| D6 .100 | 4 — "`NodeMeta.history: [{commit_id, op, ts}]` from walking the log for that node id (bounded to last 32)… the Inspector shows 'this node was added in run 12, relabelled in run 15'" (a.md:177-179) | 4's `first_seen_at`/`last_seen_at` — absent | `=4` | **4** |
| D7 .133 | 4 — inherits A1's node-content slice on the existing `/node/*id` (a.md:177 "As A1, plus…") | 4's phasing — absent; the first value needs the `MinigrafStore` wiring (a.md:153-155) | `2<·<4`, lean none | **3** |
| D8 .134 | 4 — "log walks for per-node history are **O(runs × ops)** without an index" (a.md:184-185): a named, quantified landmine | 4's tvix/closure/atomicity landmines and their existing-code countermeasures — absent; "Coupling the User view to VCS internals" (a.md:184) has no countermeasure | `2<·<4`, lean none (specific but unmitigated) | **3** |

**Total: 3.366** → **capped to 2.000** (CK-008, CK-009 essential NO)

### A3 — Instrument the existing rebuild: run events on `/progress` (p 0.80)

| Dim | Closer to | Further from | Placement | Score |
|---|---|---|---|---|
| D1 .100 | 4 — `run_started { run_id, source_id, trigger, transform_id?, due_ms? }`, `run_touched { run_id, added, removed, changed }`, `run_finished { run_id, revision, counts, error? }` (a.md:194-199) — every field names a transformation against a graph revision | 4's `bound: BTreeMap<String, Value>` and `status` — absent; and 4's third-party `Transform` is only *inherited* ("The transform itself is A1's package + `eval_transform` stage", a.md:199) | `2<·<4`, lean none | **4** |
| D2 .133 | 4 — inherits A1's package (a.md:199) | 4's variable table — inherited unstated | `=4` | **4** |
| D3 .133 | 4 — "nothing beyond the last-N event window… **the past state is not retained**" (a.md:200-201, 218-219): bounded deltas, zero whole-graph copies | 4's "full keyframe every K runs" — absent entirely; the delta stream has no anchor to replay from | `2<·<4`, lean none (perfect on boundedness, absent on the keyframe half) | **3** |
| D4 .133 | 4 — "clicking a marker replays `run_touched` as a colour pulse (`update_colors`) and a dim of removed nodes (`set_selected`)" (a.md:206-208); indices are "against the *new* revision's `/graph/ids`" (a.md:196-197) | 4's per-frame revision guard — absent | `=4` | **4** |
| D5 .134 | 2 — "`run_scheduled { run_id, due_ms }` **emitted by `spawn_poll` / the filesystem rescan timer one tick ahead**" (a.md:202): upcoming is a value the timer emits, and the proposal adds no plan of its own | 4's `RunPlan` enum with coalescing/no-overlap — absent | `2<·<4`, lean none (reuses the tick, but the plan half is not a run plan) | **2** |
| D6 .100 | 4 — "`NodeMeta.frontmatter` projection fix + schema-driven Facts as in A1" (a.md:212-213) on the shared node path for every importer | 4's `changed_in: Vec<RunId>` on shared metadata — absent: per-node history is **client-side** ("the app keeps the last N `run_touched` events", a.md:213-214) | `2<·<4`, lean none | **4** |
| D7 .133 | 4 — "**A3 is the cheapest first slice** and composes with A1 or A2 (same events, richer store)" (a.md:337-338) + "no new persistence and no VCS" (a.md:189-190) | 4's P1–P5 sequence — absent; A3 is one slice, and the set notes it is a composition, not a plan | `2<·<4`, lean none | **3** |
| D8 .134 | 4 — "`run_touched` for a 100k-node delta bloats the JSON progress log — **cap and elide beyond 10k indices with an explicit 'truncated' flag**" (a.md:219-220): exact failure, exact threshold, exact countermeasure | 4's tvix/closure/atomicity landmines — not restated (inherited from A1 but unstated in A3) | `2<·<4`, lean none (one landmine, precisely mitigated) | **3** |

**Total: 3.332** → **capped to 2.000** (CK-009 essential NO)

### A4 — Bitemporal nodes and reified runs (p 0.08)

| Dim | Closer to | Further from | Placement | Score |
|---|---|---|---|---|
| D1 .100 | 4 — `transform → run → node` provenance edges, PROV-O style; "the transformation, its runs, and their effects are themselves nodes" (a.md:226-228) | 4's second half — a named externally-implementable `Transform` — absent: A4 never names the Rust type; it says only "The transform stage emits `{ nodes, links }`" (a.md:230) | `2<·<4`, lean toward 4 (unit is purely graph-typed) | **3** |
| D2 .133 | 4 — reuses the existing rewrite rules with no new engine tag or envelope (a.md:19-22) | 4's "expr is a function of declared, typed variables bound at apply time" — absent: A4 defines no package shape | `2<·<4`, lean none | **3** |
| D3 .133 | 2 — "the graph grows monotonically (retention = a compaction transform, which is itself a run)" (a.md:245) | 4 — absent on both counts: no delta, no keyframe, no ring | `<2` — worse than score_2: score_2 at least bounds nothing but stores a per-run JSON value; A4 appends run data **into the single served snapshot** with no bound at all | **1** |
| D4 .133 | 4 — "evaluated by the existing `/search/matches` set primitive and applied through `set_selected` **with no remount, ever**" (a.md:234-235); "the animated DAG is literally the provenance subgraph lighting up as `t` advances" (a.md:237) | 4's per-frame buffer patching / revision guard — absent (a set filter, not a patch) | `=4` (the extreme of "patch only what changed") | **4** |
| D5 .134 | 2 — "Upcoming runs are `run` nodes with `status = scheduled` **emitted ahead of time by the scheduler stage**" (a.md:238): a now-derived value from an unnamed new producer | 4's declared serialisable plan with miss/overlap — absent | `=2` | **2** |
| D6 .100 | 4 — "clicking a node shows its `valid_from`/`valid_to` facts, and its neighbours **include the run and transform that produced it**" (a.md:240-242) | 4's "**new fields on the shared node metadata** served by the existing `GET /node/*id`" — absent: A4 enriches by traversal, not by `NodeMeta` | `2<·<4`, lean none | **3** |
| D7 .133 | 2 — "It changes what 'the graph' means for **every consumer** (metrics, layout, search) and needs a compaction story before it is safe" (a.md:249-250) | 4's P1 — absent: no slice lands alone | `<2` — no landing seam at all | **2** |
| D8 .134 | 2 — "metrics (PageRank, Louvain) are polluted by provenance nodes **unless the metric pass learns to exclude a `provenance` tag**" (a.md:246-247) | 4's "countermeasure that **exists in this codebase**" — absent: the proposed mitigations (a metric-exclusion tag pass, layout filtering) are new work, not existing code | `2<·<4`, lean none (specific, but every countermeasure is prospective) | **3** |

**Total: 2.600** → **capped to 2.000** (CK-008, CK-009 essential NO; pitfall caps D3, D5)

### A5 — Lazy stream evaluator (p 0.06)

| Dim | Closer to | Further from | Placement | Score |
|---|---|---|---|---|
| D1 .100 | 4 — "A 'run' is `(stream_id, k, wall_clock)`" (a.md:264) — an application of a transformation, in graph terms | 4's `bound`/`input_revision`/`output_revision`/`status` — absent; no `Transform` interface named at all | `2<·<4`, lean none | **3** |
| D2 .133 | 2 — "A transform package's `expr` returns not a graph but a lazy **stream** of graphs" (a.md:254-255) + "Rust gains `tvix_wasm::eval_stream(expr, k, fuel)`" (a.md:262-263) | 4's "the expression **returns a graph**" with `[[parser.variables]]` — A5 changes the return contract, so the existing engine's graph contract no longer holds and a sibling evaluator is required | `2<·<4`, lean none (same package envelope, altered contract) | **3** |
| D3 .133 | 2 — "**every state is a full graph, so memory is states × graph**" (a.md:274) | 4's `RunDelta` + keyframes — absent | `=2` (bounded by the LRU, but a full graph per state) | **2** |
| D4 .133 | 2 — A5 discusses no canvas seam at all; "the host forces element *k*" (a.md:255-256) is the whole rendering story | 4's per-frame GPU buffer patching — **artifact lacks: any rendering/patching integration** | `2<·<4`, lean none (neither refetch nor patch) | **2** |
| D5 .134 | 2 — "the scheduler is **a clock that increments `k` on cadence**" (a.md:265) | 4's declared `RunPlan` with miss/overlap — absent | `=2` (indistinguishable: now-derived, interval-driven, no policy) | **2** |
| D6 .100 | 2 — A5's approach text contains no node-content proposal (a.md:252-276) | 4's shared `NodeMeta` change — **absent**; the set's closing "node-content work is approach-independent" (a.md:329) is a cross-cutting recommendation, not part of A5's design | `2<·<4`, lean none (worse than 2: not even a per-source panel) | **2** |
| D7 .133 | 2 — "**Needs an evaluator feature (fuel / cancellation) tvix does not expose at the pinned rev**" (a.md:275-276) | 4's P1 — absent: the first prerequisite is upstream in tvix | `2<·<4`, lean none | **2** |
| D8 .134 | 2 — "tvix has no fuel/step limit (`lib.rs` caps only output), so a divergent `step` hangs the blocking thread" (a.md:272) — precise, with no countermeasure for any of the three named risks | 4's "per-run node/edge budget under `JC_MAX_NODES`… run on `spawn_blocking`… recorded as a failed run" — entirely absent | `2<·<4`, lean none (score_4's specificity, score_2's absence of mitigation) | **3** |

**Total: 2.367** → **capped to 2.000** (CK-009 essential NO; pitfall caps D5)

### A6 — External timers + `Push` + CLI (p 0.07)

| Dim | Closer to | Further from | Placement | Score |
|---|---|---|---|---|
| D1 .100 | 4 — "The transform contract, Nix library, `eval_transform`, and the node-content work are exactly A1's" (a.md:291-292) | 4's run record — **artifact lacks: any `Run` field list**; "Past runs are the CLI's append to `runs.jsonl`" (a.md:290) names a file, not a schema | `2<·<4`, lean toward 2 | **3** |
| D2 .133 | 4 — inherits A1's package shape exactly (a.md:291) | 4's variable table — inherited unstated | `=4` | **4** |
| D3 .133 | 2 — "the CLI's append to `runs.jsonl` next to the overlay files" (a.md:290-291): unbounded append, no retention | 4's ring + keyframes — absent | `=2` on the unbounded axis (not a whole-graph copy, so not worse) | **2** |
| D4 .133 | 4 — "Animation | as A1" (a.md:312) → A1's in-place `update_colors`/`update_sizes`/`set_selected` path (a.md:120-121) | 4's per-frame revision guard — absent | `=4` | **4** |
| D5 .134 | 4 — "The catalog source definition gains `scheduler: { kind = systemd-timer \| cron \| k8s-cronjob, ref }`… `upcoming` from `systemctl list-timers --output=json`… **never from a guess**" (a.md:286-289): a declared, serialisable plan whose next-fire is the real scheduler's answer | 4's miss/overlap semantics — absent | `2<·<4`, lean toward 4 (declared plan + authoritative, not now-derived) | **3** |
| D6 .100 | 4 — node-content work "exactly A1's" (a.md:292) → the shared `NodeMeta` `facts`+`provenance` widening (a.md:127-133) | 4's `first_seen_at`/`last_seen_at` — absent | `=4` | **4** |
| D7 .133 | 4 — "best as a *later* adapter on top of A1's `/runs` contract, **not the first design**" (a.md:300-301): an explicit sequencing decision, and the CLI lands on the existing `WatchPlan::Push` trigger (a.md:283) | 4's P1–P5 — absent (one sequencing note, no phase list) | `2<·<4`, lean none | **3** |
| D8 .134 | 2 — "three scheduler adapters is a maintenance tax, **dev on macOS has no systemd**" (a.md:296-297) | 4's tvix/closure/atomicity landmines with existing-code countermeasures — absent; no risk is paired with a mitigation | `<2` on the mitigation axis: these are product complaints, not this repo's landmines | **2** |

**Total: 3.099** → **capped to 2.000** (CK-008, CK-009 essential NO)

### B1 — Transform packages + run ledger in graph-api (p 0.90)

| Dim | Closer to | Further from | Placement | Score |
|---|---|---|---|---|
| D1 .100 | 4 — `RunRecord { run_id, transform_id, variables, trigger, scheduled_at, started_at, finished_at, status, input_revision, output_revision, delta }` (b.md:69) **plus** "`trait Transform { fn id(&self) -> &str; fn apply(&self, input: &VaultGraph, ctx: &RunCtx) -> Result<TransformOutput, TransformError>; }`" (b.md:69) in a wasm-clean crate with built-in impls | 4's `apply(g, bound)` takes a `&RunCtx` rather than a `bound: BTreeMap<String,Value>` — a naming difference only; the axis (graph-typed run, externally implementable `Transform`) is fully met | `=4` | **4** |
| D2 .133 | 4 — "The package gains `[parser] input = \"graph\"` (default `\"none\"`, preserving `generate-*.toml`). The rendered call becomes `( <expr> ) { graph = import /jc/input/graph.nix; <bindings> }`" (b.md:66) — the anchor's shape, plus the anchor's own laziness property | 4's explicit `[[parser.variables]]` typed-default table — inherited from the existing engine rather than restated | `=4` | **4** |
| D3 .133 | 4 — "**Whole-graph output, not deltas** (Rust computes the delta with `graph_vcs::diff_snapshots`)… `GET /runs/:id/delta` (ids added/removed/changed as dense indices + revisions)" (b.md:73, 69); store is "a `RunLedger` (**ring** in `AppState`…)" (b.md:69) | 4's "full keyframe every K runs" — absent: B1 keeps only the head graph as the anchor, so deltas past the ring cannot be replayed | `=4` (a per-run id-level delta into a bounded ring with no per-run whole-graph copy; the missing periodic keyframe is a *stronger* bound, not a weaker one, on this axis) | **4** |
| D4 .133 | 4 — "the canvas gains `render::apply_delta` — on revision advance, `reload_graph` **carries positions by id** from `positions_cpu()` into the new scene so unchanged nodes stay put; added nodes spawn at the centroid of their neighbours and fade in over 400 ms" (b.md:71) | 4's "the revision guard … is the admission check for every frame" — b.md:13 names the revision header as the trigger but never names the guard as a per-frame admission check | `2<·<4`, lean toward 4 | **4** |
| D5 .134 | 4 — "`transforms = [{ package, variables, schedule = { every_ms \| on_change \| manual } }]`" (b.md:69) — a declared, serialisable plan with a typed variant enum, one case away from the anchor's `enum RunPlan { Manual, Every(Duration), Daily(TimeOfDay) }` | 4's "missed runs coalesce to one; **no overlap**" — absent; and `on_change` is a trigger mode `WatchPlan` does not provide | `2<·<4`, lean toward 4 (the plan enum is nearly verbatim the anchor; the miss/overlap half is unstated) | **3** |
| D6 .100 | 4 — "`RawNode`/`RawEdge` gain `#[serde(flatten)] extra` so tvix metadata reaches `frontmatter`; `/node/:id` gains `edges: [...]`, `provenance {source_id, importer_id, importer_version, graph_revision, last_run_id}`, `history: [run_id, op]`… the Inspector renders schema-declared fields as a typed table" (b.md:71, 158) on the existing `/node/:id` for every importer | 4's `first_seen_at`/`last_seen_at` — absent (`graph_revision` instead) | `=4` | **4** |
| D7 .133 | 4 — set-level decomposition: "The three conventional approaches agree on the contract (`input = \"graph\"`, `graph-transform` crate, metadata flattening into frontmatter, position carry-over by id) and **differ only in persistence/scheduling/host**" (b.md:146) — a shared core that lands before any host-specific work | 4's P1–P5 — absent: B1 states no phases of its own | `2<·<4`, lean none | **3** |
| D8 .134 | 4 — "`deepSeq` on a 1M-node attrset can stall the blocking thread — needs a **per-run time budget** and `JC_MAX_*` caps applied to input as well as output. Position carry-over must **key on id, not index**, or a reorder scrambles the scene." (b.md:76) — two named landmines, each with a countermeasure that exists in this codebase, plus the set's constraint list naming the closure bug and the packages-not-crates law | 4's foldl'-of-addNode/soupGen and non-atomic `/graph/*` landmines — absent | `=4` | **4** |

**Total: 3.733 — UNCAPPED (all 7 essential items YES). Field-highest.**

### B2 — Runs are graph-vcs commits; History panel is the timeline (p 0.85)

| Dim | Closer to | Further from | Placement | Score |
|---|---|---|---|---|
| D1 .100 | 4 — "The transform contract and Nix-side library are **identical to Approach 1**" (b.md:82) → the externally-implementable `Transform` trait; run = a transformation applied to a snapshot | 4's structured `Run` struct — absent: the run is a commit plus `ChangeId` (b.md:82) with no `status`/`delta` field of its own | `2<·<4`, lean none | **3** |
| D2 .133 | 4 — inherits Approach 1's `input = "graph"` package verbatim (b.md:82) | 4's variable table — inherited | `=4` | **4** |
| D3 .133 | 2 — "**commit-per-run of a 1M-node snapshot is heavy** (snapshot dedupe by content hash mitigates)" (b.md:87): a full snapshot per run; dedupe is a cost mitigation, not a retention bound | 4's `RunDelta` + keyframe + ring — absent | `=2` | **2** |
| D4 .133 | 4 — "clicking a commit **pushes that snapshot to the canvas through the existing `graph_data_from_snapshot` path**, with the position carry-over from Approach 1" (b.md:84) | 4's per-frame patch — the animation is fed by a **materialised Snapshot** per commit, not by a delta onto the live buffers | `2<·<4`, lean none | **3** |
| D5 .134 | 2 — "the standalone graph-api **runs the same scheduler struct in-process**" (b.md:82); "Upcoming" is "the scheduler's next-fire list" | 4's declared serialisable plan with miss/overlap — absent; the two-host duplication is named as a risk, not designed | `=2` | **2** |
| D6 .100 | 4 — "Node history is `vcs` op-log filtered by node id (`OpLogEntry`), so `/node/:id/history` is a store query rather than a new index" (b.md:84) — generic, every importer | 4's "**new fields on the shared node metadata** served by the existing `GET /node/*id`" — B2 adds a separate route instead, and Approach 1's `NodeMeta` widening is not restated | `2<·<4`, lean none | **3** |
| D7 .133 | 2 — "the standalone graph-api binary today has no VCS store; **wiring one in means every deployment grows a minigraf file**" (b.md:87); "Session-manager coupling makes the local `just dev-up` path heavier" | 4's P1 — absent: nothing lands on an existing seam first | `2<·<4`, lean none | **2** |
| D8 .134 | 4 — "Two sources of truth if the served graph tracks `main` but runs commit elsewhere; **must define that the served snapshot is the head of the configured branch**" (b.md:89) + "snapshot dedupe by content hash mitigates" (b.md:87): two landmines with code-grounded mitigations | 4's tvix/closure/atomicity set — not restated (inherited from Approach 1) | `=4` | **4** |

**Total: 2.867** → **capped to 2.000** (CK-008, CK-009 essential NO; pitfall caps D5)

### B3 — Client-side lazy pipeline over the fetched graph (p 0.80)

| Dim | Closer to | Further from | Placement | Score |
|---|---|---|---|---|
| D1 .100 | 4 — "each run stores `{record, delta}` in a bounded ring" (b.md:95) with Approach 1's `RunRecord` fields, plus the shared wasm-clean `graph-transform` crate (b.md:95) — a run is a transformation applied to a graph revision | 4's `apply(g, bound)` — present, but the run's `input_revision`/`output_revision` are the client buffers' state, unnamed in B3 | `=4` | **4** |
| D2 .133 | 4 — "The `graph-transform` crate is wasm-clean by construction (like `importer` with `--no-default-features`), so `TvixTransform::apply` runs identically in a worker" (b.md:95): the same package text, a new host | 4's variable table — inherited | `=4` | **4** |
| D3 .133 | 4 — "each run stores `{record, delta}` in a **bounded ring**" (b.md:95) — a per-run id-level delta, no whole-graph copy | 4's keyframe — absent, as in B1 | `=4` | **4** |
| D4 .133 | 4 — "The Timeline panel gets a second track: 'graph states' beside 'positions'; **scrubbing applies the delta to the live scene** with the same `set_positions` hold semantics" (b.md:95) | 4's per-frame revision guard — absent; and "hold semantics" means positions are *held*, not patched, during the scrub | `2<·<4`, lean none | **3** |
| D5 .134 | 2 — "Runs are scheduled by a **`spawn_forever` ticker** (the Timeline panel's `ensure_ticker` pattern) with a per-transform interval persisted in localStorage"; "'upcoming runs' are only **as durable as the tab**" (b.md:95, 98) | 4's plan evaluated by the existing watch-plan tick loop — absent: a client ticker, not the server tick; no miss/overlap policy | `=2` | **2** |
| D6 .100 | 4 — "the client derives edges/history for the focused node from its own buffers and ring; `/node/:id` still supplies title/properties/body" (b.md:158) | 4's "**new fields on the shared node metadata** served to every importer" — absent: B3 explicitly changes nothing server-side (b.md:97), so no importer gains a field | `2<·<4`, lean none (generic, but not the shared path) | **3** |
| D7 .133 | 4 — "**Zero server changes in the first increment**; runs are per-viewer; server-side execution is a later `POST /transform` mirror of `/generate`" (b.md:97): an explicit increment that lands on an existing seam, with the second named | 4's P1–P5 — absent beyond these two | `2<·<4`, lean toward 4 | **3** |
| D8 .134 | 2 — "**Diverges from 'packages bound by an administrator'** — a per-viewer schedule is not deployment configuration; risk of building a second, weaker run model" (b.md:100) + "the browser thread limits graph size (the tvix worker with timeout is retired)" (b.md:98) | 4's "countermeasure that exists in this codebase" — absent: B3 names the landmines (including the retired worker) and pairs none with a mitigation | `2<·<4`, lean none | **3** |

**Total: 3.232 → pitfall caps D4 (3→2) and D5 (already 2) → 3.099** → **capped to 2.000** (CK-009 essential NO)

### B4 — Temporal graph: validity intervals, timeline as a render-time filter (p 0.08)

| Dim | Closer to | Further from | Placement | Score |
|---|---|---|---|---|
| D1 .100 | 4 — "every node and edge carries `[valid_from, valid_to)` in run-index units" (b.md:105): a transformation indexed by revision interval | 4's externally-implementable `Transform` — absent; 4's structured `Run` — absent (the run is an interval) | `2<·<4`, lean none | **3** |
| D2 .133 | 4 — no new engine, no bespoke package field; transforms are the existing rules (b.md:105-106) | 4's typed-variable expr bound at apply time — absent | `2<·<4`, lean none | **3** |
| D3 .133 | 2 — "**unbounded growth needs compaction**" (b.md:109) | 4's `RunDelta` + keyframe + ring — absent | `<2` — worse than score_2: score_2 bounds nothing but stores a per-run value; B4 grows the one served snapshot without limit and admits it | **1** |
| D4 .133 | 4 — "scrubbing and animation **cost nothing and never reload**"; the renderer "adds a `t` uniform" (b.md:104-106) | 4's "patch the changed node/edge ranges into the existing GPU buffers" — B4's mechanism is a uniform, i.e. the extreme of not rebuilding | `=4` | **4** |
| D5 .134 | 2 — Schedule recorded as "**any**" (b.md:143); the model derives every state from `t` and has no run plan at all | 4's declared plan with miss/overlap — absent entirely | `=2` | **2** |
| D6 .100 | 4 — "Node click shows the node's **entire lifetime**… history is then literally the node's list of intervals" (b.md:106) | 4's shared `NodeMeta` fields — absent (enriched by traversal) | `2<·<4`, lean none | **3** |
| D7 .133 | 2 — "it changes `VaultGraph`, **every importer's output**, metrics, facets, the binary cache, and the shaders… **Blast radius across seven crates**" (b.md:109) | 4's P1 — absent: no slice lands alone | `<2` — the anchor's score_2 is a single long phase; B4 has no landing seam at all | **1** |
| D8 .134 | 4 — "conflicts with the `SourceEvent`/delta plan in `docs/importer-architecture.md` **unless intervals are derived from that op-log rather than replacing it**" (b.md:111): a named, document-cited landmine with a stated resolution | 4's tvix/closure/atomicity landmines and their existing-code countermeasures — absent; only one of two risks is mitigated | `2<·<4`, lean none | **3** |

**Total: 2.467** → **capped to 2.000** (CK-008, CK-009 essential NO; pitfall caps D3)

### B5 — The graph schedules itself: runs/schedules/transforms as nodes (p 0.07)

| Dim | Closer to | Further from | Placement | Score |
|---|---|---|---|---|
| D1 .100 | 4 — "`kind = transform`, `kind = schedule`, `kind = run` nodes with `run -> run (next)`, `run -> transform`, `run -> touched node` edges" (b.md:115) | 4's structured `Run` and third-party `Transform` — absent (a graph projection, not a record) | `2<·<4`, lean none | **3** |
| D2 .133 | 4 — "A source's transforms are **declared as today**" (b.md:117): the existing package mechanism, no new engine tag or envelope | 4's typed-variable example — inherited | `=4` | **4** |
| D3 .133 | 2 — "a run touching 10k nodes is a **giant star**" (b.md:120); "meta-nodes pollute metrics (PageRank of a `run` hub), layouts, and counts" | 4's delta + keyframe + ring — absent; no bound stated | `<2` — unbounded per-run edge fan-out written into the single served graph | **1** |
| D4 .133 | 2 — "The Runs panel is a **saved search** (`tags:run kind:upcoming`) rendered as a table" (b.md:117) | 4's per-frame GPU buffer patching — **artifact lacks: any animated-DAG frame mechanism**; the timeline is a facet filter and a table, not a per-frame patch | `2<·<4`, lean none (neither refetch nor patch) | **2** |
| D5 .134 | 2 — "emits `run` nodes whose `scheduled_at > now` are the upcoming list" (b.md:117) | 4's declared plan with miss/overlap — absent | `=2` | **2** |
| D6 .100 | 4 — "Node click on a data node shows its `run` neighbours = its history; node click on a `run` node shows its delta as neighbours. The Inspector's neighbour pills already render the lineage" (b.md:117) | 4's shared `NodeMeta` fields — absent | `2<·<4`, lean none | **3** |
| D7 .133 | 4 — "**No new endpoints, no new ledger type**; provenance is edges" (b.md:119) — a small change layered on the existing transform stage | 4's P1–P5 — absent | `2<·<4`, lean none | **3** |
| D8 .134 | 2 — "reserved namespace collisions with pest packages that emit `run` kinds; the requester's loop already emits `session`/`event` nodes, so the two vocabularies must be visibly distinct" (b.md:122) | 4's codebase landmines with existing-code countermeasures — absent; no risk is paired with a mitigation | `=2` | **2** |

**Total: 2.466** → **capped to 2.000** (CK-008, CK-009 essential NO; pitfall caps D3, D5)

### B6 — Nix compiles a combinator program; Rust executes lazily over a `GraphOp` stream (p 0.05)

| Dim | Closer to | Further from | Placement | Score |
|---|---|---|---|---|
| D1 .100 | 4 — the unit is a transformation over an op stream; `Program::compile(ast) -> Pipeline` (b.md:128) | 4's `Run` struct — absent: "every run is a **window over the op-log**", run boundaries are "checkpoints" (b.md:126, 128) with no identity, status, or bound | `2<·<4`, lean toward 2 | **3** |
| D2 .133 | 2 — "the tvix expression evaluates **not to a graph but to a small AST** of `map \| filter \| reduce` combinators" + a sibling `eval_program` (b.md:126, 128) | 4's expr-returns-a-graph with `[[parser.variables]]` — the return contract is replaced by a bespoke program AST | `2<·<4`, lean none | **3** |
| D3 .133 | 4 — "every run is a **window over the op-log**… deltas are free, and node history is a seek in the log" (b.md:128); per-run marginal cost is a checkpoint marker, no copy | 4's "ring retention of N runs" / keyframes — absent; the op-log's own retention is unspecified | `2<·<4`, lean toward 4 | **3** |
| D4 .133 | 2 — B6 says nothing about pixels; it lands the `SourceEvent`/`GraphOp` boundary "as a prerequisite" (b.md:128) | 4's per-frame GPU buffer patching — **artifact lacks: any rendering integration** | `2<·<4`, lean none | **2** |
| D5 .134 | 2 — schedule recorded as "**checkpoints**" (b.md:144); no run plan, no next-fire concept at all | 4's declared `RunPlan` — absent | `2<·<4`, lean none (no interval, so marginally worse than score_2's interval) | **2** |
| D6 .100 | 4 — "node history is a **seek in the log**" (b.md:128) — generic across importers | 4's shared `NodeMeta` fields — absent | `2<·<4`, lean none | **3** |
| D7 .133 | 2 — "**Depends on unlanded op-stream work**" (b.md:133); "This lands the `SourceEvent`/`GraphOp` boundary from `docs/importer-architecture.md` Phase 3 **as a prerequisite**" (b.md:128) | 4's P1 — absent: the first change is someone else's unlanded plan | `=2` | **2** |
| D8 .134 | 2 — "A DSL inside Nix inside TOML is three layers of syntax for users; vocabulary churn until real transforms exist" (b.md:133) | 4's this-repo landmines with existing-code countermeasures — **absent: B6's risk section names no code landmine at all** | `=2` | **2** |

**Total: 2.466** → **capped to 2.000** (CK-008, CK-009 essential NO)

### C1 — Transform packages as an importer decorator + run ledger (p 0.85)

| Dim | Closer to | Further from | Placement | Score |
|---|---|---|---|---|
| D1 .100 | 4 — `Run { id, transform_id, started, finished, input_revision, output_revision, outcome: Ok{added,removed,changed}\|Failed{msg}\|Skipped{Unchanged}, duration }` (c.md:74) + `GraphTransform { package, variables, schedule }` with `apply(&VaultGraph) -> Result<TransformOutcome>` composed as `TransformedImporter<I: Importer>` through the **existing** `Importer` trait (c.md:72) | 4's `bound` inside `apply` — C1 carries bindings on the struct instead; the third-party implementation path is the existing `Importer` trait, not a new one | `=4` | **4** |
| D2 .133 | 4 — "A transform package is a **normal `format_version = 3` tvix package** with `[parser] input = \"graph\"`… `render_expression` gains one more binding" (c.md:72) | 4's typed `[[parser.variables]]` table — inherited unstated; and `render_expression` gains a *bespoke graph literal binding*, not only declared variables | `=4` | **4** |
| D3 .133 | 4 — "graph-api keeps a **ring of the last N runs** in `AppState`… `outcome: Ok{added,removed,changed}`… `/runs/:id/diff` serves the diff as **dense node indices** against the output revision" (c.md:74) | 4's keyframe — absent; `runs.local.jsonl` is append-only with no stated rotation | `=4` (bounded delta per run, no per-run whole-graph copy) | **4** |
| D4 .133 | 4 — "selecting a run fetches that run's diff and **tweens colors/alpha via `set_search_highlights`** (added = highlight, removed = ghost) and positions via `set_positions`" (c.md:74) | 4's "revision guard … admission check for every frame" — absent | `2<·<4`, lean toward 4 | **4** |
| D5 .134 | 4 — "Scheduling **reuses `WatchPlan::Poll { interval_ms }`** and adds one optional `schedule = \"every 5m\" \| \"cron …\"` on the binding; `next_run_at` is computed from it and served" (c.md:74) | 4's "missed runs coalesce to one; **no overlap**" — absent; and C1 itself flags "cron parsing scope creep" (c.md:79) | `2<·<4`, lean toward 4 | **3** |
| D6 .100 | 4 — "`NodeMeta` gains `provenance { source_id, first_seen_revision, last_run_id }`; the Nodes focus view gains a generic **Facts** section (every frontmatter key/value, tags, degree, neighbours) above the Document viewer **so metadata-only nodes are no longer blank**" (c.md:74) — the shared path, every importer | 4's `changed_in: Vec<RunId>` and `first_seen_at`/`last_seen_at` — absent | `=4` | **4** |
| D7 .133 | 4 — set-level plan: "Approach 1 as the spine… Approach 2's commit model as the persistence backend where a world exists (with the JSONL ring as the standalone fallback), Approach 3's ring-based scrubber as the animation surface, and Approach 6's `t` binding as an **optional** variable" (c.md:152) — four landable increments in order | 4's P1–P5 within one approach — C1 itself states none; and C1 concedes "protobuf change to `NodeMeta` touches session-manager and app proto regen" (c.md:77) as a coordination cost | `2<·<4`, lean toward 4 | **3** |
| D8 .134 | 4 — "tvix eval time on a 100k-node input; **`deepSeq` forces the whole output so laziness is only inside the expression**" (c.md:79) — a named landmine read from `lib.rs:268-298`; "cron parsing scope creep — **keep to `every <duration>`** plus fixed clock times" (c.md:79) — a narrowing mitigation | 4's `JC_MAX_NODES` budget + `spawn_blocking` + "wall-clock ceiling recorded as a failed run" — absent: the eval-time risk is named and left unmitigated | `2<·<4`, lean none | **3** |

**Total: 3.599 — UNCAPPED (all 7 essential items YES).**

### C2 — Runs as graph-vcs commits; timeline = branch log (p 0.80)

| Dim | Closer to | Further from | Placement | Score |
|---|---|---|---|---|
| D1 .100 | 4 — the transform contract is "as in Approach 1" (c.md:86) → `GraphTransform` + `apply`; the commit's "message and metadata carry the run record (transform id, variables hash, diff counts, duration, outcome)" (c.md:86) | 4's structured `Run` fields on a first-class record — carried in commit metadata, not a `Run` type | `2<·<4`, lean none | **3** |
| D2 .133 | 4 — inherits C1's `format_version = 3` package with `input = "graph"` (c.md:86) | 4's variable table — inherited | `=4` | **4** |
| D3 .133 | 2 — "**commit growth for high-cadence runs (needs squash/retention)**" (c.md:93): unbounded, with retention named but not designed | 4's `RunDelta` + keyframe + ring — absent | `=2` | **2** |
| D4 .133 | 4 — "animates by **materialising two commits client-side** (`graph_data_from_snapshot`, `graph_canvas.rs:174`) and tweening between their buffer layouts" (c.md:88) | 4's per-frame patch onto the live buffers — the tween is fed by two full snapshot materialisations, and C2's own risk names "snapshot materialisation cost in the browser for large graphs" (c.md:93) | `2<·<4`, lean none | **3** |
| D5 .134 | 2 — "A **scheduler task** in `session-manager`… or graph-api… `TransformSchedule { world, branch, transform, cadence }`; each tick…" (c.md:86) | 4's declared serialisable plan evaluated by the existing tick loop — absent; "scheduler duplication between hosts" is named as a risk, not designed (c.md:93) | `=2` | **2** |
| D6 .100 | 4 — "`/node/:id` returns the commit that introduced the node and the last commit that changed its metadata, **plus the same Facts section as Approach 1**" (c.md:88) | 4's `first_seen_at`/`last_seen_at` — absent | `=4` | **4** |
| D7 .133 | 2 — "**worlds are a separate host** from the standalone importer path, so the standalone `graph-api --source=…` user only gets this if graph-api also embeds a store per source" (c.md:91) | 4's P1 — absent: a large conditional build | `2<·<4`, lean none | **2** |
| D8 .134 | 4 — "commit growth for high-cadence runs (**needs squash/retention**)" (c.md:93) names the specific growth mode | 4's codebase landmines with existing-code countermeasures — absent; two of three risks are unmitigated | `2<·<4`, lean none | **3** |

**Total: 2.833** → **capped to 2.000** (CK-008 essential NO; pitfall caps D5)

### C3 — Client-resident transform pipeline on the existing Timeline panel (p 0.80)

| Dim | Closer to | Further from | Placement | Score |
|---|---|---|---|---|
| D1 .100 | 4 — `Run` "as in Approach 1" (c.md:100): `id, transform_id, input_revision, output_revision, outcome` | 4's third-party `Transform` — absent: C1's trait is a `struct` in `app/ui/src/transform.rs`, not a public interface, so the "topos from outside this repo" half of the axis is unaddressed | `2<·<4`, lean toward 4 | **3** |
| D2 .133 | 4 — "`graph-iter.nix` **ships in the embedded VFS so the same package text works server-side later**" (c.md:100): one package text, two hosts — the anchor's engine reuse, preserved | 4's variable table — inherited | `=4` | **4** |
| D3 .133 | 2 — `enum Frame { Positions(..), GraphState { run_id, ids, edges, colors } }` persisted under `jc_runs_v1` "with a bounded count" (c.md:100): bounded, but a full id+edge set stored **per run** | 4's `RunDelta{added,removed,changed}` — absent: `ids`+`edges` per run is the whole topology, not a delta | `2<·<4`, lean toward 2 (bounded, but per-run graph copies) | **2** |
| D4 .133 | 4 — the frame ring "moves between graph states with a tween" (c.md:100) | 4's per-frame patch into the existing buffers — each frame is a whole `ids`+`edges` scene, swapped rather than patched | `2<·<4`, lean none | **3** |
| D5 .134 | 2 — "**schedule = browser timer**… Upcoming runs are the timer's next fire plus the cadence" (c.md:100, 104): now-derived from a tab clock | 4's declared plan on the existing watch-plan tick — absent | `=2` | **2** |
| D6 .100 | 4 — "the app already has `frontmatter_json`; the Nodes focus view gets the Facts section" (c.md:102) | 4's shared `NodeMeta` provenance fields — absent: run facts are "merged **client-side** into a per-run overlay" (c.md:102), so no importer gains a field | `2<·<4`, lean none | **3** |
| D7 .133 | 4 — "**Probability:** 0.80 for a **first vertical slice**; 0.55 as the final shape" (c.md:106) + "**no server changes**" (c.md:104) | 4's P1–P5 — absent; and C3's own risk concedes "**duplication once a server path is wanted**" (c.md:108), i.e. the slice is not a step toward the final shape | `2<·<4`, lean toward 4 | **3** |
| D8 .134 | 4 — "wasm eval **blocks the UI thread** above the caps (the **retired tvix-worker would need restoring, `lib.rs:39-41`**)" (c.md:105): the exact landmine, cited to the line that documents its retirement | 4's countermeasure that *exists* — absent: the countermeasure is something to restore, and C3's restated risk "eval on the main thread" (c.md:108) is unmitigated | `2<·<4`, lean none | **3** |

**Total: 2.866 → pitfall caps D4 (3→2) and D5 (already 2) → 2.733** → **capped to 2.000** (CK-009 essential NO)

### C4 — The reflexive graph: runs, schedules, and transforms are nodes (p 0.08)

| Dim | Closer to | Further from | Placement | Score |
|---|---|---|---|---|
| D1 .100 | 4 — `type = transform\|schedule\|run` nodes with `run→touched node`, `schedule→transform`, `run→run` edges (c.md:113) | 4's third-party `Transform` — absent; the Rust type is only "the transform contract stays tvix `{ graph, … }`" (c.md:115) | `2<·<4`, lean none | **3** |
| D2 .133 | 4 — "The transform contract **stays tvix** `{ graph, … }`" (c.md:115): no new engine, no bespoke field | 4's typed-variable example — inherited | `=4` | **4** |
| D3 .133 | 2 — "**inflates edge counts by O(touched nodes)**" (c.md:118); no bound stated | 4's delta + keyframe + ring — absent; and the proposal notes the `Unchanged` short-circuit stops firing, so the store grows every run | `<2` — worse than score_2: unbounded growth of the single served snapshot | **1** |
| D4 .133 | 4 — "**no new panel**… selecting a run node highlights its neighbourhood (touched nodes) using `set_search_highlights`" (c.md:115); a scrubber is a filter over `run` nodes by `started` | 4's per-frame GPU buffer patching — absent (selection-only) | `=4` (the extreme of not rebuilding) | **4** |
| D5 .134 | 2 — "Schedules are `schedule:<id>` nodes whose metadata holds cadence and `next_run_at`, **kept current by the scheduler rewriting that one node**" (c.md:115): a mutable now-value | 4's declared serialisable plan with miss/overlap — absent | `=2` | **2** |
| D6 .100 | 4 — generic for every source: "the Inspector's neighbour pills already render the lineage" (c.md:115) | 4's "**new fields on the shared node metadata**" — C4 **declines** it by design: "node content is enriched by traversal **rather than by new wire fields**" (c.md:113), "no proto change" (c.md:117) | `2<·<4`, lean none (avoids score_2's per-source panel, but declines score_4's mechanism) | **3** |
| D7 .133 | 4 — "no new persistence, no new endpoints, no proto change" (c.md:117): a small layer on the existing transform stage | 4's P1–P5 — absent | `2<·<4`, lean none | **3** |
| D8 .134 | 2 — "the existing **`Unchanged` short-circuit no longer fires** because provenance nodes change every run" (c.md:120): one real, source-specific landmine | 4's existing-code countermeasures — absent; and "users cannot opt out of the pollution without a filter" (c.md:120) is a UX complaint, not a landmine | `2<·<4`, lean none | **2** |

**Total: 2.732** → **capped to 2.000** (CK-008, CK-009 essential NO; pitfall caps D3, D5)

### C5 — graph-compute engine + Kueue CronJob scheduling (p 0.06)

| Dim | Closer to | Further from | Placement | Score |
|---|---|---|---|---|
| D1 .100 | 4 — the worker "evaluates the Nix package natively, computes the diff, and returns a new CSR + metadata patch" (c.md:127): a transformation applied to a graph | 4's `Run` struct and third-party `Transform` — absent; the run is a gRPC job + an appended record | `2<·<4`, lean none | **3** |
| D2 .133 | 2 — "graph-api gains a **`LoadGraph`-style RPC for transform jobs**" (c.md:127) wrapping the package in a bespoke gRPC envelope | 4's `format_version = 3` package evaluated by the existing engine with typed variables — the *engine* is reused but a new transport envelope is added | `2<·<4`, lean none | **3** |
| D3 .133 | 4 — "the worker… **computes the diff**, and returns a new CSR + metadata patch" (c.md:127): a delta, no per-run copy | 4's "ring retention of N runs" — absent: "graph-api swaps a snapshot and **appends a run record**" (c.md:127) with no bound | `2<·<4`, lean toward 4 | **3** |
| D4 .133 | 4 — "Animation **reuses `/graph/layout/stream` framing** (`server.rs:2374-2385`) by emitting position frames for a morph between old and new layouts" (c.md:127) | 4's per-frame patch into the live buffers — the whole layout is re-emitted per frame, and C5's own risk names "**abusing the position-stream contract for topology changes**" (c.md:132) | `2<·<4`, lean none | **3** |
| D5 .134 | 4 — "Scheduling is delegated to the cluster (CronJob per binding, `next_run_at` read from the CronJob status), so 'upcoming' is **authoritative and survives restarts**" (c.md:127) | 4's "evaluated by the existing watch-plan tick loop" — absent: a job queue (Kueue) + CronJobs, not the repo's tick | `2<·<4`, lean toward 4 (declared, serialisable, authoritative) | **3** |
| D6 .100 | 2 — C5's approach text (c.md:123-133) contains no node-content proposal | 4's shared `NodeMeta` change — **absent** | `2<·<4`, lean none (worse than 2: not even a per-source panel) | **2** |
| D7 .133 | 2 — "the standalone/local user has **neither Kueue nor a worker** (`JUMP_CANNON_COMPUTE_URL` unset), so the feature would be cluster-only — **contrary to 'serve users beyond the requester'**" (c.md:130); "two implementations (local + worker) **or none for local**" (c.md:132) | 4's P1 — absent: nothing lands for the primary deployment | `2<·<4`, lean none | **2** |
| D8 .134 | 2 — "abusing the position-stream contract for topology changes; two implementations (local + worker) or none for local" (c.md:132) | 4's codebase landmines with existing-code countermeasures — absent; no risk is paired with a mitigation | `=2` | **2** |

**Total: 2.633 → pitfall caps D5 (3→2) → 2.499** → **capped to 2.000** (CK-008, CK-009 essential NO)

### C6 — Time as a variable: lazy Nix schedule, pure replay, no ledger (p 0.07)

| Dim | Closer to | Further from | Placement | Score |
|---|---|---|---|---|
| D1 .100 | 4 — "A run's identity is `(package hash, variables hash, input revision, t)`" (c.md:139): a transformation over an input revision | 4's `Run` struct and third-party `Transform` — absent: "a run is a **memo key** rather than a record" (c.md:139) | `2<·<4`, lean none | **3** |
| D2 .133 | 4 — "The package declares `[parser] input = \"graph\"` and a `[parser.schedule] expr = …`" (c.md:139): version-3 tvix, expr a function of declared `{ graph, t }` | 4's `[[parser.variables]]` example — the schedule table is a bespoke extra, though the transform itself is a normal package | `=4` | **4** |
| D3 .133 | 2 — "replaying `t` in the past **requires the input revision to be retained** — which reintroduces Approach 2's store through the back door" (c.md:142) | 4's `RunDelta` + keyframe + ring — absent, and the store it needs is undeclared | `2<·<4`, lean toward 2 (the cost model is snapshots per input revision with no policy) | **2** |
| D4 .133 | 2 — C6 states no rendering mechanism; "'animation' can be continuous in `t`" (c.md:139) is a claim about the model, not the canvas | 4's per-frame GPU buffer patching — **artifact lacks: any rendering integration** | `2<·<4`, lean none | **2** |
| D5 .134 | 4 — "graph-api **evaluates the schedule expr** to get the next 64 fire times (upcoming runs, exactly)" (c.md:139): a declared, serialisable plan that is computed, not guessed | 4's miss/overlap semantics — absent, and the run-identity model makes a "missed" run unrepresentable | `2<·<4`, lean toward 4 | **3** |
| D6 .100 | 2 — C6's approach text (c.md:135-145) contains no node-content proposal | 4's shared `NodeMeta` change — **absent** | `2<·<4`, lean none | **2** |
| D7 .133 | 2 — "reintroduces Approach 2's store through the back door" (c.md:142): the pure design needs another approach's infrastructure; no phases | 4's P1 — absent | `2<·<4`, lean none | **2** |
| D8 .134 | 2 — "users expect a run to be a fact, not a recomputation; a slow transform makes scrubbing unusably slow; **error attribution for failed runs vanishes**" (c.md:144) | 4's this-repo landmines with existing-code countermeasures — **absent; no code landmine is cited at all** | `=2` | **2** |

**Total: 2.500 → pitfall caps D5 (3→2) → 2.366** → **capped to 2.000** (CK-008, CK-009 essential NO)

---

## 6. Stage 6 — Score calculation

### 6.1 Ranked table (pre-cap total, essential cap, final)

| Rank | # | Pre-cap total | Essential NO? | **Final total** | D1 | D2 | D3 | D4 | D5 | D6 | D7 | D8 | Disqualified |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 1 | **B1** | **3.733** | none | **3.733** | 4 | 4 | 4 | 4 | 3 | 4 | 3 | 4 | — |
| 2 | **C1** | **3.599** | none | **3.599** | 4 | 4 | 4 | 4 | 3 | 4 | 3 | 3 | — |
| — | A1 | 3.600 | CK-009 | **2.000** | 4 | 4 | 3 | 4 | 3 | 4 | 3 | 4 | — |
| — | B3 | 3.099 | CK-009 | **2.000** | 4 | 4 | 4 | 2 | 2 | 3 | 3 | 3 | — |
| — | A3 | 3.332 | CK-009 | **2.000** | 4 | 4 | 3 | 4 | 2 | 4 | 3 | 3 | — |
| — | A2 | 3.366 | CK-008, CK-009 | **2.000** | 3 | 4 | 3 | 4 | 3 | 4 | 3 | 3 | — |
| — | A6 | 3.099 | CK-008, CK-009 | **2.000** | 3 | 4 | 2 | 4 | 3 | 4 | 3 | 2 | — |
| — | B2 | 2.867 | CK-008, CK-009 | **2.000** | 3 | 4 | 2 | 3 | 2 | 3 | 2 | 4 | — |
| — | C3 | 2.733 | CK-009 | **2.000** | 3 | 4 | 2 | 2 | 2 | 3 | 3 | 3 | — |
| — | C2 | 2.833 | CK-008 | **2.000** | 3 | 4 | 2 | 3 | 2 | 4 | 2 | 3 | — |
| — | C4 | 2.732 | CK-008, CK-009 | **2.000** | 3 | 4 | 1 | 4 | 2 | 3 | 3 | 2 | — |
| — | C5 | 2.499 | CK-008, CK-009 | **2.000** | 3 | 3 | 3 | 3 | 2 | 2 | 2 | 2 | — |
| — | A4 | 2.600 | CK-008, CK-009 | **2.000** | 3 | 3 | 1 | 4 | 2 | 3 | 2 | 3 | — |
| — | C6 | 2.366 | CK-008, CK-009 | **2.000** | 3 | 4 | 2 | 2 | 2 | 2 | 2 | 2 | — |
| — | B4 | 2.467 | CK-008, CK-009 | **2.000** | 3 | 3 | 1 | 4 | 2 | 3 | 1 | 3 | — |
| — | B5 | 2.466 | CK-008, CK-009 | **2.000** | 3 | 4 | 1 | 2 | 2 | 3 | 3 | 2 | — |
| — | B6 | 2.466 | CK-008, CK-009 | **2.000** | 3 | 3 | 3 | 2 | 2 | 3 | 2 | 2 | — |
| — | A5 | 2.367 | CK-009 | **2.000** | 3 | 3 | 2 | 2 | 2 | 2 | 2 | 3 | — |

**Disqualifications (CK-001/002/003/014/016): 0 of 18.**
**Checklist penalties applied**: essential cap only — 16 proposals capped at 2.0. No `important`
cap (per §1.1). Pitfall dimension caps applied to 10 proposals (CK-019→D5, CK-020→D4,
CK-022→D3) and recorded in each row above.
**Gate source**: the specification's `scoring` block (`essential_fail_cap`, `pitfall_handling`).

### 6.2 The two uncapped proposals, and why the cap is the story

**B1 (3.733) and C1 (3.599) are the only approaches that clear every essential gate**, and they
clear it for the same structural reason: each binds a *typed outcome* to a *bounded, revision-keyed
store*.

- B1: `RunRecord { …, status, input_revision, output_revision, delta }` into "a `RunLedger`
  (**ring** in `AppState`…)" (b.md:69), plus "a **per-run time budget** and `JC_MAX_*` caps
  applied to input as well as output" (b.md:76) — the cap breach has a budget, a ceiling, and a
  status field to land in.
- C1: `outcome: Ok{added,removed,changed}|Failed{msg}|Skipped{Unchanged}` in "a **ring of the last
  N runs** in `AppState`" (c.md:74), plus the explicit guarantee "**a failed run never publishes a
  partial graph**" (c.md:47).

**The single most consequential finding in this evaluation**: **A1 is the highest-quality
proposal in the field on the pre-cap total (3.600 — second only to B1, and ahead of C1 by 0.001),
and it fails the essential cap on CK-009.** A1's cap handling is a *performance* note —
"tvix eval of a big injected graph is slow — mitigate with the caps in `lib.rs` and a per-package
`[limits] input_nodes`" (a.md:141) — never a *behavioural* one. A1 has `RunRecord.error` and
`apply() -> Result<…>`, so the mechanism is one sentence away from compliance, and it concedes the
hang in its trade-offs ("full rebuild per run (metrics + Tantivy) caps cadence at tens of seconds
for large graphs", a.md:138-139) without bounding it. **This is a fixable-in-one-paragraph
defect, and the specification's binary gate cannot see that.** I report it rather than soften it.

### 6.3 Tie-break applied for slot 3

16 proposals tie at exactly 2.000. Specification order: D1, then D3, then D6.

1. D1 = 4 group: **A1 (4,3,4) · A3 (4,3,4) · B3 (4,4,3)**. All other capped proposals have D1 = 3.
2. D3 breaks it: **B3 (D3 = 4)** > A1 (3) = A3 (3). B3 takes slot 3 on the specification's own
   tie-break.
3. A1 vs A3 remain tied on D1, D3 **and** D6 — the chain is exhausted. Fallback (disclosed,
   per §1.3): pre-cap total, A1 3.600 > A3 3.332 → A1 orders above A3.

B3's win is not merely mechanical. The specification's stated rationale for the tie-break is that
D1 and D3 "are the two failure modes that cannot be fixed after the fact" — and B3 is genuinely
better on D3 than A1: B3 stores "`{record, delta}` in a **bounded ring**" (b.md:95) with no
whole-graph state at all, where A1 keeps "the last K (default 8) **full snapshots**" (a.md:111).
**But B3 trips CK-019 and CK-020** — a `spawn_forever` client ticker (b.md:95) and
browser-main-thread eval (b.md:98) — which are the scope-bomb and tab-lockup pitfalls the
specification is most explicit about. I therefore record **A1 as the swap-in** and say why (§11).

---

## 7. Stage 7 — Rule generation

### 7.1 Root-cause analysis and recurrence test

**Issue 1 — 15 of 18 proposals leave the terminal failure path of a repeated transform unspecified.**
Five Whys: (1) a proposal names `caps in lib.rs` as a *performance* mitigation but never as a
*behaviour* → (2) the risk section is written as "what might be slow", not "what does the user see
when it is wrong" → (3) the proposal treats the run record as a success artefact, so failure has
nowhere to land → (4) no convention in the repo's design-writing practice forces a failure-path
sentence, and AGENTS.md's UI rules cover rendering, not transform semantics.
→ Root cause: **systemic pattern** (any design introducing a recurring in-process computation).
Recurrence: cross-task YES (any background/repeating work in any crate); cross-project YES;
frequency HIGH (15/18 here); actionable YES (one sentence, contrastive); token justification YES
(a runaway transform is the spec's stated "default failure mode, not an edge case").
**→ CREATE RULE.**

**Issue 2 — 11 of 18 append per-run state with no retention bound.**
Five Whys: (1) the store is introduced as "keep the runs" → (2) the author reasons about
*retrieval* not *erasure* → (3) the design is written top-down from the happy path → (4) no
convention requires a bound in the same sentence as the store.
→ **Systemic pattern.** Recurrence YES/YES/HIGH/YES/YES. **→ CREATE RULE.**

**Issue 3 — CK-013 knowledge-map update, 18/18 miss.** Five Whys: the `AGENTS.md` rule exists
(verified in the specification's own rationale) but no proposal author treats a design proposal as
a "behavior/deployment change" → root cause: **already covered** by an existing documented
convention. **→ NO RULE** (a rule would duplicate AGENTS.md; fix the authors' reading, not the
rule set).

**Issue 4 — 10/18 introduce a new scheduler runtime (CK-019).** Root cause: no scheduler exists
in the repo, so authors reach for the nearest familiar pattern (a ticker, a CronJob, a fold over
schedule nodes). Recurrence YES, actionability YES — but the *mitigation* is already written into
the spec's pitfall list and is enforced by the checklist gate in this very exercise.
**→ NO RULE** (already covered by the evaluation gate).

### 7.2 Rules created

Two rules in `/Users/casazza/Repositories/ocasazza/jump-cannon/.claude/rules/`. Decomposed (two
distinct concepts, two files), filtered for redundancy against each other and against AGENTS.md
(neither duplicates the panel-kit/loading-store or `prefers-reduced-motion` rules), and reweighted
by impact.

- `transform-terminal-failure-path.md` — **CRITICAL**. A repeating transform with no specified
  failure path hangs or truncates the served graph. 15/18 proposals failed this gate.
- `per-event-store-retention-bound.md` — **HIGH**. An unbounded per-event store is the one cost
  decision that cannot be revisited after the store is in production. 11/18 failed this gate.

Both are grounded in real artifact text (the Incorrect examples are drawn from A1 and A2
respectively), and both use the Description–Incorrect–Correct template with a single concept each.

---

## 8. Stage 8 — Self-verification

| # | Question | Answer | Adjustment |
|---|---|---|---|
| 1 | **Evidence completeness** — did I examine all relevant text? | I read all three files in full (a.md 340 lines, b.md 158, c.md 152) and separately extracted the tails of every line truncated at 768 bytes by the reader (b.md:13, 69, 71, 82, 95; c.md:15, 72, 74, 100), which is where several decisive quotes live — B1's `RunLedger`/`RunRecord` (b.md:69), its `history` field (b.md:71), and C1's `NodeMeta`/Facts/reduced-motion (c.md:74). I verified CK-003 and CK-013 by grep over all three files rather than by reading. | No change; the truncated-line extraction was necessary — three of my YES determinations on CK-006/CK-015 rest on tails I would otherwise not have seen. |
| 2 | **Bias check** — am I influenced by length, polish, or which set reads best? | A1 (340 lines, the most polished set, with a dedicated "Diversity and overfitting check" section and explicit "Judge's pruning hints") landed **capped at 2.0**; B3, which I judge on paper the weaker engineering choice, took slot 3. That is the opposite of a length or polish bias, and I flag the B3 outcome as contestable rather than hiding it. Conversely I did not reward c.md or b.md for volume. | No change. I also declined to soften CK-009 for A1 despite it being the field's most complete design, and recorded the cost of that decision. |
| 3 | **Anchor fidelity** — did I write both sides of every `anchor_comparison` before any number, and stay on the axis? | All 144 placements carry both a closer-to pair and a further-from pair, each with its own quoted anchor text and its own quoted artifact text (or a named absence, e.g. A5/D6 "**artifact lacks: any rendering/patching integration**"). I caught myself drifting on D6 for C4 (initial instinct was to score it like A4/B4 at 3 and treat "declines the shared-metadata mechanism" as worse) and re-derived the placement from the axis only. I never let a proposal's D3 or D8 strength raise a D5 or D1 placement. | C4/D6 held at 3 with the decline stated as the further-from evidence rather than as a score reduction. I also tightened the "4 requires both halves" rule mid-evaluation and re-checked every 4 for a missing second half — this is what held A1/D4, C1/D4, and B1/D5 at 3-4 on defensible rather than generous grounds. |
| 4 | **Comparison integrity** — is my reference result correct, and did I introduce errors? | The reference (§2) was written from the specification alone before scoring, and every MUST traces to a checklist item or anchor. Two places where I had to correct my own analysis: (a) I initially read the aggregation formula literally as `Σ(w·s)/5` and caught the contradiction against the stated 1.0–5.0 range and the 2.0 cap (§1.2); (b) I initially leaned toward applying a built-in 1.0 cap for `important` NOs, then reversed on the grounds that it would tie all 18 at 1.0 via CK-013 and contradicts a spec that names its cap threshold (§1.1). Both corrections are disclosed in the report. | Disclosed. I also verified the CK-001 crate boundary (b.md's `graph-transform` crate) against CK-001's rationale before passing it, rather than tripping the gate on the word "crate". |
| 5 | **Proportionality** — am I uniformly harsh or lenient? | Not uniform, and deliberately so. The essential cap is binary, so 16 proposals are identical at 2.0 — that is the specification's design, not leniency, and I report the pre-cap total alongside it so the ordering is still visible. Among uncapped/uncapped-adjacent work the spread is real: D3 separates 4 (bounded delta: B1, B3, C1) from 3 (bounded but count-only: A1, A2, A3) to 1 (unbounded in-graph growth: A4, B4, B5, C4) — a 3-point spread on the weight-tied dimension. D8 separates 4 (landmine-with-countermeasure: A1, B1, B2) from 2 (risk named with no mitigation: A6, B5, C4, C5, C6). | No change. I report slot 3 (B3) as contestable with a named swap-in (A1) rather than presenting the mechanical tie-break outcome as if it were a clear engineering verdict. |

---

## 9. Issues

| Pri | Issue | Evidence | Impact | Suggestion |
|---|---|---|---|---|
| **1** | **16 of 18 proposals fail an essential gate; only B1 and C1 survive uncapped.** The cap then flattens the field into a 16-way tie at 2.0, so slots 3+ are decided entirely by the D1/D3 tie-break rather than by design quality. | 11/18 NO on CK-008; 15/18 NO on CK-009. The consequential case is **A1**: 3.600 pre-cap (second-highest in the field) capped to 2.0 for naming "the caps in `lib.rs`" as a speed mitigation (a.md:141) and never as a failed-run behaviour. | The specification's own ranking signal is destroyed for 89% of the field, and the field's most complete design is excluded from selection by a defect that is one paragraph to fix. | Before Phase 2, ask A1 (and A3, B3, C3, A5) for a one-paragraph addendum: *what the run record contains when the cap is breached, and what the UI shows.* Re-score the addendum. A1 at 3.600 is the highest pre-cap total outside the selected set. |
| **2** | **CK-013 fails for all 18** — no proposal names the required `charts/jump-cannon/knowledge/` map update. | Grep `knowledge\|charts/jump-cannon\|wikilink` over all three files returns one hit, b.md:158, which *reads* `charts/jump-cannon/knowledge/OMP Auto-Loop Topos.md` as grounding rather than proposing a change to it. | An `AGENTS.md`-mandated, same-commit convention is missed by the entire field. Cheap to fix; guaranteed to be flagged in review. | Add to the Phase 2 brief: "name the specific `charts/jump-cannon/knowledge/` note to create or update and the wikilinks to add, in the same change." B1 is best placed to answer it — it already reads the OMP topos note. |
| **3** | **CK-019 trips 10 of 18** — a new in-process scheduler instead of the existing `WatchPlan`/watcher tick. | A4 "the scheduler stage" (a.md:238); A5 "a clock that increments `k`" (a.md:265); B2 "runs the same scheduler struct in-process" (b.md:82); B3 "`spawn_forever` ticker" (b.md:95); B5 "the scheduler **is a Rust transform**" (b.md:117); C2 "A **scheduler task**" (c.md:86); C3 "schedule = browser timer" (c.md:104); C4 "the scheduler rewriting that one node" (c.md:115); C5 "CronJobs admitted by Kueue" (c.md:125); C6 "walks the sequence against the wall clock" (c.md:137). | Each is a scope bomb in a design proposal — the specification calls it "the classic scope bomb". A5's and C6's are worse: their cadence is derived from the current clock, which is exactly the anti-pattern the D5 anchor names. | Phase 2 must state the run plan as a value on the existing tick. B1's `schedule = { every_ms \| on_change \| manual }` (b.md:69) is the field's cleanest expression of this. |
| **4** | **CK-020 trips 2 of 18** — tvix evaluation on the browser main thread. | C3: "**wasm eval blocks the UI thread** above the caps (the retired tvix-worker would need restoring, `lib.rs:39-41`)" (c.md:105) and "eval on the main thread" as an unmitigated risk (c.md:108). B3 is self-contradictory: "runs identically in a worker" (b.md:95) vs "**the browser thread** limits graph size (the tvix worker with timeout is retired)" (b.md:98). | Tab lockup on large graphs. This bears directly on B3, which holds slot 3. | If B3 is taken forward, the `spawn_blocking` mirror must land with or before the first vertical slice, not as "a later `POST /transform`" (b.md:97). |
| **5** | **The proposals were written to the wrong repository tree.** | All three explorers' outputs exist at `/Users/casazza/.config/nixos-config/.specs/research/`, while their completion payloads claim `.specs/research/jc-timeline-2026-09-25.proposals.{a,b,c}.md` relative to jump-cannon. The spec, meanwhile, *is* in the jump-cannon repo. | Phase 2 will read from the jump-cannon path and find nothing. No content was lost. | Relocate the three files and pin explorer `cwd` to the jump-cannon repo. |
| **6** | **Internal inconsistencies inside A1 and C1.** | a.md:110 `RunRecord` stores added/removed/changed **counts**, but a.md:115 promises `/runs/:id/diff` returning "two LE `u32` index buffers" — with 8 retained snapshots and count-only records, older diffs are not computable. c.md:74 specifies `/runs` as JSON while c.md:64 names "protobuf `NodeMeta` is shared with session-manager and the browser" as a wire-growth axis, with no stated exemption. | Both are in the selected set's lineage. The A1 one is a real hole in its scrub story; the C1 one will be raised in proto review. | A1: store the id-level delta in the record, or state that diffs older than the K-snapshot window are unavailable. C1: state why the run endpoint is exempt from the protobuf rule or serve protobuf. |
| **7** | **The specification's aggregation formula is self-contradictory.** | `total = sum(weight_d * score_d) / 5, giving 1.0-5.0` — the division yields 0.2–1.0. | Any judge applying it literally produces a 0.2–1.0 scale, on which `essential_fail_cap: 2.0` is unreachable and every proposal trivially "passes". | Fix the spec to `total = sum(weight_d * score_d)`. I did not edit the spec; §1.2 records the resolution used. |

---

## 10. Strengths

1. **All three sets independently rediscovered the four load-bearing facts about the codebase,
   with citations that resolve.** `RawNode` dropping Nix metadata (a.md:25-26, b.md:13, c.md:10);
   `timeline.rs` being a layout scrubber (a.md:36-38, c.md:15); `WatchPlan` as the only cadence
   source (a.md:28, b.md:24, c.md:13); `graph-combinators.nix` holding hostless `graph→graph`
   rewrite rules (a.md:19-22, c.md:9). That convergence is the strongest evidence in the
   submission that the exploration was grounded rather than generic.
2. **The overfitting discipline the requester asked for is real.** Zero requester vocabulary in
   any core type, field, endpoint, or panel across all 18 (§3.5). B5 goes further and identifies
   a genuine collision landmine — "the requester's loop already emits `session`/`event` nodes, so
   the two vocabularies must be visibly distinct" (b.md:122) — and resolves it toward generic
   naming rather than away from it.
3. **Every proposal names this repo's actual landmines, and the grounded ones are correctly
   identified.** `eval_seed`'s `n`-binding idiom as the host-injection pattern (a.md:95);
   `foldl'`/lazy-forcing behaviour in `deepSeq` (c.md:79); content-hash snapshot dedupe (b.md:87);
   position carry-over keying on id rather than index (b.md:76); the retired tvix-worker cited to
   `lib.rs:39-41` (c.md:105). This is not generic risk-writing.
4. **B1 is the best single proposal in the field on its own terms** — 3.733, the only proposal
   besides C1 to clear every essential gate, and the one whose D8 pairs each landmine with a
   countermeasure that already exists in the codebase ("a per-run time budget and `JC_MAX_*` caps
   applied to input as well as output"; "key on id, not index").
5. **The sets are honest about their own weaknesses.** a.md:336-340 ("A1 is the minimal complete
   design… A3 is the cheapest first slice"), b.md:146 ("that shared core is the likely selection
   regardless of winner"), c.md:152 (an explicit four-part synthesis). Each set identifies its own
   best idea independently of which approach wins, which is exactly what Phase 1 should produce.

---

## 11. Selection note (specification's `selection_note_format`)

**Selection: B1, C1, B3. Disqualifications: none. No shortfall to report** (3 survivors for 3
slots) — but see the slot-3 caveat, which is the most important note in this report.

**#1 — B1 "Transform packages + run ledger in graph-api" — 3.733**
- *Two dimensions that carried it:* **D3 Per-run state model and cost (4)** and **D8 Risk
  specificity (4)**. D3 because it is one of only three proposals that store a per-run id-level
  delta (`GET /runs/:id/delta`, "ids added/removed/changed as dense indices + revisions", b.md:69)
  into a bounded ring with no per-run whole-graph copy. D8 because its risk section pairs two
  exact landmines with countermeasures that exist in this codebase — "a per-run time budget and
  `JC_MAX_*` caps applied to input as well as output" and "key on id, not index" (b.md:76).
- *Weakest dimension, and what would raise it:* **D7 Incremental deliverability (3)**. B1 states
  no phases of its own; its incrementality is inherited from the set-level shared-core observation
  (b.md:146). To raise it: enumerate P1–P5 in B1's own text — P1 manual one-shot transform + run
  record + runs list (no scheduler, no animation), P2 delta-driven history, P3 the catalog
  schedule on the existing poll tick, P4 animated playback over the existing buffers, P5
  `NodeMeta` provenance — each landing on an existing seam.
- *First phase's kill criterion:* ship P1 (one-shot transform + run record + runs list, server-side,
  no scheduler, no animation) and kill the approach if the tvix expression receiving a
  100k-node input graph does not complete inside a per-run wall-clock budget on `spawn_blocking`,
  or if `diff_snapshots` over consecutive runs costs more than the ~2 s revision-probe cadence
  allows. Either result invalidates the whole graph-into-Nix premise that B1, C1, and A1 share.
- *Requester-specific vocabulary found:* none in B1's core. One flag: "a 'dream' transform can
  commit to `transform/dream`" (b.md:84) — a **branch name** example. If that string reaches
  implementation it becomes a user-visible identifier, and it is the one place requester
  vocabulary could leak into a surface.

**#2 — C1 "Transform packages as an importer decorator + run ledger" — 3.599**
- *Two dimensions that carried it:* **D3 (4)** and **D6 Node-content generality (4)**. D3 via
  `outcome: Ok{added,removed,changed}|Failed{msg}|Skipped{Unchanged}` in "a ring of the last N
  runs" (c.md:74). D6 via the shared-node-path guarantee — "`NodeMeta` gains `provenance { source_id,
  first_seen_revision, last_run_id }`… a generic **Facts** section… so metadata-only nodes are no
  longer blank" (c.md:74) — which is the most direct answer in the field to the requester's "click
  a node, it's not much info".
- *Weakest dimension, and what would raise it:* **D8 Risk specificity (3)**. C1 names the
  `deepSeq` landmine with precision (c.md:79) and narrows the cron scope, but leaves "tvix eval
  time on a 100k-node input" unmitigated and never names the `JC_MAX_*` budget or the
  non-atomic `/graph/*` fetch. To raise it: add the same two lines B1 has — apply `JC_MAX_*` to
  the *input* as well as the output, and use the existing revision guard as the admission check
  for every animation frame.
- *First phase's kill criterion:* land the `GraphTransform` decorator with a **manual** trigger and
  confirm `convert_generated_graph` no longer discards Nix `metadata` (c.md:11 identifies the two
  loss points, `lib.rs:233-246` and `tvix.rs:512-566`). Kill if a tvix package cannot be
  round-tripped so that a `metadata` key set in `graph-iter.nix` reaches `frontmatter_json` on
  `/node/*id` — that single fact is load-bearing for the whole node-content half of the feature.
- *Requester-specific vocabulary found:* none in C1's core. c.md:151's overfitting guard
  ("The pest package continues to be the only place the omp projection is named") is the correct
  behaviour, stated explicitly.

**#3 — B3 "Client-side lazy pipeline over fetched graph" — 2.000 (pre-cap 3.099; won the tie-break on D3 = 4)**
- **⚠ This slot is contestable and the orchestrator should read the reason.** The essential cap
  ties 16 proposals at 2.000; B3 takes slot 3 because it is the only capped proposal with
  **D1 = 4 and D3 = 4** — and the specification's own rationale for that tie-break is that D1 and
  D3 "are the two failure modes that cannot be fixed after the fact". On D3 that judgement is
  correct: B3 stores "`{record, delta}` in a **bounded ring**" (b.md:95) with no whole-graph
  state at all, which is strictly better than A1's "the last K (default 8) **full snapshots**".
- *But B3 trips both of the specification's hardest-warned pitfalls:* **CK-019** — "Runs are
  scheduled by a **`spawn_forever` ticker**… with a per-transform interval" (b.md:95) — and
  **CK-020** — eval on the browser main thread, which B3 states self-contradictorily ("runs
  identically in a worker", b.md:95, versus "**the browser thread** limits graph size (the tvix
  worker with timeout is retired)", b.md:98). B3's own risk names the consequence: "risk of
  building a second, weaker run model that must later be migrated to the server" (b.md:100).
- *My recommendation to the orchestrator, stated as a finding rather than a substitution:*
  **swap slot 3 to A1** (pre-cap 3.600 — the highest pre-cap total in the entire field) unless the
  orchestrator weights the CK-020 tab-lockup pitfall below the D3 tie-break. A1 trips **no pitfall
  at all** and is the only non-browser proposal in the top four. The reason I did not simply swap:
  the specification's procedure is cap → rank → tie-break on D1/D3/D6, and A1 loses that chain to
  B3 on D3. Changing the outcome requires changing the specification, which is the orchestrator's
  call, not mine.
- *Weakest dimension (whether or not it is selected), and what would raise it:* **D5 Scheduling (2,
  pitfall-capped)**. The run plan is a client `localStorage` interval and "upcoming runs are only
  as durable as the tab" (b.md:95, 98). To raise it: state the schedule as a value on the existing
  `WatchPlan::Poll` tick, with missed runs coalesced and no overlap.
- *First phase's kill criterion:* if selected — ship the zero-server-change vertical slice and kill
  the approach if a transform package cannot be evaluated over a real 10k-node client-resident
  graph without blocking the main thread, **or** if a second, browser-only run model has to be
  built at all (B3's own stated risk).
- *Requester-specific vocabulary found:* none in B3's core.

**Diversity check (specification's `diversity_requirement`):** the three are **not** three
phrasings of one design, but the collision must be recorded. **B1 and C1 land their first phase on
the same seam** — a tvix package with `[parser] input = "graph"` plus `/runs` in graph-api — and
keep run state in the same place (a bounded ring in `AppState` plus a JSONL file under the packages
dir: b.md:69 vs c.md:74). **B3 is the pair that differs**, and it is the one carrying this
selection's diversity: run state in the browser under `jc_runs_v1`, evaluation in the client,
**zero server changes** (b.md:97). The genuine design split across the selected three is
**B1 (new `graph-transform` crate, public `Transform` trait, Rust built-ins for 1M-node scale,
typed `{every_ms|on_change|manual}` schedule) vs C1 (no new crate — an `Importer` decorator, with
the explicit guarantee "a failed run never publishes a partial graph") vs B3 (client-resident,
server untouched)**. If the orchestrator takes A1 instead of B3, note that A1 sits between B1 and
C1 on run-state location (in-memory only, deliberately: "No durability across restart (deliberate:
bounded)", a.md:137) — which is the axis the diversity rule names, so A1 and B3 are the
*preferred* pair, and B1/C1 are the colliding one.

---

## 12. Structured result

```yaml
evaluation_report:
  metadata:
    judge: "TimelinePruneJudge3"
    specification: "/Users/casazza/Repositories/ocasazza/jump-cannon/.specs/research/jc-timeline-pruning-spec.yaml"
    artifacts:
      a: "/Users/casazza/.config/nixos-config/.specs/research/jc-timeline-2026-09-25.proposals.a.md"
      b: "/Users/casazza/.config/nixos-config/.specs/research/jc-timeline-2026-09-25.proposals.b.md"
      c: "/Users/casazza/.config/nixos-config/.specs/research/jc-timeline-2026-09-25.proposals.c.md"
    artifact_location_finding: >-
      All three explorers wrote to ~/.config/nixos-config/.specs/research/ rather than the
      jump-cannon .specs/research/ directory their payloads claim. No content lost; Phase 2
      must read from the actual path or the files must be relocated.

  specification_interpretations:
    important_no_cap: >-
      NOT applied. The spec's scoring block defines a cap for essential only and attaches no
      consequence to an important NO. A built-in 1.0 cap would tie all 18 at 1.0 via CK-013
      (which all 18 fail) and would contradict a spec that names its cap threshold.
    aggregation_formula: >-
      total = sum(weight * score), NOT /5. The spec's "/5, giving 1.0-5.0" is self-contradictory;
      the stated range and the 2.0 essential cap both confirm the intended reading.
    cap_collapse: >-
      The 2.0 essential cap is applied before ranking per the spec's procedure, flattening 16
      proposals into an exact tie. Tie-break chain D1 then D3 then D6 decides slots 3+; where the
      chain is exhausted (A1 vs A3) the pre-cap total is used as a disclosed final fallback.

  checklist_summary:
    total_items: 22
    essential_items: 7
    important_items: 10
    optional_items: 1
    pitfall_items: 4
    universal_yes: [CK-001, CK-002, CK-003, CK-004, CK-005, CK-010, CK-011, CK-012, CK-014, CK-016, CK-017, CK-018, CK-021]
    universal_no: [CK-013]
    never_yes: [CK-006 (A1,A2,A6), CK-007 (C6), CK-008 (11 proposals), CK-009 (15 proposals), CK-015 (12 proposals)]
    pitfall_triggers:
      CK-019: [A4, A5, B2, B3, B5, C2, C3, C4, C5, C6]
      CK-020: [B3, C3]
      CK-022: [A4, B4, B5, C4, C6]
      CK-018: []

  disqualifications:
    rule: "NO on CK-001, CK-002, CK-003, CK-014, or CK-016"
    count: 0
    detail: >-
      No approach is infeasible-as-written. All 18 carry the transformation as a
      format_version = 3 tvix package with no new SourceKind, keep agent vocabulary out of the
      shared core, stay in Rust/Dioxus, express no filesystem or network input at eval time, and
      are documents rather than patches.

  ranked_results:
    - {rank: 1, id: B1, name: "Transform packages + run ledger in graph-api", pre_cap: 3.733, final: 3.733, uncapped: true, essential_no: []}
    - {rank: 2, id: C1, name: "Transform packages as an importer decorator + run ledger", pre_cap: 3.599, final: 3.599, uncapped: true, essential_no: []}
    - {rank: 3, id: B3, name: "Client-side lazy pipeline over fetched graph", pre_cap: 3.099, final: 2.000, uncapped: false, essential_no: [CK-009], tiebreak: "D1=4, D3=4 — only capped proposal topping both"}
    - {rank: 4, id: A1, name: "Transform packages + run journal in graph-api", pre_cap: 3.600, final: 2.000, uncapped: false, essential_no: [CK-009], note: "HIGHEST pre-cap total in the field; loses the tie-break on D3=3 vs B3's 4. Recommended swap-in for slot 3."}
    - {rank: 5, id: A3, pre_cap: 3.332, final: 2.000, essential_no: [CK-009]}
    - {rank: 6, id: A2, pre_cap: 3.366, final: 2.000, essential_no: [CK-008, CK-009]}
    - {rank: 7, id: A6, pre_cap: 3.099, final: 2.000, essential_no: [CK-008, CK-009]}
    - {rank: 8, id: B2, pre_cap: 2.867, final: 2.000, essential_no: [CK-008, CK-009]}
    - {rank: 9, id: C2, pre_cap: 2.833, final: 2.000, essential_no: [CK-008]}
    - {rank: 10, id: C3, pre_cap: 2.733, final: 2.000, essential_no: [CK-009]}
    - {rank: 11, id: C4, pre_cap: 2.732, final: 2.000, essential_no: [CK-008, CK-009]}
    - {rank: 12, id: C5, pre_cap: 2.499, final: 2.000, essential_no: [CK-008, CK-009]}
    - {rank: 13, id: A4, pre_cap: 2.600, final: 2.000, essential_no: [CK-008, CK-009]}
    - {rank: 14, id: C6, pre_cap: 2.366, final: 2.000, essential_no: [CK-008, CK-009]}
    - {rank: 15, id: B4, pre_cap: 2.467, final: 2.000, essential_no: [CK-008, CK-009]}
    - {rank: 16, id: B5, pre_cap: 2.466, final: 2.000, essential_no: [CK-008, CK-009]}
    - {rank: 17, id: B6, pre_cap: 2.466, final: 2.000, essential_no: [CK-008, CK-009]}
    - {rank: 18, id: A5, pre_cap: 2.367, final: 2.000, essential_no: [CK-009]}

  gate_source: "specification scoring block (essential_fail_cap 2.0; pitfall caps the affected dimension at 2)"

  vote:
    - B1
    - C1
    - B3
  swap_in_recommended: A1
  swap_in_reason: >-
    Slot 3 is decided entirely by the D1/D3 tie-break that the 2.0 cap creates, not by design
    quality. A1 has the highest pre-cap total in the field (3.600), trips no pitfall, and is the
    only non-browser proposal in the top four. B3 trips CK-019 (spawn_forever client ticker) and
    CK-020 (browser-main-thread eval, stated self-contradictorily at b.md:95 vs b.md:98).
    I applied the specification's procedure as written rather than substituting my own preference.

  strongest_disqualification:
    found: false
    nearest_miss: >-
      A1, the field's most complete design (pre-cap 3.600, second only to B1 and ahead of C1),
      fails essential CK-009: a.md:141 names "the caps in lib.rs" as a speed mitigation on the
      input side and never specifies a cap breach as a recorded failed run with a visible reason
      or any wall-clock ceiling — while conceding at a.md:138-139 that a full rebuild per run
      "caps cadence at tens of seconds for large graphs". Mechanism one sentence away
      (RunRecord.error + apply() -> Result), behaviour absent. Capped 3.600 -> 2.000.

  requester_specific_vocabulary:
    in_core_types_fields_endpoints_or_panels: "none — CK-002 YES for all 18"
    occurrences:
      - {where: "a.md:39-40", quote: "Requester's package: `omp-auto-loop.toml` (pest, N|/E| lines...)", kind: "grounding", assessment: "correctly framed as one generic instance"}
      - {where: "a.md:159-160", quote: "which is exactly what the requester's 'dream consolidation' wants: consolidation is a merge with recorded conflicts", kind: "core design rationale", assessment: "requester metaphor in a rationale sentence, not a field name"}
      - {where: "a.md:297-298", quote: "the requester's 'periodic dream consolidation' wants one-click cadence from the UI", kind: "trade-off note"}
      - {where: "b.md:44", quote: "Requester's loop (pest package + 'dream' consolidation transform) is one *configuration* of the feature, not a code path", kind: "success criteria", assessment: "framing"}
      - {where: "b.md:84", quote: "a 'dream' transform can commit to `transform/dream`", kind: "USER-VISIBLE IDENTIFIER EXAMPLE", assessment: "closest to a leak; a branch name a user could see — must be renamed at implementation"}
      - {where: "b.md:122", quote: "the requester's loop already emits `session`/`event` nodes, so the two vocabularies must be visibly distinct", kind: "risk commentary", assessment: "proactive and correct — names a real namespace collision and resolves toward generic naming"}
      - {where: "b.md:131", quote: "the requester's 'dream consolidation' must be expressible in it", kind: "trade-off note"}
      - {where: "b.md:148", quote: "bound to the `ocasazza.omp-auto-loop` source", kind: "overfitting guard", assessment: "source id appears only as a catalog binding value"}
      - {where: "c.md:151", quote: "The requester's 'dream' consolidation is one transform package... The pest package continues to be the only place the omp projection is named", kind: "overfitting guard"}
      - {where: "c.md:15", quote: "`graph_session {epoch, graph_revision}`", kind: "pre-existing repo struct", assessment: "quoted as grounding, not introduced by C"}

  rules_generated:
    - file: ".claude/rules/transform-terminal-failure-path.md"
      reason: "15/18 proposals leave the terminal failure path of a repeated transform unspecified; systemic, cross-task, high damage (runaway transform is the specification's stated default failure mode)."
      root_cause: "Risk sections are written as 'what might be slow' rather than 'what does the user see when it is wrong', so failure has nowhere to land in the run record."
    - file: ".claude/rules/per-event-store-retention-bound.md"
      reason: "11/18 proposals append per-run state with no retention bound; a cost decision that cannot be revisited once the store is in production."
      root_cause: "Stores are designed for retrieval, not erasure; nothing in the repo's practice requires a bound in the same sentence that introduces the store."
    rejected:
      - {candidate: "name the knowledge-map update", reason: "already covered by AGENTS.md — fix the authors' reading, not the rule set"}
      - {candidate: "never add a new scheduler runtime", reason: "already covered by the spec's CK-019 pitfall gate and enforced in this exercise"}

  confidence:
    level: "High"
    factors:
      evidence_strength: "Strong — all three files read in full, every line truncated at 768 bytes separately extracted, CK-003 and CK-013 verified by grep"
      criterion_clarity: "Clear on all 8 dimensions (each has a concrete score_2/score_4 pair)"
      specification_quality: "Partial — the aggregation formula is self-contradictory and the essential cap destroys the ranking signal for 16 of 18; both resolved in §1 and reported as issues"
```
