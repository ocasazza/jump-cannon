# jump-cannon Transformation-Runs Timeline — Pruning Report (Judge 2)

**Date:** 2026-09-25
**Specification:** `.specs/research/jc-timeline-pruning-spec.yaml` (applied verbatim)
**Artifacts evaluated:** 18 design proposals (6 each from explorers A, B, C)

---

## 0. Where the artifacts actually are

All three explorer agents reported writing
`jump-cannon/.specs/research/jc-timeline-2026-09-25.proposals.{a,b,c}.md`. At the time I evaluated,
those files did **not** exist there. The explorers ran with `cwd = /Users/casazza/.config/nixos-config`,
so their writes landed in the requester's repo:

- `/Users/casazza/.config/nixos-config/.specs/research/jc-timeline-2026-09-25.proposals.a.md` (340 lines)
- `/Users/casazza/.config/nixos-config/.specs/research/jc-timeline-2026-09-25.proposals.b.md` (158 lines)
- `/Users/casazza/.config/nixos-config/.specs/research/jc-timeline-2026-09-25.proposals.c.md` (152 lines)

**I read all three from that nixos-config path**, then Main copied them into
`/Users/casazza/Repositories/ocasazza/jump-cannon/.specs/research/`. I verified the copies are
byte-identical to the originals I scored (`sha256` a: `238295f8…`, b: `1e3bbca4…`, c: `850851a5…`),
so every quote and line reference in this report is valid against **either** location, and the
ranking does not depend on which path is used.

The evaluation specification is at
`/Users/casazza/Repositories/ocasazza/jump-cannon/.specs/research/jc-timeline-pruning-spec.yaml`
and was read verbatim from there. This report is at the assigned jump-cannon path
`…/jump-cannon/.specs/research/jc-timeline-2026-09-25.pruning.2.md`.

---

## 1. Method and the conventions I declared before scoring

### 1.1 My reference result (written before any artifact was scored)

A correct proposal for this brief must do ten things: (1) express the transform as a
`format_version = 3` / `engine = "tvix"` package whose `expr` is a function of declared typed
`[[parser.variables]]`, with the current graph arriving as a value in the existing
`render_expression` output — no new engine tag, no bespoke `[transform]` envelope, no crate per
source; (2) model a run as `Run { id, transform, bound, input_revision, output_revision, status,
delta }` in pure graph vocabulary, with a public extension point a third-party Rust transform can
implement; (3) store one bounded delta per run plus periodic keyframes with an explicit retention
N, keep run state *outside* the single `ArcSwap<GraphSnapshot>`, and land the transform's output as
a distinct addressable artifact; (4) declare `enum RunPlan { Manual, Every(..), Daily(..) }`
evaluated by the existing watch-plan tick loop **with missed-run coalesce and no-overlap stated**;
(5) justify every new endpoint against `/graph/*`, `/node/*id`, `/progress`, `POST /generate`, and
use the existing `graph_canvas::load` revision guard as the per-frame admission check; (6) widen the
*shared* `NodeMeta` path and fix the **root cause** of thin nodes (`RawNode` drops Nix metadata at
`lib.rs:233-246`; `validate_package_schema` rejects `[schema]` at `tvix.rs:290-307`); (7) name the
repository's actual landmines — no fuel/timeout, caps under `JC_MAX_*` recorded as a failed run and
never a hang or a silent truncation, the 3+-import closure bug forcing `{ graph }` parameters, the
O(n²) `foldl'`-of-`addNode` trap, the non-atomic `/graph/*` fetches — each with an in-repo
countermeasure; (8) honest progress, `prefers-reduced-motion`, panel-kit `loading_store` +
`LoadingGate`; (9) the `charts/jump-cannon/knowledge/` note updated in the same change, at least one
non-omp worked example, and a success probability; (10) the `timeline.rs` `FrameRing` recognised as a
layout scrubber, not a run timeline.

### 1.2 Conventions applied uniformly to all 18

These are stated up front because they decide a large share of the answers.

1. **Document-level constraints are part of every approach's proposal text.** The `Constraints`
   blocks in all three files are unscoped. This is load-bearing, not a convenience: set A's
   constraint line reads *"Rust everywhere (Dioxus/wgpu, no JS). Packages not crates … Honest
   progress, reduced-motion variants, one hierarchy per surface"* — it names **no** loading store,
   so CK-006 is NO for A1–A6. Set B's reads *"…panel-kit loading stores; honest progress;
   `prefers-reduced-motion`"* and set C's reads *"honest progress via `panel_kit::loading`"* plus
   *"the existing loading-store discipline in the UI"* — CK-006 is YES for both sets.
2. **CK-009 YES** requires (a) the tvix caps / a per-run budget named as the enforcement point, in
   the approach's own text or via an explicit `"exactly A1's"`-style inheritance, **and** (b) a
   typed failure record for a run.
3. **CK-015 YES** requires the run record to carry a typed error/outcome field, so a failed run is
   marked with a reason.
4. **D7 buckets** — 5: explicit phase list; 4: explicit ≥3-increment ordering; 3: explicit "this is a
   slice / composes with X" on a named seam; 2: no sequencing; 1: no sequencing **and** a named
   unbuilt prerequisite or a rewrite of a shared contract.
5. **D8 buckets** — 4: ≥2 anchor landmines with in-repo countermeasures; 3: 1 anchor landmine, or
   ≥2 specific hazards with countermeasures; 2: specific hazards without countermeasures;
   1: generic worry words.
6. **Silence on a dimension scores 2, not 1.** Score 1 is reserved for artifact text that is
   actively *worse* than the `score_2` anti-pattern.
7. **Pitfall mapping to the capped dimension** — CK-019 → D5; CK-020 → D4; CK-021 and CK-022 → D3.
   CK-018 → D4 (no proposal triggers it).

### 1.3 A defect in the specification (reported, does not change the ranking)

`scoring.formula` reads `"total = sum(weight_d * score_d) / 5, giving 1.0-5.0"`. These two halves
contradict each other: with `total_weight = 1.000` and `score ∈ [1,5]`, `Σ(w·s)/5 ∈ [0.2, 1.0]`.
The `essential_fail_cap: "…caps the total at 2.0"` gate is only meaningful on a 1–5 scale, so the
intended total is `Σ(w·s)`. I report `total` on the 1–5 scale (so the 2.0 cap bites) and print
`spec_formula = total / 5` next to it. **The ranking is identical under either reading**, so this
defect changes no selection decision. Main independently reached the same conclusion and directed
all judges to use the 1.0–5.0 scale; **this report already does**, so its totals are directly
comparable with the other judges' and the aggregate vote is meaningful. The spec should still drop
the `/ 5` before the next round.

### 1.4 Gates applied

Per the specification's own `scoring` block, which governs:

- any `essential` NO → **total capped at 2.0**;
- any `pitfall` YES → the **affected dimension capped at 2**, with the defect stated;
- no cap is defined for `important` items, and I do not substitute one.
- The floor of 1.0 is inherent in the 1–5 scale.

`gate_source: "specification scoring block (essential_fail_cap 2.0; pitfall_handling dimension cap 2)"`

---

## 2. Requester-specific vocabulary — the complete inventory

**CK-002 is YES for all 18.** No proposed core type, field, endpoint, or panel in any of the 18
approaches speaks `session`, `agent`, `tool call`, `goal`, `omp`, or a local filesystem path. Every
run record is `Run`/`RunRecord`/`Commit` with `transform_id`, `input_revision`/`output_revision`,
`trigger`, `delta`, `status`. Every endpoint is `/runs`, `/runs/:id/diff`, `/node/:id`, `/graph/*`.
The requester's vocabulary appears **only in set-level framing and in the overfitting-check prose**,
in eleven places:

| location | quoted text | surface it sits on |
|---|---|---|
| a.md:39 | "Requester's package: `omp-auto-loop.toml` (pest, N\|/E\| lines, `content` capture → body)" | §1 problem decomposition, set-level |
| a.md:159 | "which is exactly what the requester's \"dream consolidation\" wants" | A2 body, prose |
| a.md:297-298 | "the requester's \"periodic dream consolidation\" wants one-click cadence from the UI" | A6 body, prose |
| a.md:318 | "**Overfitting.** The requester's omp loop is a pest source that appends lines" | §4, set-level |
| b.md:44 | "Requester's loop (pest package + \"dream\" consolidation transform) is one *configuration* of the feature" | success criteria, set-level |
| **b.md:84** | **"a \"dream\" transform can commit to `transform/dream` while `main` tracks the raw import"** | **B2 body — the single nearest-to-core instance: a proposed branch name** |
| b.md:122 | "the requester's loop already emits `session`/`event` nodes, so the two vocabularies must be visibly distinct" | B5 body, risk line — a *warning about* collision, not a collision |
| b.md:148 | "a transform package with `input = \"graph\"` bound to the `ocasazza.omp-auto-loop` source with `schedule.every_ms`" | §4 overfitting guard, set-level |
| c.md:30 | "Must serve an Obsidian vault user, a Kubernetes-cluster user, an httpjson API user, and a synthetic-generator user as well as the agent-loop requester" | constraint 5, set-level |
| c.md:64 | "generality vs. specificity (graph-as-input vs. agent-log viewer)" | trade-off axis, set-level |
| c.md:94 | "a \"dream\" consolidation that misfires is one revert away" | C2 body, prose |
| c.md:151 | "The requester's \"dream\" consolidation is one transform package (`{ graph, window }:` that folds event nodes into labelled summary nodes)…The pest package continues to be the only place the omp projection is named" | §4, set-level |

**Explicitly not requester vocabulary:** `session-manager` / `session_manager` at b.md:80 and c.md:86
(and c.md:77, c.md:91). `crates/session-manager` is a real repository crate — *"Abstract
world/session interface (WorldHost), the embedded single-user implementation, the Kubernetes
multi-user server, and the HTTP client"* (`crates/session-manager/Cargo.toml:4`) — and B2/C2 name it
as the host where world reconcile loops already run. Flagging it as overfitting would be a false
positive.

The one instance worth an editor's attention is **b.md:84's `transform/dream` branch name**. It is
the requester's word landing in a proposed design artifact. It does not trip CK-002 — a git branch
name is not a type, field, endpoint, or panel, and it is clearly the requester's illustrative
example — but it is the item to fix if this feature is ever taken to a PR.

---

## 3. Corpus-wide findings (independent of the ranking)

1. **Not one of the 18 states miss/overlap/coalesce semantics.** `grep -i "coalesc|overlap|missed|skipp"`
   across all three files returns zero scheduling hits. The `score_4` anchor for D5 demands *"missed
   runs coalesce to one; no overlap"*. **D5 is capped at 3 for all 18.** This is the largest single
   unclaimed improvement in the corpus and it is worth 13.4% of the score.
2. **Not one of the 18 ships a phase list.** No P1…P5-style slicing anywhere. **D7 ≤ 3 for all 18.**
   The closest statements are a.md:337-338 ("A3 is the cheapest first slice and composes with A1 or
   A2"), b.md:97 ("Zero server changes in the first increment; server-side execution is a later
   `POST /transform` mirror of `/generate`"), c.md:152 (the Phase-2 synthesis).
3. **CK-018 is a NO for all 18.** All three sets independently and correctly identify
   `panels/timeline.rs` as a layout scrubber distinct from run history — a.md:35-36 ("it scrubs a
   `FrameRing` of *layout positions*, not graph states"), b.md:24, c.md:15 ("it is a layout scrubber,
   not a graph-state timeline"). B3 and C3 both *extend* the ring with a second variant rather than
   conflating the two. This is the one pitfall nobody fell into.
4. **CK-014 is a YES for all 18.** Every set keeps the graph out of Nix's reach as a host-injected
   binding (A1: `let graph = builtins.fromJSON ''…''`, the `eval_seed` idiom) or an in-memory VFS
   file (B1: `/jc/input/graph.nix` via `GraphNixIO`). No proposal reads a filesystem or network at
   eval time.
5. **CK-016 is a YES for all 18.** a.md:3 "Propose only"; b.md:4 "Research only … nothing here is
   implemented"; c.md:4 "Design options only. Nothing here is implemented."
6. **CK-013 is a NO for all of set A and all of set C.** Neither file contains the string
   `knowledge` at all. Only set B does, and only document-level: b.md:148 ("a knowledge note beside
   `OMP Auto-Loop Topos.md`") and its Grounding entry b.md:158. This is a cheap repo convention and
   two thirds of the corpus misses it.
7. **14 of 18 trip the essential cap.** Four survive with no cap at all: **B1 (3.800), A1 (3.733),
   C1 (3.466), A3 (3.366)**. The 2.0 ceiling flattens the other fourteen into an exact tie, so the
   ranking among them is decided entirely by the specification's tie-break chain (D1 → D3 → D6),
   which I apply below.

### 3.1 Which checklist items do the damage

| checklist item | importance | NO count | why |
|---|---|---|---|
| **CK-009** cap breach → failed run | essential | **12 / 18** | The runaway-eval landmine is named by the spec as *"the default failure mode, not an edge case"*. Only A1, A3, A6, B1, C1, C2 name the caps **and** a typed failure record. |
| **CK-008** bounded per-run state | essential | **8 / 18** | A2, A4, A6, B2, B4, B5, B6, C2, C3, C4, C5, C6 — most of them because they record a whole-graph `Snapshot` per run in an unbounded log. |
| **CK-006** panel-kit loading stores | important | 6 / 18 | Set A's constraint block never mentions loading stores. |
| **CK-013** knowledge map | important | 12 / 18 | Sets A and C. |
| **CK-015** failed/empty run UI | important | 11 / 18 | Only A1, A3, B1, C1, C2, C4 carry a typed outcome. |
| **CK-010** closure bug workaround | important | 1 / 18 | A5 names the bug and then builds the risky pattern (a Y-built thunk list across imports) with no workaround. |
| **CK-012** non-omp worked example | important | 1 / 18 | A4. |

**The strongest disqualification is CK-009.** It is the only essential item that fails on a
proposal that is otherwise excellent on the package/engine axis, and it fails on the failure mode
the specification calls the default rather than the edge case. B1 is the only proposal that answers
it completely — b.md:76, *"needs a per-run time budget and `JC_MAX_*` caps applied to input as well
as output"* — and it is the only one that pairs that with a `status` field on the run record. C1 and
C2 answer it at one remove (a typed `outcome` enum and *"A failed run commits nothing and is recorded
as a branch-scoped event in `/progress`"*, c.md:86) but never name the caps in the approach body.

---

## 4. Per-proposal evaluation

Deciding quotes are verbatim. `—` in the quote column means the required thing is absent from the
proposal; the reason follows.

---

### A1 — Transform packages + run journal in graph-api (p=0.85, med)

| CK | imp | Y/N | deciding quote |
|---|---|---|---|
| 001 | ess | **Y** | "The package shape stays format_version=3 and `engine = \"tvix\"`; the only new field is `[parser] role = \"transform\"` (default `\"generate\"`). Validation is the same `validate_config`" / "The Rust side is a `crates/importer` type, not a crate" |
| 002 | ess | Y | "RunRecord { id, source_id, transform_id, trigger: Watch\|Poll\|Manual, started_ms, finished_ms, revision_before, revision_after, added/removed/changed node & edge counts, error }" — no agent vocabulary in the core |
| 003 | ess | Y | "Rust everywhere (Dioxus/wgpu, no JS)" (constraint block); no JS appears anywhere in the file |
| 004 | ess | Y | "`prefers-reduced-motion` swaps the pulse for a static highlight" (also a.md:45) |
| 005 | imp | Y | "a new Runs panel (past strip · now marker · upcoming strip, honest countdown from `due_ms`)" — no fabricated number |
| 006 | imp | **N** | — set A's constraint block names honest progress and reduced-motion but never `loading_store` / `LoadingGate`; the new Runs panel's hydration is unspecified |
| 007 | ess | Y | "graph-api applies it as a pipeline stage after the source import, on the source's existing `WatchPlan` cadence"; new endpoints are `/runs`, `/runs/:id/diff` — no existing endpoint serves run history |
| 008 | ess | Y | "into a ring of N (default 256) alongside the last K (default 8) full snapshots for scrubbing" |
| 009 | ess | Y | "mitigate with the caps in `lib.rs` and a per-package `[limits] input_nodes`" + `RunRecord { … error }` |
| 010 | imp | Y | "the closure bug may bite `graph-transform.nix` if it imports `graph.nix` — keep the `{ graph }` parameter convention" |
| 011 | imp | Y | "crates/importer/src/tvix.rs", "graph_canvas::load", "nodes.rs", "`validate_config`" |
| 012 | imp | Y | "A Kubernetes operator gets a `collapse-by-namespace` transform"; "An Obsidian user gets … a periodic \"cluster and label my notes\" transform" |
| 013 | imp | **N** | — the string `knowledge` does not occur anywhere in set A |
| 014 | ess | Y | "injects the current snapshot … as a top-level `let graph = builtins.fromJSON ''…''` binding — the same host-injection idiom `eval_seed` uses" |
| 015 | imp | Y | `RunRecord { … error }` — a failed run is recorded with a reason |
| 016 | ess | Y | "Phase 1 of Tree-of-Thoughts. Propose only." |
| 017 | opt | Y | "probability 0.85" |
| 018 | pit | N | recognises the distinction: "it scrubs a `FrameRing` of *layout positions*, not graph states" |
| 019 | pit | N | "on the source's existing `WatchPlan` cadence"; "schedule derived from existing `WatchPlan`" |
| 020 | pit | N | "**Key decisions.** Server-side eval" |
| 021 | pit | N | run journal is a separate in-memory ring, not the `ArcSwap<GraphSnapshot>` |
| 022 | pit | N | output lands as a recorded revision (`revision_before`/`revision_after`) through the atomic `rebuild_snapshot` swap |

**Dimensions**

| dim | score | placement | evidence for the closer pole | gap vs the other pole |
|---|---|---|---|---|
| D1 | **3** | past score_2, short of score_4; lean→score_4 | "RunRecord { id, source_id, transform_id, trigger, started_ms, finished_ms, revision_before, revision_after, added/removed/changed node & edge counts, error }" vs score_2's `pub session: String … agent_tool: String` | anchor requires `pub bound: BTreeMap<String, Value>` — A1's record has no bound-variable field, and "The Rust side is a `crates/importer` type, not a crate" means no `Transform` extension point for a third party |
| D2 | **5** | better than score_4 on this axis | "The package shape stays format_version=3 and `engine = \"tvix\"` … `render_expression` renders `( <expr> ) { graph = <injected>; name = <literal>; … }`" | anchor has a bare scalar `[[parser.variables]]`; A1 shows the graph itself arriving as a bound value in the existing render path, with `default \"generate\"` keeping every `generate-*.toml` valid — the same-axis respect |
| D3 | **4** | matches score_4 | "a ring of N (default 256) alongside the last K (default 8) full snapshots" + "`/runs/:id/diff` returns two LE `u32` index buffers (added, removed) against `/graph/ids` of `revision_after`" | vs score_2's "full { nodes, links } JSON per run, unbounded retention" — A1 is bounded *and* differential |
| D4 | **4** | past score_2, short of score_4; lean→score_4 | "added nodes pulse via `update_colors` + `update_sizes`, removed nodes dim via `set_selected`; a differing node set remounts through `graph_canvas::load` against `/graph/*?revision=`" | anchor's third clause — "the revision guard already in `graph_canvas::load` is the admission check for every frame" — is never stated; A1 uses the guarded path only for remounts |
| D5 | **3** | past score_2, short of score_4 | "`upcoming: [{ due_ms, transform_id }]` derived from the `Poll` interval / filesystem rescan interval" | anchor: "missed runs coalesce to one; no overlap" — no miss/overlap semantics anywhere in set A |
| D6 | **5** | better than score_4 on this axis | "`convert_generated_graph` projects Nix `metadata` into `frontmatter` (today discarded) and allows `[schema.fields]` on tvix packages"; "`NodeMeta` proto gains `provenance {…}` and `facts: [{key, value, field_type}]` — the schema-declared fields of *whatever* importer"; "Obsidian, Kubernetes, and pest nodes all get richer with zero source-specific code" | the anchor only adds fields; A1 also removes the *cause* of thin nodes for tvix sources, which is the same-axis respect |
| D7 | **2** | matches score_2 | one design, no phases | vs a.md:337-338 "A3 is the cheapest first slice" — that slicing is about *other* approaches, not A1's own delivery; no increment list |
| D8 | **4** | past score_2, short of score_4; lean→score_4 | "mitigate with the caps in `lib.rs` and a per-package `[limits] input_nodes`" (anchor landmine 1) and "the closure bug may bite … keep the `{ graph }` parameter convention" (anchor landmine 3) | anchor landmines 2 (`foldl'` O(n²)) and 4 (non-atomic `/graph/*` fetches) are absent — and A1's own `foldl'`-on-`attrValues` combinators walk into trap 2 unremarked |

**Raw 3.733. Every essential item is YES (CK-001, CK-002, CK-003, CK-004, CK-007, CK-008, CK-009,
CK-014, CK-016) — no cap, and no pitfall YES. FINAL 3.733 — rank 2.**

---

### A2 — Runs as graph-vcs commits; timeline is the branch log (p=0.82, med-high)

| CK | imp | Y/N | deciding quote |
|---|---|---|---|
| 001 | ess | Y | "Same transform package and Nix library as A1" |
| 002 | ess | Y | run = a `Commit`; no agent vocabulary in the core |
| 003 | ess | Y | set constraint: "Rust everywhere (Dioxus/wgpu, no JS)" |
| 004 | ess | Y | set constraint: "reduced-motion variants" |
| 005 | imp | Y | "A `Scheduled` progress event is emitted when a run is queued so the Progress panel shows it" — no fabricated number |
| 006 | imp | **N** | — set A never names `loading_store` / `LoadingGate` |
| 007 | ess | Y | "`GET /runs` maps to `log(branch, n)`; `GET /runs/:commit/graph` materialises a `Snapshot` and serves it through the same `/graph/*` buffers with `x-graph-revision` set to a commit-derived id, so the canvas needs no new wire" — explicit reuse reasoning |
| 008 | ess | **N** | "a commit per run means a `Snapshot` clone per run" — whole-graph state per run, and no retention window is stated; the only bound in the approach is "bounded to last 32" for the per-node history list |
| 009 | ess | **N** | — no cap handling and no failure semantics; the commit message is `transform:<id> trigger:<poll\|watch\|manual> vars:<…>` with no status field |
| 010 | imp | Y | "Same transform package and Nix library as A1" (inherits the `{ graph }` convention) |
| 011 | imp | Y | "`VAULT_ROOT/.jump-cannon/runs/<source>.graph`", "`GET /runs/:commit/graph`" |
| 012 | imp | Y | "An httpjson tenant (Hindsight memory bank) gets scheduled consolidation with recorded conflicts (A2) and provenance on every memory node" |
| 013 | imp | **N** | — no `knowledge` mention in set A |
| 014 | ess | Y | inherits A1's `builtins.fromJSON` injection |
| 015 | imp | **N** | — nothing states what the UI shows for a failed or empty run; "Conflicts between a scheduled transform and a live import are a real semantic" is the only failure discussion |
| 016 | ess | Y | "Propose only" |
| 017 | opt | Y | "probability 0.82" |
| 018 | pit | N | it distinguishes the History panel from the Runs panel rather than merging them |
| 019 | pit | N | "From `WatchPlan` as in A1" |
| 020 | pit | N | server-side (A1's key decision) |
| 021 | pit | N | history is a `MinigrafStore`, not the snapshot |
| 022 | pit | N | output is a distinct addressable commit on `runs/<source_id>` |

**Dimensions**

| dim | score | placement | closer evidence | gap |
|---|---|---|---|---|
| D1 | **3** | past score_2, short of score_4 | "commits with message `transform:<id> trigger:<poll\|watch\|manual> vars:<…>`" — records the bound variables, which the anchor's `pub bound` requires | no `status`/outcome field; the transform is A1's, so no `Transform` extension point |
| D2 | **4** | matches score_4 | "Same transform package and Nix library as A1" | inherits rather than restates; adds no same-axis respect |
| D3 | **2** | matches score_2 | "a `Snapshot` clone per run" | exactly score_2's failure mode, and unbounded: no retention window, no keyframe interval, no cap |
| D4 | **4** | past score_2, short of score_4 | "serves it through the same `/graph/*` buffers with `x-graph-revision` set to a commit-derived id, so the canvas needs no new wire" | no per-frame patch rule and no revision-guard admission statement |
| D5 | **3** | past score_2, short of score_4; lean→score_4 | "a per-source `schedule.local.json` overlay (`interval_ms` or a fixed `at: [ISO…]` list) so an operator can declare future runs without a cron system" | `at: [ISO…]` is a declared serialisable plan, but no coalesce/no-overlap clause |
| D6 | **4** | matches score_4 | "As A1, plus `NodeMeta.history: [{commit_id, op, ts}]` from walking the log for that node id (bounded to last 32)" | additive history, not a same-axis step past the anchor |
| D7 | **3** | past score_2, short of score_4; lean→score_4 | a.md:337: "A2 is A1 with durability and merge semantics at the cost of VCS coupling" — an increment confined to a named separate seam (`graph-vcs`) | no increment list of its own |
| D8 | **2** | past score_2, short of score_4 | "Coupling the User view to VCS internals; log walks for per-node history are O(runs × ops) without an index" | one codebase-specific hazard with no countermeasure; none of the anchor's four landmines |

**Raw 3.099. Essential NO on CK-008 and CK-009 → CAPPED at 2.000.**

---

### A3 — Instrument the existing rebuild; run events on `/progress` (p=0.80, low)

| CK | imp | Y/N | deciding quote |
|---|---|---|---|
| 001 | ess | Y | "The transform itself is A1's package + `eval_transform` stage" |
| 002 | ess | Y | "run_started { run_id, source_id, trigger, transform_id?, due_ms? }" — graph vocabulary only |
| 003 | ess | Y | set constraint |
| 004 | ess | Y | set constraint: "reduced-motion variants" |
| 005 | imp | Y | "the UI shows a real due time and never invents one" — the strongest honest-progress line in the corpus |
| 006 | imp | **N** | — no loading store named; set A's constraint block omits it |
| 007 | ess | Y | "`progress.rs` gains three events" — no new HTTP endpoint at all |
| 008 | ess | Y | "nothing beyond the last-N event window" and "cap and elide beyond 10k indices with an explicit \"truncated\" flag" |
| 009 | ess | Y | "run_finished { run_id, revision, counts, error? }" + inherits A1's caps |
| 010 | imp | Y | inherits A1's `{ graph }` convention |
| 011 | imp | Y | "`progress.rs`", "spawn_poll", "`main.rs`" |
| 012 | imp | Y | "An Obsidian user gets \"runs\" for save bursts (A3 immediately)" |
| 013 | imp | **N** | — no `knowledge` mention |
| 014 | ess | Y | inherits A1's `builtins.fromJSON` injection |
| 015 | imp | Y | "run_finished { … error? }" |
| 016 | ess | Y | "Propose only" |
| 017 | opt | Y | "probability 0.80" |
| 018 | pit | N | "below the layout-frame scrub, a run track of markers" — a second track, not a conflation |
| 019 | pit | N | "`run_scheduled` … emitted by `spawn_poll` / the filesystem rescan timer one tick ahead" — rides the existing tick |
| 020 | pit | N | server-side |
| 021 | pit | N | nothing is stored in the snapshot |
| 022 | pit | N | "an *animation of the delta*, not a checkout of the past state" — nothing is merged into the served graph |

**Dimensions**

| dim | score | placement | closer evidence | gap |
|---|---|---|---|---|
| D1 | **3** | past score_2, short of score_4; lean→score_4 | "run_started { run_id, source_id, trigger, transform_id?, due_ms? }" + "run_finished { run_id, revision, counts, error? }" | no bound-variable field; making *every* rebuild a run is genuinely graph-generic, but it is not what the anchor's field list asks for |
| D2 | **4** | matches score_4 | "The transform itself is A1's package + `eval_transform` stage" | inherited, not restated |
| D3 | **3** | past score_2, short of score_4 | "nothing beyond the last-N event window" + "cap and elide beyond 10k indices with an explicit \"truncated\" flag" | the anchor's "full keyframe every K runs" is structurally absent — "the past state is not retained", so no past run can be reconstructed |
| D4 | **4** | past score_2, short of score_4; lean→score_4 | "replays `run_touched` as a colour pulse (`update_colors`) and a dim of removed nodes (`set_selected`) — an *animation of the delta*" | the purest delta patch of the 18, but the revision-guard admission clause is unstated |
| D5 | **3** | past score_2, short of score_4; lean→score_4 | the cadence is the source's declared `WatchPlan` and `run_scheduled` rides `spawn_poll` | the upcoming value is an event emitted one tick ahead, and no coalesce/no-overlap clause |
| D6 | **4** | matches score_4 | "`NodeMeta.frontmatter` projection fix + schema-driven Facts as in A1" | the per-node history is client-side only, so nothing new is served to every importer |
| D7 | **3** | past score_2, short of score_4; lean→score_4 | a.md:337-338: "A3 is the cheapest first slice and composes with A1 or A2 (same events, richer store)" | the strongest slice language of the 18, but still no increment list |
| D8 | **3** | past score_2, short of score_4 | "`run_touched` for a 100k-node delta bloats the JSON progress log — cap and elide beyond 10k indices with an explicit \"truncated\" flag" | one codebase-specific hazard with a concrete countermeasure; none of the anchor's four landmines |

**Raw 3.366. Every essential item is YES (CK-001, CK-002, CK-003, CK-004, CK-007, CK-008, CK-009,
CK-014, CK-016) — no cap, and no pitfall YES. FINAL 3.366 — rank 4, and the first reserve.**

---

### 3.2 Essential-item audit (the gate that decides 14 of the 18)

The nine `essential` items are CK-001, CK-002, CK-003, CK-004, CK-007, CK-008, CK-009, CK-014,
CK-016. Resolving them against each proposal's own checklist table above gives exactly four
survivors:

| proposal | essential failures | survives the 2.0 cap? |
|---|---|---|
| **B1** | none | **yes — 3.800** |
| **A1** | none | **yes — 3.733** |
| **C1** | none | **yes — 3.466** |
| **A3** | none | **yes — 3.366** |
| B2 | CK-008, CK-009 | capped 2.000 (raw 3.066) |
| A2 | CK-008, CK-009 | capped 2.000 |
| A6 | CK-008 | capped 2.000 |
| B5 | CK-008, CK-009 | capped 2.000 |
| C4 | CK-008, CK-009 | capped 2.000 |
| B4 | CK-007, CK-008, CK-009 | capped 2.000 |
| C3 | CK-009 | capped 2.000 |
| B3 | CK-009 | capped 2.000 |
| A4 | CK-008, CK-009 | capped 2.000 |
| C5 | CK-008, CK-009 | capped 2.000 |
| **A5** | **CK-001**, CK-009 | **disqualified** |
| **B6** | **CK-001**, CK-008, CK-009 | **disqualified** |
| C6 | CK-008, CK-009 | capped 2.000 |
| C2 | CK-008 | capped 2.000 |

**CK-002, CK-003, CK-014 and CK-016 are YES for all 18** — no proposal puts agent vocabulary in the
shared core, no proposal reaches for JavaScript, no proposal lets the Nix expression touch a
filesystem or the network, and no proposal presents itself as an implementation. Those four
essential gates are cleared by the entire corpus, which is the strongest single thing that can be
said about it. The gate that does the damage is **CK-008** (13 NOs) and **CK-009** (12 NOs), both of
which are about what a *failing or unbounded* run does — the part of the design nobody in this
exercise wrote down.

Diversity note for the selected trio (B1, A1, C1): all three land their first increment on the
**same seam** — the tvix package gaining a graph input, applied as a stage of `rebuild_snapshot`.
That collision is recorded in full, with the run-state divergence that satisfies the diversity
requirement, in §5.4.

---

### A4 — Time inside the graph: bitemporal nodes and reified runs (p=0.08, med)

| CK | imp | Y/N | deciding quote |
|---|---|---|---|
| 001 | ess | Y | set constraint: "Reuse the tvix package contract rather than a parallel one"; A4 adds no new Nix module |
| 002 | ess | Y | `run:<id>` / `transform:<id>` nodes are graph vocabulary |
| 003 | ess | Y | set constraint |
| 004 | ess | Y | set constraint: "reduced-motion variants" |
| 005 | imp | Y | no fabricated number appears |
| 006 | imp | **N** | — set A never names a loading store |
| 007 | ess | Y | "evaluated by the existing `/search/matches` set primitive and applied through `set_selected` with no remount, ever" — adds no endpoint |
| 008 | ess | **N** | "the graph grows monotonically (retention = a compaction transform, which is itself a run)" — unbounded, and the compaction is a *future* transform, not a policy |
| 009 | ess | **N** | — no cap handling |
| 010 | imp | Y | set constraint "Respect `{ graph }` param passing"; A4 adds no combinator |
| 011 | imp | Y | "the metric pass learns to exclude a `provenance` tag"; "the existing `/search/matches` set primitive" |
| 012 | imp | **N** | — A4's body carries no non-requester worked example (a.md:320-327 assigns those to A1/A2/A3/A5/A6 only) |
| 013 | imp | **N** | — no `knowledge` mention |
| 014 | ess | Y | the transform consumes the host's current graph |
| 015 | imp | **N** | — nothing about a failed or empty run |
| 016 | ess | Y | "Propose only" |
| 017 | opt | Y | "probability 0.08" |
| 018 | pit | N | no conflation |
| 019 | pit | N | no new runtime is introduced; the schedule origin is simply unchosen |
| 020 | pit | N | server-side |
| 021 | pit | **Y** | **defect** — "A run does not replace the graph; it *appends* to one graph in which every node and edge carries `valid_from`/`valid_to` run indices" — per-run history is held inside the one live snapshot |
| 022 | pit | **Y** | **defect** — "The transform stage emits `{ nodes, links }`" and the result is merged in place; no distinct addressable artifact |

**Dimensions**

| dim | score | placement | closer evidence | gap |
|---|---|---|---|---|
| D1 | **2** | past score_2, short of score_4 | "the Rust stage adds `run:<id>` and `transform:<id>` nodes with `produced` edges" — a run expressed purely in graph terms | no `id`/`transform`/`bound`/`input_revision`/`output_revision`/`status`/`delta` record; a bitemporal index, not the anchor's struct |
| D2 | **4** | matches score_4 | adds nothing to the envelope; inherits the tvix contract | restated only by the set constraint, not by A4 |
| D3 | **2** | **capped at 2 by CK-021 + CK-022** | — | "retention = a compaction transform, which is itself a run" is an admission, not a bound; all history is inside the single snapshot |
| D4 | **4** | past score_2, short of score_4; lean→score_4 | "The timeline is then a **filter** — `valid_from <= t < valid_to` — applied through `set_selected` with no remount, ever" | zero per-frame traffic beats the anchor's patch rule, but A4 replaces the served wire (`/graph/validity`, `?t=`) rather than patching under the existing revision guard |
| D5 | **2** | matches score_2 | "Upcoming runs are `run` nodes with `status = scheduled` emitted ahead of time by the scheduler stage" | a value derived from the current time; §2's "Schedule origin" row is left unchosen and there is no miss/overlap clause |
| D6 | **3** | past score_2, short of score_4 | "clicking a node shows its `valid_from`/`valid_to` facts, and its neighbours *include* the run and transform that produced it" | improves every source, but through neighbour traversal and bitemporal intervals rather than new shared `NodeMeta` fields served to every importer |
| D7 | **1** | worse than score_2 | "It changes what \"the graph\" means for every consumer (metrics, layout, search) and needs a compaction story before it is safe on long-running sources" | worse than a single big phase: the prerequisite (compaction) does not exist and the change rewrites a contract every consumer reads |
| D8 | **3** | past score_2, short of score_4 | "metrics (PageRank, Louvain) are polluted by provenance nodes unless the metric pass learns to exclude a `provenance` tag, and layout shows dead nodes unless filtered" | two codebase-accurate hazards with partial counters; none of the anchor's four landmines |

**Raw 2.633. Essential NO on CK-008 and CK-009 → CAPPED at 2.000.**

---

### A5 — Lazy stream evaluator; the timeline is an infinite lazy Nix list (p=0.06, high)

| CK | imp | Y/N | deciding quote |
|---|---|---|---|
| 001 | ess | **N** | "A transform package's `expr` returns not a graph but a lazy **stream** of graphs — `unfold step g0`"; "Rust gains `tvix_wasm::eval_stream(expr, k, fuel)`" — a new engine entry point and a bespoke return type, not the existing `{expr, variables}` → graph contract |
| 002 | ess | Y | "A \"run\" is `(stream_id, k, wall_clock)`" — no agent vocabulary |
| 003 | ess | Y | set constraint; no JS |
| 004 | ess | Y | set constraint: "reduced-motion variants" |
| 005 | imp | Y | "scrubbing forward past `now` is allowed and clearly labelled \"predicted\"" — honest labelling |
| 006 | imp | **N** | — no loading store named |
| 007 | ess | Y | adds no HTTP endpoint |
| 008 | ess | Y | "states 0..k are memoised server-side in a bounded LRU" — a stated bound, though unnamed in size |
| 009 | ess | **N** | "tvix has no fuel/step limit (`lib.rs` caps only output), so a divergent `step` **hangs the blocking thread**" — A5 *specifies the hang*; no failed-run record |
| 010 | imp | **N** | "the closure bug is likelier with Y-built thunks across imports" names the bug, but `graph-transform.nix` provides `unfold`, a Y-built lazy cons list — the risky pattern — with no `{ graph }`-parameter workaround stated |
| 011 | imp | Y | "`lib.rs` caps only output", "`graph-transform.nix`" |
| 012 | imp | Y | "A generator user (`generate-clusters.toml`) gets A5's predicted futures for free because generators are pure" |
| 013 | imp | **N** | — no `knowledge` mention |
| 014 | ess | Y | "Impure inputs (a vault reload) enter as a *second* stream zipped with the transform" — host-supplied, no Nix I/O |
| 015 | imp | **N** | — nothing about a failed or empty run |
| 016 | ess | Y | "Propose only" |
| 017 | opt | Y | "probability 0.06" |
| 018 | pit | N | no conflation |
| 019 | pit | **Y** | **defect** — "the scheduler is a clock that increments *k* on cadence" — a new wall-clock driver unmoored from `WatchPlan` |
| 020 | pit | N | `eval_stream` is server-side |
| 021 | pit | N | states live in a separate LRU |
| 022 | pit | N | a run is `(stream_id, k, wall_clock)`, not a merge into the served graph |

**Dimensions**

| dim | score | placement | closer evidence | gap |
|---|---|---|---|---|
| D1 | **2** | past score_2, short of score_4 | "A \"run\" is `(stream_id, k, wall_clock)`" | three fields, none of the anchor's seven; no transform identity, no revisions, no status, no delta |
| D2 | **3** | past score_2, short of score_4 | stays a `format_version = 3` tvix package with typed variables | `eval_stream(expr, k, fuel)` is a new return type the engine does not produce — the anchor's expr returns a graph |
| D3 | **3** | past score_2, short of score_4; lean→score_2 | "memoised server-side in a bounded LRU" converts score_2's unboundedness into a bound | but "every state is a full graph, so memory is states × graph" — no delta, no keyframe; the primary half of the axis fails |
| D4 | **2** | matches score_2 by silence | — | A5 states nothing about how pixels move; the client is never discussed |
| D5 | **2** | **capped at 2 by CK-019** | "scrubbing forward past `now` … labelled \"predicted\"" — the value is computed from the current time | no declared plan, no coalesce clause, and the driver is a new clock |
| D6 | **2** | by silence | — | A5 proposes no node-content change at all; neither the anchor's `NodeMeta` fields nor a source-conditional panel |
| D7 | **1** | worse than score_2 | "Needs an evaluator feature (fuel / cancellation) tvix does not expose at the pinned rev" | an unbuilt prerequisite is the whole design; worse than one long phase |
| D8 | **3** | past score_2, short of score_4 | "tvix has no fuel/step limit (`lib.rs` caps only output), so a divergent `step` hangs the blocking thread"; "the closure bug is likelier with Y-built thunks across imports" | two anchor landmines, accurately cited, but with no in-repo countermeasure — it names the missing feature instead |

**Raw 2.267. Essential NO on CK-001 and CK-009 → DISQUALIFIED, CAPPED at 2.000.**

---

### A6 — No scheduler: external timers + `Push`, runs applied by a CLI (p=0.07, low-med)

| CK | imp | Y/N | deciding quote |
|---|---|---|---|
| 001 | ess | Y | "The transform contract, Nix library, `eval_transform`, and the node-content work are exactly A1's" |
| 002 | ess | Y | run = a `runs.jsonl` line; no agent vocabulary |
| 003 | ess | Y | set constraint |
| 004 | ess | Y | set constraint |
| 005 | imp | Y | "derives `upcoming` from … `.status.nextScheduleTime` … **never from a guess**" |
| 006 | imp | **N** | — no loading store named |
| 007 | ess | Y | "best as a *later* adapter on top of A1's `/runs` contract"; `GET /runs` derived from the real scheduler |
| 008 | ess | **N** | "Past runs are the CLI's append to `runs.jsonl` next to the overlay files" — append-only, no retention window, no delta, no keyframe |
| 009 | ess | Y | inherits A1's `eval_transform` and its caps, and reuses A1's error-carrying `/runs` contract |
| 010 | imp | Y | "exactly A1's" |
| 011 | imp | Y | "a `graph-api run <package> --var k=v` subcommand"; "`runs.jsonl`" |
| 012 | imp | Y | "`scheduler: { kind: systemd-timer \| cron \| k8s-cronjob, ref }`" — a Kubernetes CronJob deployment, no omp |
| 013 | imp | **N** | — no `knowledge` mention |
| 014 | ess | Y | inherits A1's injection |
| 015 | imp | **N** | — no failure or empty-run behaviour |
| 016 | ess | Y | "Propose only" |
| 017 | opt | Y | "probability 0.07" |
| 018 | pit | N | animation is "as A1" |
| 019 | pit | N | "jump-cannon deliberately does not grow a scheduler" — cadence is external by design |
| 020 | pit | N | the CLI pushes through the existing trigger |
| 021 | pit | N | a jsonl file, not the snapshot |
| 022 | pit | N | the pushed result enters through the normal `WatchPlan::Push` rebuild |

**Dimensions**

| dim | score | placement | closer evidence | gap |
|---|---|---|---|---|
| D1 | **2** | past score_2, short of score_4 | "Past runs are the CLI's append to `runs.jsonl`" | no run-record schema at all; A6 is "exactly A1's" for everything *except* its store, which is the one place a run's fields would live |
| D2 | **4** | matches score_4 | "exactly A1's" | inherited |
| D3 | **2** | matches score_2 | "the CLI's append to `runs.jsonl`" | append-only, unbounded, no delta, no keyframe — score_2's failure with a different filename |
| D4 | **4** | past score_2, short of score_4 | "Animation … as A1" — inherits the `update_colors`/`set_selected`/remount path | inherited; the revision-guard clause still unstated |
| D5 | **3** | past score_2, short of score_4; lean→score_4 | "`scheduler: { kind: systemd-timer \| cron \| k8s-cronjob, ref }` … never from a guess" | a genuinely declared, serialisable plan whose `upcoming` is the real scheduler's answer — but no coalesce/no-overlap clause, and the plan is not reachable from the UI |
| D6 | **4** | matches score_4 | "the node-content work are exactly A1's" | inherited |
| D7 | **3** | past score_2, short of score_4; lean→score_4 | "best as a *later* adapter on top of A1's `/runs` contract, not the first design" | an explicit deferral onto an existing seam — real slicing, but not an increment list |
| D8 | **2** | past score_2, short of score_4 | "three scheduler adapters is a maintenance tax, dev on macOS has no systemd" | an ops concern, not a repository landmine; no countermeasure pair |

**Raw 2.999. Essential NO on CK-008 → CAPPED at 2.000.**

---

### B1 — Transform packages + run ledger in graph-api (p=0.90, medium)

| CK | imp | Y/N | deciding quote |
|---|---|---|---|
| 001 | ess | Y | "The package gains `[parser] input = \"graph\"` (default `\"none\"`, preserving `generate-*.toml`)"; "**Packages, not crates** … No `SourceKind` variant per transformation" |
| 002 | ess | Y | "RunRecord { run_id, transform_id, variables, trigger, scheduled_at, started_at, finished_at, status, input_revision, output_revision, delta }" — pure graph vocabulary |
| 003 | ess | Y | "**Rust everywhere**; Dioxus panels in `app/ui/src/panels/`" |
| 004 | ess | Y | "`prefers-reduced-motion` snaps" (also constraint 2) |
| 005 | imp | Y | "a **Runs panel** (past list + upcoming list + trigger button, honest `m:ss` until next)" |
| 006 | imp | Y | constraint 2: "panel-kit loading stores" |
| 007 | ess | Y | "Endpoints: `GET /runs?since=`, `GET /runs/upcoming`, `GET /runs/:id/delta` … `POST /runs/:transform_id` (manual trigger, **default-source only like `/generate`**)" — the reuse reasoning is stated explicitly |
| 008 | ess | Y | "a `RunLedger` (ring in `AppState`, appended to `runs.jsonl` …)" — "a bounded run ledger" |
| 009 | ess | Y | "`deepSeq` on a 1M-node attrset can stall the blocking thread — needs a per-run time budget and `JC_MAX_*` caps applied to input as well as output" + `status` on the record |
| 010 | imp | Y | constraint 3: "library files take `{ graph }` as a parameter (`graph-combinators.nix:3-12`); a transform package imports `graph.nix` at depth 1 and receives the input graph as *data*, never as closures" |
| 011 | imp | Y | "`crates/importer/src/tvix.rs:354-371,512-566`", "`watcher.rs:476`", "`app/ui/src/main.rs:1198-1204`" |
| 012 | imp | Y | "Kubernetes source: hourly `ContractChains` collapses ReplicaSet→Pod fans; Obsidian: `LabelCommunities` writes a `community-label` tag from Louvain; OpenAlex: periodic citation-cluster relabelling" |
| 013 | imp | Y | b.md:148: "a knowledge note beside `OMP Auto-Loop Topos.md`"; b.md:158 lists `charts/jump-cannon/knowledge/OMP Auto-Loop Topos.md` |
| 014 | ess | Y | "`GraphNixIO` serves `/jc/input/graph.nix` from memory"; the expression receives the graph "as *data*, never as closures" |
| 015 | imp | Y | `RunRecord { … status … }` on a bounded ledger the Runs panel lists |
| 016 | ess | Y | "**Status:** Research only (Tree-of-Thoughts phase 1); nothing here is implemented" |
| 017 | opt | Y | "probability 0.90" |
| 018 | pit | N | constraint 6 lists "the Timeline panel's `FrameRing` scrub UI" among "existing seams to reuse", and B1 adds a distinct Runs panel |
| 019 | pit | N | "the existing poll driver pattern (`spawn_poll`) runs a ticker per transform" — reuse, not a new runtime |
| 020 | pit | N | constraint 4: "`eval_graph` is synchronous, runs on `spawn_blocking` server-side" |
| 021 | pit | N | the ledger is a ring in `AppState`, separate from the `ArcSwap<GraphSnapshot>` |
| 022 | pit | N | "`TransformOutput { graph, documents, delta }`" is a distinct value, and `revision_before`/`revision_after` make the run addressable |

**Dimensions**

| dim | score | placement | closer evidence | gap |
|---|---|---|---|---|
| D1 | **5** | better than score_4 on this axis | "`trait Transform { fn id(&self) -> &str; fn apply(&self, input: &VaultGraph, ctx: &RunCtx) -> Result<TransformOutput, TransformError>; }`" in a standalone wasm-clean `graph-transform` crate, implemented by `TvixTransform` **and** `LabelCommunities` / `PruneDegree` / `ContractChains`; "RunRecord { run_id, transform_id, variables, trigger, scheduled_at, started_at, finished_at, status, input_revision, output_revision, delta }" | the anchor only says a third party *may* implement `Transform`; B1 ships the extension point in a crate independent of the server with two implementation families, so a run's transform need not even be Nix — that is the same-axis respect |
| D2 | **5** | better than score_4 on this axis | "`[parser] input = \"graph\"` (default `\"none\"`, preserving `generate-*.toml`)"; "`graph.nix` grows `mapNodes`, `filterNodes`, `foldNodes`, `mapEdges`, `filterEdges`, `partitionBy`, `relabel` … plus `delta g1 g2`" | the anchor shows a scalar bound variable; B1 adds a *declared, defaulted* input channel plus the functional library, entirely inside the existing `[parser]` envelope and with named backward compatibility |
| D3 | **3** | past score_2, short of score_4; lean→score_4 | "a `RunLedger` (ring in `AppState`, appended to `runs.jsonl` …)"; "Rust computes the delta with `graph_vcs::diff_snapshots`" | the anchor's "full keyframe every K runs; ring retention of N runs" — the ring is there but neither K nor N is named, and `runs.jsonl` is unbounded |
| D4 | **4** | past score_2, short of score_4; lean→score_4 | "the canvas gains `render::apply_delta` — on revision advance, `reload_graph` carries positions by id from `positions_cpu()` into the new scene so unchanged nodes stay put; added nodes spawn at the centroid of their neighbours and fade in over 400 ms"; "**Risks.** Position carry-over must key on id, not index, or a reorder scrambles the scene" | the most detailed rendering answer of the 18, and it names its own id-vs-index hazard — but the revision-guard admission clause is still unstated |
| D5 | **3** | past score_2, short of score_4 | "`transforms = [{ package, variables, schedule = { every_ms \| on_change \| manual } }]`"; "the existing poll driver pattern (`spawn_poll`) runs a ticker per transform" | two of the anchor's three clauses (a typed serialisable plan, the existing tick loop) are delivered; "missed runs coalesce to one; no overlap" is entirely absent |
| D6 | **5** | better than score_4 on this axis | "`RawNode`/`RawEdge` gain `#[serde(flatten)] extra` so tvix metadata reaches `frontmatter`"; "`/node/:id` gains `edges: [{other, direction, edge_type, metadata}]`, `provenance {source_id, importer_id, importer_version, graph_revision, last_run_id}`, `history: [run_id, op]`"; "the Inspector renders schema-declared fields as a typed table, then leftovers, then edges, then a compact run strip"; constraint 5: "Transform output must re-enter that contract" (one validated `SearchDocument` per node) | the anchor only widens `NodeMeta`; B1 also removes the *cause* of thin nodes and keeps transformed nodes inside the discovery contract, so the gain is complete rather than partial |
| D7 | **2** | matches score_2 | one design, no phase list | B1's own first increment bundles a new crate, a pipeline stage, four endpoints, a new panel, a new render seam, and a `NodeMeta` change — no ordering is offered |
| D8 | **4** | past score_2, short of score_4; lean→score_4 | "`deepSeq` on a 1M-node attrset can stall the blocking thread — needs a per-run time budget and `JC_MAX_*` caps applied to input as well as output" (anchor landmine 1, with a countermeasure that exists in this codebase) and "Position carry-over must key on id, not index" (a real hazard, countermeasured) | anchor landmines 2 (`foldl'` O(n²)) and 4 (non-atomic `/graph/*`) are unmentioned — and B1's own `mapNodes`/`foldNodes` walk on `toNodeList` sit next to that trap |

**Raw 3.800. No essential NO. No pitfall YES. FINAL 3.800 — rank 1.**

---

### B2 — Runs are graph-vcs commits; the History panel is the timeline (p=0.85, med-high)

| CK | imp | Y/N | deciding quote |
|---|---|---|---|
| 001 | ess | Y | "The transform contract and Nix-side library are identical to Approach 1 (`input = \"graph\"`, `graph-transform` crate)" |
| 002 | ess | Y | a run is a `Commit`; no agent vocabulary in the core |
| 003 | ess | Y | constraint 2 |
| 004 | ess | Y | constraint 2 |
| 005 | imp | Y | no fabricated number; the History panel renders existing commit rows |
| 006 | imp | Y | constraint 2: "panel-kit loading stores" |
| 007 | ess | Y | "`panels/history.rs` already renders and polls every 1.5 s"; "Node history is `vcs` op-log filtered by node id (`OpLogEntry`), so `/node/:id/history` is a store query rather than a new index" |
| 008 | ess | **N** | "commit-per-run of a 1M-node snapshot is heavy (**snapshot dedupe by content hash mitigates**)" — content hash is not a bound; no retention window, no keyframe interval, a full snapshot per run |
| 009 | ess | **N** | — no cap handling and no run status; the failure discussion is "Conflicts between a scheduled transform and a live import are a real semantic" |
| 010 | imp | Y | "identical to Approach 1" |
| 011 | imp | Y | "`store.commit(branch = \"transform/<id>\", snapshot, message = run metadata)`"; "`OpLogEntry`" |
| 012 | imp | Y | "Obsidian users get \"what did the nightly labelling change\" as a diffable commit" |
| 013 | imp | Y | b.md:148 / b.md:158 (document-level) |
| 014 | ess | Y | inherits `input = "graph"` via `GraphNixIO` |
| 015 | imp | **N** | — nothing specifies what the UI shows for a failed or empty run |
| 016 | ess | Y | "nothing here is implemented" |
| 017 | opt | Y | "probability 0.85" |
| 018 | pit | N | "History gains a \"runs\" filter" — a filter on a distinct surface, not a conflation with `timeline.rs` |
| 019 | pit | **Y** | **defect** — "the standalone graph-api runs the same scheduler struct **in-process**" with a `TransformSchedule` per world — a new in-process scheduler, not the watch tick loop |
| 020 | pit | N | server-side / world host |
| 021 | pit | N | a `MinigrafStore` keyed by source, not the `ArcSwap<GraphSnapshot>` |
| 022 | pit | N | "a \"dream\" transform can commit to `transform/dream` while `main` tracks the raw import" — the output is a distinct, addressable commit |

**Dimensions**

| dim | score | placement | closer evidence | gap |
|---|---|---|---|---|
| D1 | **4** | matches score_4 | set S2: "A run = (transform id, bound variables, input revision, start/end, outcome, output revision, delta stats)"; "`ChangeId` lets a re-run of the same tuple be recognised as the same change" — and it inherits B1's `Transform` trait | the anchor's `pub status: RunStatus` is not realised on the commit; outcome is carried in free-form commit metadata |
| D2 | **4** | matches score_4 | "identical to Approach 1" | inherited |
| D3 | **2** | matches score_2 | "commit-per-run of a 1M-node snapshot"; "`store.commit(branch = \"transform/<id>\", snapshot, …)`" | a whole-graph snapshot per run, unbounded; content-hash dedupe removes duplicate storage but not the unbounded growth of a high-cadence branch, and there is no keyframe interval |
| D4 | **4** | past score_2, short of score_4; lean→score_4 | "clicking a commit pushes that snapshot to the canvas through the existing `graph_data_from_snapshot` path, with the position carry-over from Approach 1" | inherits B1's carry-over; the per-frame patch rule and the revision guard are unstated |
| D5 | **2** | **capped at 2 by CK-019** | "a `TransformSchedule` per world" is a declared plan | a new in-process scheduler duplicated across two hosts, with no coalesce/no-overlap clause |
| D6 | **4** | matches score_4 | "`/node/:id/history` is a store query rather than a new index"; "per-node history is … filtered by node id (`OpLogEntry`)" | inherits B1's `NodeMeta` widening; the addition is a per-node op log rather than a step past the anchor |
| D7 | **2** | matches score_2 | "the UI needs less new surface" | one design, no phase list; its own trade-off concedes "the standalone graph-api binary today has no VCS store; wiring one in means every deployment grows a minigraf file" — an unlanded prerequisite with no sequencing |
| D8 | **3** | past score_2, short of score_4 | "commit-per-run of a 1M-node snapshot is heavy (snapshot dedupe by content hash mitigates)"; "Two sources of truth if the served graph tracks `main` but runs commit elsewhere; must define that the served snapshot is the head of the configured branch" | three codebase-accurate hazards, one countermeasured; none of the anchor's four landmines |

**Raw 3.066. Essential NO on CK-008 ("commit-per-run of a 1M-node snapshot is heavy (snapshot dedupe
by content hash mitigates)" — a whole graph per run with no retention window) and CK-009 (no cap
handling, no run status) → CAPPED at 2.000. Rank 6.**

---

### B3 — Client-side lazy pipeline over the fetched graph (p=0.80, low-med)

| CK | imp | Y/N | deciding quote |
|---|---|---|---|
| 001 | ess | Y | reuses the same `graph-transform` crate and package model |
| 002 | ess | Y | "{record, delta} in a bounded ring"; no agent vocabulary |
| 003 | ess | Y | constraint 2 |
| 004 | ess | Y | constraint 2 |
| 005 | imp | Y | "honest `m:ss`" is B1's; B3's own UI states the next fire from a declared interval |
| 006 | imp | Y | constraint 2 |
| 007 | ess | Y | "**Zero server changes in the first increment**"; "server-side execution is a later `POST /transform` mirror of `/generate`" |
| 008 | ess | Y | "each run stores `{record, delta}` in a bounded ring" |
| 009 | ess | **N** | "the browser thread limits graph size (the tvix worker with timeout is retired)" names the hazard but no cap record and no failed-run path |
| 010 | imp | Y | constraint 3 |
| 011 | imp | Y | "`GraphData.ids` + `/graph/edges` + `/graph/meta_summary`", "`nodes.rs:745-751`" |
| 012 | imp | Y | "Generate-panel users can iterate `subdivideRule`/`hubRule` rewrites as animated steps; researchers with a static vault can prototype a labelling transform" |
| 013 | imp | Y | b.md:148 / b.md:158 (document-level) |
| 014 | ess | Y | "The input graph is built from `GraphData.ids` + `/graph/edges` + `/graph/meta_summary` (already client-resident) into the Nix attrset form" — no FS or network in the expression |
| 015 | imp | **N** | — no failure or empty-run behaviour |
| 016 | ess | Y | "nothing here is implemented" |
| 017 | opt | Y | "probability 0.80" |
| 018 | pit | N | "The Timeline panel gets a second track: \"graph states\" beside \"positions\"" |
| 019 | pit | **Y** | **defect** — "Runs are scheduled by a `spawn_forever` ticker … with a per-transform interval persisted in localStorage" — a new per-transform timing loop, and "upcoming runs are only as durable as the tab" |
| 020 | pit | **Y** | **defect** — "Run transformations in the browser with the tvix-wasm the app already ships"; "the browser thread limits graph size (the tvix worker with timeout is retired)" |
| 021 | pit | N | a client-side ring, not the snapshot |
| 022 | pit | N | the transform patches the view; the served graph is untouched |

**Dimensions**

| dim | score | placement | closer evidence | gap |
|---|---|---|---|---|
| D1 | **3** | past score_2, short of score_4 | inherits B1's `Transform` trait; a run is `{record, delta}` | no stated run-record fields and no `bound`; per-viewer truth is a conceptual narrowing of the run identity |
| D2 | **4** | matches score_4 | "the same package text works server-side later" (C3 says the same) | inherited; B3 does not restate `input = "graph"` |
| D3 | **3** | past score_2, short of score_4; lean→score_4 | "each run stores `{record, delta}` in a bounded ring" | a bounded delta, but no keyframe interval and "history is not shared between viewers or machines" |
| D4 | **2** | **capped at 2 by CK-020** | "scrubbing applies the delta to the live scene with the same `set_positions` hold semantics" | the patch mechanics are right, but the whole path runs Nix on the browser main thread with the timeout worker retired — the defect the cap records |
| D5 | **2** | **capped at 2 by CK-019** | "a per-transform interval persisted in localStorage" | a per-viewer schedule is not deployment configuration — B3's own risk line says so |
| D6 | **3** | past score_2, short of score_4 | "the client derives edges/history for the focused node from its own buffers and ring" | no shared `NodeMeta` field is added; the improvement is a client-side view, so not every importer gains it from the API |
| D7 | **3** | past score_2, short of score_4; lean→score_4 | "**Zero server changes in the first increment**; … server-side execution is a later `POST /transform` mirror of `/generate`" | an explicit two-increment ordering on two existing seams — the strongest deliverability language among the browser designs, still not a phase list |
| D8 | **2** | past score_2, short of score_4 | "the browser thread limits graph size (the tvix worker with timeout is retired)" — anchor landmine 1, correctly cited | no countermeasure; "history is not shared between viewers or machines" is a product concern, not a repository landmine |

**Raw 2.732. Essential NO on CK-009 → CAPPED at 2.000.**

---

### B4 — Temporal graph: validity intervals, timeline as a render-time filter (p=0.08, high)

| CK | imp | Y/N | deciding quote |
|---|---|---|---|
| 001 | ess | Y | constraint 1 |
| 002 | ess | Y | `[valid_from, valid_to)` intervals are graph vocabulary |
| 003 | ess | Y | constraint 2 |
| 004 | ess | Y | constraint 2 |
| 005 | imp | Y | no fabricated number |
| 006 | imp | Y | constraint 2 |
| 007 | ess | **N** | "graph-api serves a `/graph/validity` u32 buffer" and adds a `?t=` query across the whole surface, with no statement of why `/graph/edges` + `set_search_highlights` cannot carry it |
| 008 | ess | **N** | "unbounded growth needs compaction" — a named need, not a policy |
| 009 | ess | **N** | — no cap handling |
| 010 | imp | Y | constraint 3; B4 reuses the existing rewrite rules |
| 011 | imp | Y | "`VaultNode` gains `intervals: Vec<(u32,u32)>`"; "`docs/importer-architecture.md`" |
| 012 | imp | Y | "Kubernetes and GitHub sources are naturally temporal (pods churn, notes get renamed) — users would see churn as a scrub" |
| 013 | imp | Y | b.md:148 / b.md:158 (document-level) |
| 014 | ess | Y | constraint 1; no FS or network in the expression |
| 015 | imp | **N** | — no failure or empty-run behaviour |
| 016 | ess | Y | "nothing here is implemented" |
| 017 | opt | Y | "probability 0.08" |
| 018 | pit | N | no conflation |
| 019 | pit | N | B4's schedule row is "any" — it introduces no runtime |
| 020 | pit | N | server-side |
| 021 | pit | **Y** | **defect** — "one snapshot holds all history"; "`VaultNode` gains `intervals`" — every run's history lives inside the single `ArcSwap<GraphSnapshot>` |
| 022 | pit | **Y** | **defect** — "A transform's output is merged into the temporal graph" — in-place, no distinct artifact |

**Dimensions**

| dim | score | placement | closer evidence | gap |
|---|---|---|---|---|
| D1 | **2** | past score_2, short of score_4 | "the timeline is a uniform `t` the shader uses for alpha" | a run is a `t` index, not a record; none of the anchor's fields exist |
| D2 | **4** | matches score_4 | constraint 1 | inherited |
| D3 | **2** | **capped at 2 by CK-021 + CK-022** | — | "unbounded growth needs compaction" and every run's intervals live in the one snapshot |
| D4 | **4** | past score_2, short of score_4; lean→score_4 | "the renderer adds a `t` uniform … scrubbing and animation cost nothing and never reload" | zero per-frame traffic beats the anchor's patch rule, but B4 replaces the served wire rather than patching under the existing revision guard |
| D5 | **2** | matches score_2 | the schedule row is literally "any" | no declared plan at all |
| D6 | **4** | matches score_4 | "`VaultNode` gains `intervals: Vec<(u32,u32)>`"; "Node click shows the node's entire lifetime" | a shared field on the shared node type, served to every importer — the anchor's shape, without a same-axis step past it |
| D7 | **1** | worse than score_2 | "it changes `VaultGraph`, every importer's output, metrics, facets, the binary cache, and the shaders"; "Blast radius across seven crates" | worse than one long phase: seven crates rewritten with no increment and no first seam |
| D8 | **3** | past score_2, short of score_4 | "conflicts with the `SourceEvent`/delta plan in `docs/importer-architecture.md` unless intervals are derived from that op-log rather than replacing it" — a cited, codebase-specific hazard with a countermeasure | one hazard; none of the anchor's four landmines |

**Raw 2.733. Essential NO on CK-007, CK-008, CK-009 → CAPPED at 2.000.**

---

### B5 — The graph schedules itself: runs, schedules, and transforms are nodes (p=0.07, med)

| CK | imp | Y/N | deciding quote |
|---|---|---|---|
| 001 | ess | Y | "A source's transforms are declared as today" |
| 002 | ess | Y | `kind = transform\|schedule\|run` — graph vocabulary |
| 003 | ess | Y | constraint 2 |
| 004 | ess | Y | constraint 2 |
| 005 | imp | Y | "emits `run` nodes whose `scheduled_at > now` are the upcoming list" — derived from a node field, not a fabricated number |
| 006 | imp | Y | constraint 2 |
| 007 | ess | Y | "**No new endpoints**, no new ledger type; provenance is edges" |
| 008 | ess | **N** | runs are nodes merged into every snapshot with no retention policy; "needs a `hidden` node class in the renderer and metric exclusion" is a visibility fix, not a bound |
| 009 | ess | **N** | — no cap handling |
| 010 | imp | Y | constraint 3; the executor is a Nix fold over schedule nodes |
| 011 | imp | Y | "`jc:run:*`", "`graph.merge`", "`graph_vcs::diff_snapshots`" (via Approach 1) |
| 012 | imp | Y | "\"show me everything the nightly relabel touched\" is a neighbour click" |
| 013 | imp | Y | b.md:148 / b.md:158 (document-level) |
| 014 | ess | Y | constraint 1 |
| 015 | imp | **N** | — no outcome field on a run node; no failure or empty-run behaviour |
| 016 | ess | Y | "nothing here is implemented" |
| 017 | opt | Y | "probability 0.07" |
| 018 | pit | N | no conflation |
| 019 | pit | N | the executor is a Rust transform that rides the existing rebuild, not a separate timing loop |
| 020 | pit | N | server-side |
| 021 | pit | **Y** | **defect** — "graph-api projects them as a subgraph under a reserved namespace (`jc:run:*`), merged into every snapshot with `graph.merge`" — run history accumulates inside the one served snapshot |
| 022 | pit | **Y** | **defect** — the transform's own output is the merge; no distinct addressable artifact |

**Dimensions**

| dim | score | placement | closer evidence | gap |
|---|---|---|---|---|
| D1 | **2** | past score_2, short of score_4 | "`run -> run (next)`, `run -> transform`, `run -> touched node` edges" | a run is a node with edges; no `transform` field, no revisions, no status, no delta record |
| D2 | **4** | matches score_4 | "transforms are declared as today" | inherited |
| D3 | **2** | **capped at 2 by CK-021 + CK-022** | — | unbounded growth in the live graph, and the "hidden node class" is cosmetic |
| D4 | **3** | past score_2, short of score_4 | "the \"upcoming runs\" UI is a facet filter on the canvas" — a highlight-set patch on the existing search seams | no per-frame rule, no revision guard; metric and layout damage from the meta-nodes is the larger integration cost |
| D5 | **3** | past score_2, short of score_4; lean→score_4 | "The scheduler is a Rust transform that reads `schedule` nodes (`every_ms`, `last_run`)" — a declared, serialisable plan stored as graph data, evaluated on the existing tick | no coalesce/no-overlap clause; "upcoming" depends on the importer overwriting a node |
| D6 | **3** | past score_2, short of score_4 | "Node click on a data node shows its `run` neighbours = its history" — improves every source through the shared Inspector | no new shared `NodeMeta` field; the anchor's `provenance { first_seen_run, last_seen_run, changed_in }` is not realised as metadata |
| D7 | **2** | matches score_2 | "no new endpoints, no new ledger type" — a small blast radius | no sequencing; the metric-exclusion and hidden-node-class work is unsequenced and unowned |
| D8 | **3** | past score_2, short of score_4 | "needs a `hidden` node class in the renderer and metric exclusion"; "reserved namespace collisions with pest packages that emit `run` kinds" — and the b.md:122 note that "the requester's loop already emits `session`/`event` nodes, so the two vocabularies must be visibly distinct" | two codebase-accurate hazards with partial counters; none of the anchor's four landmines |

**Raw 2.767. Essential NO on CK-008 and CK-009 → CAPPED at 2.000.**

---

### B6 — Nix compiles a combinator program; Rust executes it over a `GraphOp` stream (p=0.05, high)

| CK | imp | Y/N | deciding quote |
|---|---|---|---|
| 001 | ess | **N** | "the tvix expression evaluates not to a graph but to a small AST of `map \| filter \| reduce \| window \| relabel \| contract` combinators … `eval_graph` gets a sibling `eval_program`" — a new engine entry point and a bespoke return type, and "A DSL inside Nix inside TOML is three layers of syntax for users" |
| 002 | ess | Y | `GraphOp` vocabulary throughout |
| 003 | ess | Y | constraint 2 |
| 004 | ess | Y | constraint 2 |
| 005 | imp | Y | no fabricated number |
| 006 | imp | Y | constraint 2 |
| 007 | ess | Y | adds no HTTP endpoint; grounded in `docs/importer-architecture.md:178-212,625-640` |
| 008 | ess | **N** | "the timeline is the op-log with run boundaries as checkpoints" — no retention policy for the op-log or the checkpoints |
| 009 | ess | **N** | "Scales to the caps" but no cap-breach behaviour and no run failure record |
| 010 | imp | Y | constraint 3; "`graph-combinators.nix` gains a `program` namespace whose functions return data … rather than acting on `graph`" — deliberately sidesteps the closure hazard |
| 011 | imp | Y | "`Program::compile(ast) -> Pipeline`", "`Pipeline::run(ops: impl Iterator<Item=GraphOp>) -> impl Iterator<Item=GraphOp>`" |
| 012 | imp | Y | "Kubernetes watches and httpjson polls become native op streams; a transform over a stream is the same as a transform over a reload" |
| 013 | imp | Y | b.md:148 / b.md:158 (document-level) |
| 014 | ess | Y | "Nix is the configuration language, Rust is the engine; **Nix never sees the data**" — the purest possible answer |
| 015 | imp | **N** | — no failure or empty-run behaviour |
| 016 | ess | Y | "nothing here is implemented" |
| 017 | opt | Y | "probability 0.05" |
| 018 | pit | N | no conflation |
| 019 | pit | N | checkpoints ride the importer's op stream, not a new timing loop |
| 020 | pit | N | "Scales to the caps and is the only design consistent with the retired tvix-worker's absence (no unbounded eval)" — Rust owns execution |
| 021 | pit | N | the op-log is not the `ArcSwap<GraphSnapshot>` |
| 022 | pit | N | the pipeline produces a new op stream, not an in-place merge |

**Dimensions**

| dim | score | placement | closer evidence | gap |
|---|---|---|---|---|
| D1 | **2** | past score_2, short of score_4 | "run boundaries as checkpoints" | no run record; a checkpoint in a log is not the anchor's `Run` struct and carries no transform identity |
| D2 | **3** | past score_2, short of score_4 | stays a `format_version = 3` tvix package | `eval_program` and the combinator AST are a new return type, the same departure as A5 — the axis's whole point is the existing engine's contract |
| D3 | **3** | past score_2, short of score_4; lean→score_4 | "deltas are free, and node history is a seek in the log" — the cheapest per-run state of the 18 | no explicit bound on the op-log; "Depends on unlanded op-stream work" |
| D4 | **2** | matches score_2 by silence | — | B6 never states how the canvas moves; no render seam, no patch rule |
| D5 | **2** | matches score_2 | "the timeline is the op-log with run boundaries as checkpoints" | no schedule is described at all; a checkpoint is not a plan |
| D6 | **3** | past score_2, short of score_4 | "node history is a seek in the log" | no new shared `NodeMeta` field; the anchor's provenance fields are not realised |
| D7 | **1** | worse than score_2 | "Depends on unlanded op-stream work"; "This lands the `SourceEvent`/`GraphOp` boundary from `docs/importer-architecture.md` Phase 3 as a prerequisite" | an entire unlanded prerequisite phase, plus "vocabulary churn until real transforms exist" — worse than one long phase |
| D8 | **2** | past score_2, short of score_4 | "the retired tvix-worker's absence (no unbounded eval)" — anchor landmine 1, correctly cited | "A DSL inside Nix inside TOML is three layers of syntax for users; vocabulary churn" is a product concern, not a repository landmine |

**Raw 2.233. Essential NO on CK-001, CK-008, CK-009 → DISQUALIFIED, CAPPED at 2.000.**

---

### C1 — Transform packages as an importer decorator with a run ledger (p=0.85, med-high)

| CK | imp | Y/N | deciding quote |
|---|---|---|---|
| 001 | ess | Y | "A transform package is a normal `format_version = 3` tvix package with `[parser] input = \"graph\"`"; constraint 4: "Rust everywhere; **no new crate per source**; packages carry mechanism, the catalog carries binding" |
| 002 | ess | Y | "Run { id, transform_id, started, finished, input_revision, output_revision, outcome: Ok{…}\|Failed{msg}\|Skipped{Unchanged}, duration }" — pure graph vocabulary |
| 003 | ess | Y | constraint 4 / "Rust everywhere; no new crate per source" |
| 004 | ess | Y | "reduced-motion snaps" (also c.md:31 and the success criteria "`prefers-reduced-motion` disables tweening") |
| 005 | imp | Y | "the in-flight run with **measured progress**" (success criteria); c.md:31 "Honest progress" |
| 006 | imp | Y | c.md:16 "honest progress via `panel_kit::loading`"; c.md:31 "the existing loading-store discipline in the UI" |
| 007 | ess | Y | "`/runs` (JSON) lists past + in-flight + next; `/runs/:id/diff` serves the diff as dense node indices against the output revision"; reuses `/graph/meta_summary` and `/node/:id` |
| 008 | ess | Y | "graph-api keeps a ring of the last N runs in `AppState` and appends one JSONL line" |
| 009 | ess | Y | c.md:10 grounds the caps ("caps output at 1M nodes / 4M edges (`lib.rs:43-44`)") and C1's run carries `outcome: … \|Failed{msg}` plus "a failed run never publishes a partial graph" |
| 010 | imp | Y | constraint 2: "Nix library style: attrset graphs, `{ graph }` parameter passing, no closure across 3+ import levels" |
| 011 | imp | Y | "`crates/importer/src/transform.rs`", "`render_expression`", "`importer_catalog.rs:225`", "`RawNode`", "`convert_generated_graph`" |
| 012 | imp | Y | "an Obsidian user writes \"label every note with its folder depth and connect orphans to an inbox node\" once and sees it run after every save; a Kubernetes user contracts ReplicaSet→Pod edges nightly" |
| 013 | imp | **N** | — the string `knowledge` does not occur anywhere in set C |
| 014 | ess | Y | "the current snapshot's graph rendered as a Nix attrset literal (or, at scale, served through the VFX as `/jc/input/graph.json` and read with `builtins.fromJSON`)"; constraint 2: "no impure builtins" |
| 015 | imp | Y | "outcome: Ok{…}\|Failed{msg}\|Skipped{Unchanged}" + "a failed run never publishes a partial graph" + the Runs panel shows "outcome, counts, error" |
| 016 | ess | Y | "**Design options only. Nothing here is implemented.**" |
| 017 | opt | Y | "**Probability:** 0.85" |
| 018 | pit | N | c.md:15: "it is a layout scrubber, not a graph-state timeline" |
| 019 | pit | N | "Scheduling reuses `WatchPlan::Poll { interval_ms }` and adds one optional `schedule = \"every 5m\" \| \"cron …\"` on the binding" |
| 020 | pit | N | "graph-api native (blocking thread, like `/generate`)" |
| 021 | pit | N | a ring in `AppState` plus a JSONL file |
| 022 | pit | N | "`apply(&VaultGraph) -> Result<TransformOutcome>` … a failed run never publishes a partial graph" — the outcome is a distinct value before it is published |

**Dimensions**

| dim | score | placement | closer evidence | gap |
|---|---|---|---|---|
| D1 | **3** | past score_2, short of score_4 | "Run { id, transform_id, started, finished, input_revision, output_revision, outcome: Ok{added,removed,changed}\|Failed{msg}\|Skipped{Unchanged}, duration }" | the anchor's `pub bound: BTreeMap<String,Value>` is absent — the run is not reproducible from its record, since `variables` live on the transform binding; and `GraphTransform` is a struct, so a Rust-only transform must be smuggled in as an `Importer` |
| D2 | **5** | better than score_4 on this axis | "`[parser] input = \"graph\"`"; "`render_expression` gains one more binding … keeping `render_expression` O(1)`"; "no new crate per source" | the anchor shows a scalar bound variable; C1 shows the graph arriving as a *declared input* without growing the render step, and the mechanism/binding split is explicit |
| D3 | **3** | past score_2, short of score_4; lean→score_4 | "a ring of the last N runs in `AppState` and appends one JSONL line to `<packages_dir>/runs.local.jsonl`"; "`/runs/:id/diff` serves the diff as dense node indices against the output revision" | a bounded differential ring, but the anchor's "full keyframe every K runs" is absent and N is unnamed |
| D4 | **4** | matches score_4 | "extends the Graph panel with a **revision scrubber**: selecting a run fetches that run's diff and tweens colors/alpha via `set_search_highlights` (added = highlight, removed = ghost) and positions via `set_positions`" | the revision guard is the trigger but is never named as the per-frame admission check |
| D5 | **3** | past score_2, short of score_4; lean→score_4 | "Scheduling reuses `WatchPlan::Poll { interval_ms }` and adds one optional `schedule = \"every 5m\" \| \"cron …\"` on the binding; `next_run_at` is computed from it and served" | a declared, serialisable plan on the existing loop; the coalesce/no-overlap clause is absent, and its own risk line caps scope to "`every <duration>` plus fixed clock times" |
| D6 | **5** | better than score_4 on this axis | "`RawNode` keeps `metadata` as `serde_json::Map`, `convert_generated_graph` maps it into `frontmatter`, and `NodeMeta` gains `provenance { source_id, first_seen_revision, last_run_id }`; the Nodes focus view gains a generic **Facts** section (every frontmatter key/value, tags, degree, neighbours) … so metadata-only nodes are no longer blank" | the anchor only adds fields; C1 names both metadata-drop points in the codebase and removes the cause, then renders it — the same-axis respect |
| D7 | **2** | matches score_2 | one design, no phase list | C1's increment bundles a new engine input, a new `transform.rs`, a decorator, a ledger, four endpoints, a Runs panel, a revision scrubber, and a `NodeMeta` change |
| D8 | **3** | past score_2, short of score_4 | "`deepSeq` forces the whole output so laziness is only inside the expression"; "graph literal injection is the scalability wall (mitigated by VFS JSON)"; "cron parsing scope creep — keep to `every <duration>` plus fixed clock times" | two codebase-accurate hazards with one countermeasure; the caps hazard is grounded in the document but never tied to a run record |

**Raw 3.466. No essential NO. No pitfall YES. FINAL 3.466 — rank 3.**

---

### C2 — Runs as graph-vcs commits; timeline = branch log (p=0.80, high)

| CK | imp | Y/N | deciding quote |
|---|---|---|---|
| 001 | ess | Y | "The transform contract and `graph-iter.nix` are as in Approach 1" |
| 002 | ess | Y | a run is a `Commit`; no agent vocabulary in the core |
| 003 | ess | Y | constraint 4 |
| 004 | ess | Y | c.md:31 |
| 005 | imp | Y | no fabricated number |
| 006 | imp | Y | c.md:16 / c.md:31 |
| 007 | ess | Y | "The UI reuses the History panel for past runs … `/node/:id` returns the commit that introduced the node" |
| 008 | ess | **N** | "commit growth for high-cadence runs (needs squash/retention)" — the policy is named as a *need*, and a full `Snapshot` is committed per run |
| 009 | ess | Y | c.md:10 grounds the caps and "A failed run commits nothing and is recorded as a branch-scoped event in `/progress`" — a typed failure record |
| 010 | imp | Y | constraint 2 |
| 011 | imp | Y | "`TransformSchedule { world, branch, transform, cadence }`", "`graph_data_from_snapshot`", "`graph_canvas.rs:174`" |
| 012 | imp | Y | "any world owner gets versioned, branchable, revertable transforms" |
| 013 | imp | **N** | — no `knowledge` mention in set C |
| 014 | ess | Y | inherits C1's declared input |
| 015 | imp | Y | "A failed run commits nothing and is recorded as a branch-scoped event in `/progress`" |
| 016 | ess | Y | "Nothing here is implemented." |
| 017 | opt | Y | "**Probability:** 0.80" |
| 018 | pit | N | c.md:15 distinguishes the FrameRing |
| 019 | pit | **Y** | **defect** — "A scheduler task in `session-manager` (native host) or graph-api (standalone …) holds `TransformSchedule {…}`; each tick materialises the branch head" — a new scheduler, and C2's own risk names "scheduler duplication between hosts" |
| 020 | pit | N | server-side / world host |
| 021 | pit | N | a `MinigrafStore`, not the `ArcSwap<GraphSnapshot>` |
| 022 | pit | N | "A failed run commits nothing"; the output is a distinct commit |

**Dimensions**

| dim | score | placement | closer evidence | gap |
|---|---|---|---|---|
| D1 | **3** | past score_2, short of score_4 | "writes a `Commit` whose message and metadata carry the run record (transform id, variables hash, diff counts, duration, outcome)" | the anchor's `pub bound` and `pub status` are only approximated — "variables **hash**", not the bound values, and no `input_revision`/`output_revision` on the commit |
| D2 | **4** | matches score_4 | "as in Approach 1" | inherited |
| D3 | **2** | matches score_2 | "each tick materialises the branch head, applies the transform, and writes a `Commit`" | a full snapshot per commit with retention named only as a future need — score_2's failure |
| D4 | **3** | past score_2, short of score_4 | "animates by materialising two commits client-side … and tweening between their buffer layouts" | materialising two scenes and tweening is materially more work than patching changed ranges under the revision guard |
| D5 | **2** | **capped at 2 by CK-019** | "`TransformSchedule { world, branch, transform, cadence }`" | a declared plan, but a new scheduler duplicated across two hosts, with no coalesce clause |
| D6 | **4** | matches score_4 | "`/node/:id` returns the commit that introduced the node and the last commit that changed its metadata, plus the same Facts section as Approach 1" | inherits C1's widening; blame is additive |
| D7 | **2** | matches score_2 | "the standalone `graph-api --source=…` user only gets this if graph-api also embeds a store per source" | a conditional, not a sequencing |
| D8 | **3** | past score_2, short of score_4 | "commit growth for high-cadence runs (needs squash/retention)"; "snapshot materialisation cost in the browser for large graphs"; "scheduler duplication between hosts" | three codebase-accurate hazards, one with a stated remedy; none of the anchor's four landmines |

**Raw 2.833. Essential NO on CK-008 → CAPPED at 2.000.**

---

### C3 — Client-resident transform pipeline on the existing Timeline panel (p=0.80 slice / 0.55 final, med)

| CK | imp | Y/N | deciding quote |
|---|---|---|---|
| 001 | ess | Y | "`graph-iter.nix` ships in the embedded VFS so the same package text works server-side later" |
| 002 | ess | Y | "Run as in Approach 1" — graph vocabulary |
| 003 | ess | Y | constraint 4 |
| 004 | ess | Y | c.md:31; the success criteria "`prefers-reduced-motion` disables tweening" |
| 005 | imp | Y | c.md:31 |
| 006 | imp | **Y** | "`stage()` steps expressed through `panel_kit::loading` stores (fetch snapshot → build Nix input → eval → diff → commit to ring)" — the strongest loading-store answer in the corpus |
| 007 | ess | Y | "Keep the server untouched"; reuses `/graph/meta_summary` and `/node/:id` |
| 008 | ess | Y | "persist to localStorage under `jc_runs_v1` with a bounded count" |
| 009 | ess | **N** | "wasm eval blocks the UI thread above the caps (the retired tvix-worker would need restoring, `lib.rs:39-41`)" names the hazard; no cap-breach run record and no failed-run path |
| 010 | imp | Y | constraint 2 |
| 011 | imp | Y | "`app/ui/src/transform.rs`", "`jc_runs_v1`", "`panel_kit::loading`" |
| 012 | imp | Y | "a viewer of a read-only deployment (no packages dir write access) can still author and run transforms against what they see" |
| 013 | imp | **N** | — no `knowledge` mention in set C |
| 014 | ess | Y | "Input is the current `GraphData` plus `/graph/meta_summary` … rendered into the VFS as JSON for `builtins.fromJSON`" |
| 015 | imp | **N** | — no failure or empty-run behaviour |
| 016 | ess | Y | "Nothing here is implemented." |
| 017 | opt | Y | "0.80 for a first vertical slice; 0.55 as the final shape" — the only split estimate in the corpus |
| 018 | pit | N | "the Timeline panel's frame ring becomes `enum Frame { Positions(..), GraphState { run_id, ids, edges, colors } }`" — a second variant, not a conflation |
| 019 | pit | **Y** | **defect** — "schedules with a browser timer"; "Upcoming runs are the timer's next fire plus the cadence" |
| 020 | pit | **Y** | **defect** — "the Dioxus app evaluates transform packages in-browser with `tvix-wasm`"; "**Risks:** … eval on the main thread" |
| 021 | pit | N | a localStorage ring |
| 022 | pit | N | the client patches the view; the served graph is untouched |

**Dimensions**

| dim | score | placement | closer evidence | gap |
|---|---|---|---|---|
| D1 | **3** | past score_2, short of score_4 | "Run as in Approach 1" | inherits C1's record, which lacks `bound`; a Rust transform here must live in `app/ui/src/transform.rs` |
| D2 | **4** | matches score_4 | "the same package text works server-side later" | inherited |
| D3 | **2** | matches score_2 | "`enum Frame { Positions(..), GraphState { run_id, ids, edges, colors } }`" | a **full id+edge list per run frame** — a whole-graph copy per run, bounded only by a count; the axis's negative pole with a bound |
| D4 | **2** | **capped at 2 by CK-020** | "the scrubber moves between graph states with a tween" | the defect the cap records: synchronous `tvix-wasm` eval on the browser main thread, above the caps, with the timeout worker retired |
| D5 | **2** | **capped at 2 by CK-019** | "Upcoming runs are the timer's next fire plus the cadence" | a browser timer is neither a declared deployment plan nor the watch tick loop |
| D6 | **3** | past score_2, short of score_4 | "the Nodes focus view gets the Facts section, and transform outputs that add `metadata` are merged client-side into a per-run overlay" | the facts are client-side; no shared `NodeMeta` field is added, so not every importer gains it from the API |
| D7 | **3** | past score_2, short of score_4; lean→score_4 | "**Zero server changes**" / "no server changes" and "0.80 for a first vertical slice" | a genuine first slice on an existing seam, but a one-line plan, not an increment list |
| D8 | **3** | past score_2, short of score_4 | "wasm eval blocks the UI thread above the caps (the retired tvix-worker would need restoring, `lib.rs:39-41`)" — anchor landmine 1 with a file citation and a named countermeasure | one anchor landmine, correctly cited; "localStorage quota" and "duplication" are product concerns |

**Raw 2.733. Essential NO on CK-009 → CAPPED at 2.000.**

---

### C4 — The reflexive graph: runs, schedules, and transforms are nodes (p=0.08, low-med)

| CK | imp | Y/N | deciding quote |
|---|---|---|---|
| 001 | ess | Y | "The transform contract stays tvix `{ graph, … }`" |
| 002 | ess | Y | "`type = transform\|schedule\|run`" — graph vocabulary |
| 003 | ess | Y | constraint 4 |
| 004 | ess | Y | c.md:31 |
| 005 | imp | Y | no fabricated number |
| 006 | imp | Y | c.md:16 / c.md:31 |
| 007 | ess | Y | "**no new persistence, no new endpoints, no proto change**" |
| 008 | ess | **N** | run nodes accumulate in the graph with no retention policy |
| 009 | ess | **N** | — no cap handling |
| 010 | imp | Y | constraint 2 |
| 011 | imp | Y | "`run:<id>` node with metadata `{started, finished, outcome, added, removed}`"; "`set_search_highlights`" |
| 012 | imp | Y | "any source gains provenance nodes explorable with tools that already exist" |
| 013 | imp | **N** | — no `knowledge` mention |
| 014 | ess | Y | constraint 1/2 |
| 015 | imp | Y | the `run:<id>` node carries `outcome`, and the Inspector renders lineage — a failed run is a node with a stated outcome |
| 016 | ess | Y | "Nothing here is implemented." |
| 017 | opt | Y | "**Probability:** 0.08" |
| 018 | pit | N | c.md:15 |
| 019 | pit | N | "kept current by the scheduler rewriting that one node" — rides the rebuild, not a separate timing loop |
| 020 | pit | N | server-side |
| 021 | pit | **Y** | **defect** — "`GraphTransform::apply` writes its own provenance into the output: a `run:<id>` node … and edges to every node it created or changed" — every run's history is written into the live served graph |
| 022 | pit | **Y** | **defect** — the transform's own output is the merge; nothing lands as a distinct artifact |

**Dimensions**

| dim | score | placement | closer evidence | gap |
|---|---|---|---|---|
| D1 | **3** | past score_2, short of score_4 | "a `run:<id>` node with metadata `{started, finished, outcome, added, removed}` and edges to every node it created or changed" — the closest any node-model proposal comes to the anchor's field set | the anchor's `input_revision`/`output_revision`/`bound` are absent, and there is no `Run` struct — a graph node is not the anchor's record |
| D2 | **4** | matches score_4 | "stays tvix `{ graph, … }`" | restated by reference only |
| D3 | **2** | **capped at 2 by CK-021 + CK-022** | — | "inflates edge counts by O(touched nodes)" and no retention policy; all history is in the one snapshot |
| D4 | **3** | past score_2, short of score_4 | "selecting a run node highlights its neighbourhood (touched nodes) using `set_search_highlights`" | a highlight-set patch, not a per-frame changed-range patch, and no revision guard |
| D5 | **3** | past score_2, short of score_4; lean→score_4 | "`schedule:<id>` nodes whose metadata holds cadence and `next_run_at`" — a declared, serialisable plan as graph data | no coalesce clause, and "makes \"upcoming\" a mutable node that the importer must overwrite" |
| D6 | **3** | past score_2, short of score_4 | "the Inspector's neighbour pills already render the lineage" | topology, not shared `NodeMeta` fields; the anchor's `provenance { first_seen_run, last_seen_run, changed_in }` is never realised as metadata |
| D7 | **2** | matches score_2 | "no new persistence, no new endpoints, no proto change" — a small blast radius | no sequencing; the metric-pollution problem C4 names has no owning phase |
| D8 | **3** | past score_2, short of score_4 | "the existing `Unchanged` short-circuit no longer fires because provenance nodes change every run" — a precise, codebase-specific hazard; "users cannot opt out of the pollution without a filter" | one precise hazard with a partial countermeasure; none of the anchor's four landmines |

**Raw 2.867. Essential NO on CK-008 and CK-009 → CAPPED at 2.000.**

---

### C5 — Transforms as a graph-compute engine with Kueue-scheduled runs (p=0.06, high)

| CK | imp | Y/N | deciding quote |
|---|---|---|---|
| 001 | ess | Y | "the worker evaluates the Nix package natively" — the same tvix package |
| 002 | ess | Y | "graph-api gains a `LoadGraph`-style RPC for transform jobs" — graph vocabulary |
| 003 | ess | Y | constraint 4 |
| 004 | ess | Y | c.md:31 |
| 005 | imp | Y | no fabricated number; `next_run_at` is read from cluster status |
| 006 | imp | Y | c.md:16 / c.md:31 |
| 007 | ess | Y | "Animation reuses `/graph/layout/stream` framing (`server.rs:2374-2385`)"; the RPC is modelled on the existing `LoadGraph` and the risk of that choice is stated |
| 008 | ess | **N** | no retention policy for "the cluster's job history" or the appended run records |
| 009 | ess | **N** | — no cap handling on the worker |
| 010 | imp | Y | constraint 2 |
| 011 | imp | Y | "`LoadGraph`-style RPC", "`/graph/layout/stream`", "`server.rs:2374-2385`", "`JUMP_CANNON_COMPUTE_URL`" |
| 012 | imp | Y | "cluster operators get scheduled graph maintenance with cluster-native observability" |
| 013 | imp | **N** | — no `knowledge` mention |
| 014 | ess | Y | the package expression is the same; no I/O |
| 015 | imp | **N** | — no failure or empty-run behaviour |
| 016 | ess | Y | "Nothing here is implemented." |
| 017 | opt | Y | "**Probability:** 0.06" |
| 018 | pit | N | no conflation |
| 019 | pit | **Y** | **defect** — "schedule them as Kubernetes CronJobs admitted by Kueue" — a job queue, the checklist's named anti-pattern, chosen deliberately |
| 020 | pit | N | "the worker evaluates the Nix package natively" — off the UI thread |
| 021 | pit | N | "graph-api swaps a snapshot and appends a run record" — the record is separate |
| 022 | pit | N | the worker returns "a new CSR + metadata patch" and graph-api swaps a fully built snapshot |

**Dimensions**

| dim | score | placement | closer evidence | gap |
|---|---|---|---|---|
| D1 | **2** | past score_2, short of score_4 | "graph-api swaps a snapshot and appends a run record" | no run-record fields at all; the anchor's seven are unstated |
| D2 | **4** | matches score_4 | "the worker evaluates the Nix package natively" | inherited; C5 never restates the package shape |
| D3 | **2** | matches score_2 | the store is "the cluster's job history" with no bound | unbounded, and per job the worker returns a whole new CSR |
| D4 | **3** | past score_2, short of score_4 | "Animation reuses `/graph/layout/stream` framing … by emitting position frames for a morph between old and new layouts" | it is a per-frame stream, but over the *position* contract for *topology* changes — C5's own risk: "abusing the position-stream contract for topology changes" |
| D5 | **2** | **capped at 2 by CK-019** | "`next_run_at` read from the CronJob status, so \"upcoming\" is authoritative" | a genuinely authoritative plan, delivered through a job queue that excludes the standalone user |
| D6 | **2** | by silence | — | C5 proposes no node-content change at all |
| D7 | **1** | worse than score_2 | "the standalone/local user has neither Kueue nor a worker (`JUMP_CANNON_COMPUTE_URL` unset), so the feature would be cluster-only"; "two implementations (local + worker) or none for local" | an unbuilt prerequisite that excludes a whole class of users, with no first seam |
| D8 | **3** | past score_2, short of score_4 | "abusing the position-stream contract for topology changes; two implementations (local + worker) or none for local" | two codebase-accurate hazards, honestly stated, with no countermeasure; none of the anchor's four landmines |

**Raw 2.400. Essential NO on CK-008 and CK-009 → CAPPED at 2.000.**

---

### C6 — Time as a variable: the schedule is a lazy Nix sequence (p=0.07, med)

| CK | imp | Y/N | deciding quote |
|---|---|---|---|
| 001 | ess | Y | "The package declares `[parser] input = \"graph\"`" — the transform expression keeps the existing contract |
| 002 | ess | Y | "A run's identity is `(package hash, variables hash, input revision, t)`" — graph vocabulary |
| 003 | ess | Y | constraint 4 |
| 004 | ess | Y | c.md:31 |
| 005 | imp | Y | no fabricated number |
| 006 | imp | Y | c.md:16 / c.md:31 |
| 007 | ess | Y | adds no HTTP endpoint |
| 008 | ess | **N** | "no run ledger at all" — and "replaying `t` in the past requires the input revision to be retained — which reintroduces Approach 2's store through the back door", with no bound on what is retained |
| 009 | ess | **N** | "error attribution for failed runs vanishes" — C6 states the opposite of the required behaviour |
| 010 | imp | Y | constraint 2; "`graph-iter.nix` gains `takeWhile/iterate/unfold`" under the stated convention |
| 011 | imp | Y | "`[parser.schedule] expr = ''{ now }: builtins.genList (i: now + i * 300) 64''`", "`takeWhile/iterate/unfold`" |
| 012 | imp | Y | "demo/synthetic users get smooth \"evolve this graph over time\" views with zero storage" |
| 013 | imp | **N** | — no `knowledge` mention |
| 014 | ess | Y | all input arrives as bound values and a declared schedule expr; no I/O |
| 015 | imp | **N** | "error attribution for failed runs vanishes" |
| 016 | ess | Y | "Nothing here is implemented." |
| 017 | opt | Y | "**Probability:** 0.07" |
| 018 | pit | N | no conflation |
| 019 | pit | **Y** | **defect** — "the Rust side walks the sequence against the wall clock" — a new wall-clock driver unmoored from `WatchPlan` |
| 020 | pit | N | "graph-api evaluates the schedule expr" and evaluates the transform server-side |
| 021 | pit | N | a memo key, not extra snapshots stuffed into the served one |
| 022 | pit | N | recomputation, not an in-place merge |

**Dimensions**

| dim | score | placement | closer evidence | gap |
|---|---|---|---|---|
| D1 | **3** | past score_2, short of score_4; lean→score_4 | "A run's identity is `(package hash, variables hash, input revision, t)`" — three of the anchor's fields (`transform`, `bound`, `input_revision`) in one tuple | "which is a memo key rather than a record"; no `output_revision`, no `status`, no `delta` |
| D2 | **3** | past score_2, short of score_4 | "`[parser] input = \"graph\"`" keeps the transform expression on the existing engine | "a `[parser.schedule] expr" is a bespoke field in the envelope, and the anchor's plain `[[parser.variables]]` shape is extended with a second expression slot |
| D3 | **2** | matches score_2 | "no run ledger at all"; "requires the input revision to be retained" | the required retention is unbounded and C6 concedes it "reintroduces Approach 2's store through the back door" |
| D4 | **1** | worse than score_2 | "the timeline scrubber asks the server to **evaluate the transform at an arbitrary `t`** … so \"animation\" can be continuous in `t\`" | every animation frame triggers a server-side Nix evaluation — strictly worse than score_2's "re-fetch and rebuild every buffer per tick" |
| D5 | **2** | **capped at 2 by CK-019** | "`{ now }: builtins.genList (i: now + i * 300) 64\`" is the most declarative plan in the corpus | but the firing driver is a wall-clock walk, not the watch tick, and there is no coalesce clause |
| D6 | **2** | by silence | — | C6 proposes no node-content change |
| D7 | **2** | matches score_2 | one design, no phases | its own trade-off concedes the design does not close |
| D8 | **2** | past score_2, short of score_4 | "the input snapshot is not deterministic for any live source" | "users expect a run to be a fact, not a recomputation … a slow transform makes scrubbing unusably slow" are product worries, not repository landmines |

**Raw 2.100. Essential NO on CK-008 and CK-009 → CAPPED at 2.000.**

---

## 5. Ranking and vote

### 5.1 Score matrix (1–5 scale; `spec_formula` = total ÷ 5)

| # | proposal | D1 .100 | D2 .133 | D3 .133 | D4 .133 | D5 .134 | D6 .100 | D7 .133 | D8 .134 | raw | gate | **final** |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 1 | **B1** | 5 | 5 | 3 | 4 | 3 | 5 | 2 | 4 | 3.800 | — | **3.800** (0.760) |
| 2 | **A1** | 3 | 5 | 4 | 4 | 3 | 5 | 2 | 4 | 3.733 | — | **3.733** (0.747) |
| 3 | **C1** | 3 | 5 | 3 | 4 | 3 | 5 | 2 | 3 | 3.466 | — | **3.466** (0.693) |
| 4 | A3 | 3 | 4 | 3 | 4 | 3 | 4 | 3 | 3 | 3.366 | — | 3.366 (0.673) |
| 5 | B2 | 4 | 4 | 2 | 4 | 2\* | 4 | 2 | 3 | 3.066 | CK-008, CK-009 | 2.000 |
| 6 | A2 | 3 | 4 | 2 | 4 | 3 | 4 | 3 | 2 | 3.099 | CK-008, CK-009 | 2.000 |
| 7 | A6 | 2 | 4 | 2 | 4 | 3 | 4 | 3 | 2 | 2.999 | CK-008 | 2.000 |
| 8 | B5 | 2 | 4 | 2\* | 3 | 3 | 3 | 2 | 3 | 2.767 | CK-008, CK-009 | 2.000 |
| 9 | C4 | 3 | 4 | 2\* | 3 | 3 | 3 | 2 | 3 | 2.867 | CK-008, CK-009 | 2.000 |
| 10 | B4 | 2 | 4 | 2\* | 4 | 2 | 4 | 1 | 3 | 2.733 | CK-007, CK-008, CK-009 | 2.000 |
| 11 | C3 | 3 | 4 | 2 | 2\* | 2\* | 3 | 3 | 3 | 2.733 | CK-009 | 2.000 |
| 12 | B3 | 3 | 4 | 3 | 2\* | 2\* | 3 | 3 | 2 | 2.732 | CK-009 | 2.000 |
| 13 | A4 | 2 | 4 | 2\* | 4 | 2 | 3 | 1 | 3 | 2.633 | CK-008, CK-009 | 2.000 |
| 14 | C5 | 2 | 4 | 2 | 3 | 2\* | 2 | 1 | 3 | 2.400 | CK-008, CK-009 | 2.000 |
| 15 | A5 | 2 | 3 | 3 | 2 | 2\* | 2 | 1 | 3 | 2.267 | **CK-001**, CK-009 | 2.000 |
| 16 | B6 | 2 | 3 | 3 | 2 | 2 | 3 | 1 | 2 | 2.233 | **CK-001**, CK-008, CK-009 | 2.000 |
| 17 | C6 | 3 | 3 | 2 | 1 | 2\* | 2 | 2 | 2 | 2.100 | CK-008, CK-009 | 2.000 |
| 18 | C2 | 3 | 4 | 2 | 3 | 2\* | 4 | 2 | 3 | 2.833 | CK-008 | 2.000 |

\* = dimension capped at 2 by a `pitfall` YES, per the specification's `pitfall_handling`.

Rows 5–18 are ordered by the specification's tie-break chain (D1, then D3, then D6) within the exact
tie at 2.000: B2 (D1 4), then A2 (D1 3, D3 2, D6 4), C2 (3, 2, 4), C4 (3, 2, 3), C3 (3, 2, 3),
B3 (3, 3, 3), C6 (3, 2, 2), then the D1 = 2 group — B6 (D3 3, D6 3), A5 (3, 2), A6 (2, 4),
B4 (2, 4), A4 (2, 3), B5 (2, 3), C5 (2, 2).

**Important:** the 2.000 floor is a gate, not a measurement. It is not evidence that those fourteen
designs are equivalent to each other — their raw totals span 2.100 to 3.099, and A2's raw 3.099 is
higher than every other capped proposal including B2's 3.066. The specification's cap deliberately
destroys that resolution, and the tie-break chain (D1, then D3, then D6) is doing the ranking work
among them instead of measured quality. Read the `raw` column, not the `final` column, if you want
to know which of those fourteen designs is actually best.


### 5.2 Disqualification

The specification disqualifies a NO on **CK-001, CK-002, CK-003, CK-014, or CK-016**.

- **CK-002, CK-003, CK-014, CK-016: no proposal is disqualified.** All 18 pass all four.
- **CK-001: two proposals are disqualified** — **A5** ("A transform package's `expr` returns not a
  graph but a lazy **stream**"; `tvix_wasm::eval_stream`) and **B6** ("`eval_graph` gets a sibling
  `eval_program`"). Both leave the existing `{expr, variables}` → graph contract for a bespoke
  return type, which is the axis CK-001 exists to protect. Neither is in contention anyway.

### 5.3 My ranked vote — top 3

> ## **B1, A1, C1**

**Rank 1 — B1, "Transform packages + run ledger in graph-api" (p=0.90). Total 3.800.**
*Carried by:* **D1 Core abstraction generality (5)** and **D2 Package and engine model reuse (5)**.
B1 is the only proposal that delivers the anchor's second D1 sentence as a shipped fact rather than
a permission: `trait Transform { fn id; fn apply(&self, input: &VaultGraph, ctx: &RunCtx) -> Result<TransformOutput, TransformError>; }` lives in a standalone wasm-clean `graph-transform` crate, outside the server, and is implemented twice over — by `TvixTransform` (a package) and by `LabelCommunities` / `PruneDegree` / `ContractChains` (pure Rust). That is precisely the seam the requester's "generic Rust transformation struct" needs, and it is the one thing none of the other seventeen proposes. B1 also leads on D6 by fixing both metadata-drop points (`RawNode` at `lib.rs:233-246`, `convert_generated_graph` at `tvix.rs:512-566`) rather than only widening `NodeMeta`. It is the only proposal that answers CK-009 completely: *"needs a per-run time budget and `JC_MAX_*` caps applied to input as well as output"*, paired with a `status` field on `RunRecord`. B1 is also the only one of the five survivors to name the knowledge-map update.
*Weakest dimension:* **D7 Incremental deliverability (2)** — one design, no phase list. Its first increment bundles a new crate, a pipeline stage, four endpoints, a new panel, a new render seam, and a `NodeMeta` change. **To raise it:** cut P1 to "`input = \"graph\"` renders, evaluates on `spawn_blocking` against a real `generate-clusters.toml` graph, and one manual run's delta round-trips through `/runs/:id/delta` with positions carried by id" — everything else moves to P2+.
*First-phase kill criterion:* full-graph JSON injection proves unusable at the ~100k-node scale B1's own trade-off names (*"Full-graph JSON into Nix is O(n) memory and slow past ~100k nodes"*), **or** `render::apply_delta` cannot be made to run under the existing `graph_canvas::load` revision guard without a full remount. Either result retires the in-Nix path for large graphs and the design must fall back to the built-in Rust transforms, which is a different proposal.

**Rank 2 — A1, "Transform packages + run journal in graph-api" (p=0.85). Total 3.733.**
*Carried by:* **D6 Node-content generality (5)** and **D2 Package and engine model reuse (5)**.
A1 is the only proposal with the tightest **D3 Per-run state model (4)** in the corpus: *"a ring of N (default 256) alongside the last K (default 8) full snapshots"*, with `/runs/:id/diff` returning *"two LE `u32` index buffers (added, removed) against `/graph/ids` of `revision_after`"*. That is a bounded delta plus periodic keyframes with named numbers — the only approach that answers the anchor's D3 completely. A1 also keeps the transform minimal in the right way: *"The Rust side is a `crates/importer` type, not a crate"* plus `role = "transform" (default "generate")`, which is the least invasive way to add a graph input to the envelope and keeps every `generate-*.toml` valid.
*Weakest dimension:* **D7 Incremental deliverability (2)**. **To raise it:** same cut as B1's, plus publish the §4 ordering A1 already identifies (*"Node-content work is approach-independent and should land first"*) as A1's own P1 — that slice lands on `focused_node` + `NodeMeta`, an existing seam, and is usable the day it merges, independent of every run decision.
*First-phase kill criterion:* a tvix cap breach or an eval error does not produce a visible failed run — `RunRecord` has a bare `error` field and no typed status — **or** the N=256 / K=8 retention cannot coexist with A1's own admission that *"full rebuild per run (metrics + Tantivy) caps cadence at tens of seconds for large graphs"*, in which case the ring is correct and the cadence is not, and the schedule model has to be redesigned before anything else lands.

**Rank 3 — C1, "Transform packages as an importer decorator with a run ledger" (p=0.85). Total 3.466.**
*Carried by:* **D6 Node-content generality (5)** and **D2 Package and engine model reuse (5)**.
C1 ties A1 on both of its carrying dimensions and beats it on CK-015 by a wide margin — the only
proposal with a typed outcome that distinguishes success from failure from *no change*:
`outcome: Ok{added,removed,changed} | Failed{msg} | Skipped{Unchanged}`, plus the guarantee
*"a failed run never publishes a partial graph"*. C1 is also the only proposal that answers the
empty-result case the specification calls out explicitly. Its `[parser] input = "graph"` with
`render_expression` kept O(1) via the VFS is the cleanest statement of the package change, and its
`TransformedImporter<I: Importer>` composition sits on an existing trait rather than introducing one.
*Weakest dimension:* **D7 Incremental deliverability (2)**. **To raise it:** ship T7 (the
`RawNode` / `convert_generated_graph` metadata fix) as P1 — C1's own "shared floor" already
identifies it as approach-independent, it touches two files, and it answers the user's third
explicit ask for every importer with no dependency on any run or timeline decision.
*First-phase kill criterion:* the graph-literal injection wall holds — *"graph literal injection is the scalability wall (mitigated by VFS JSON)"* and the VFS mitigation turns out not to keep `render_expression` O(1) in practice, **or** *"protobuf change to `NodeMeta` touches session-manager and app proto regen"* proves to require coordinated cross-repo changes that cannot land in one change. Either outcome invalidates the "widest reuse of existing seams" claim on which C1's advantage rests.

### 5.4 Diversity requirement — and the collision I must record

The specification requires that the three selected proposals not be three phrasings of one design,
and that where two would land their first phase on the same seam, the pair that differs in **where
the run state lives** be preferred.

**The collision, stated plainly: B1, A1, and C1 all land their first increment on the same seam** —
the tvix package gaining a graph input, applied as a stage of `rebuild_snapshot`, plus a bounded
run journal in `AppState`. B1 says it outright: *"The package gains `[parser] input = "graph"`
(default `"none"`, preserving `generate-*.toml`)"*; A1 says *"the only new field is `[parser] role =
"transform"`"*; C1 says *"A transform package is a normal `format_version = 3` tvix package with
`[parser] input = "graph"`"*. This is not a defect in the trio — all three explorers independently
converged on it, and set B's own diversity analysis agrees: *"the shared core is the likely selection
regardless of winner."* The shared transform contract is the thing to build first, and picking three
proposals that disagree about it would be picking three that are each weaker.

**They differ exactly where the rule asks — in where run state lives:**

| | run state | bounded by |
|---|---|---|
| **A1** | an in-memory ring in graph-api **plus the last K=8 whole-graph snapshots** | N=256 records, K=8 snapshots, both named |
| **B1** | a `RunLedger` ring in `AppState` **plus a durable `runs.jsonl`**, deltas from `graph_vcs::diff_snapshots`, **no keyframes** — scrubbing replays deltas onto the current buffer | "bounded run ledger"; N unnamed, `runs.jsonl` unbounded |
| **C1** | a ring of the last N runs plus a `runs.local.jsonl` line, **diffs served as dense node indices** against the output revision | N unnamed, JSONL unbounded |

Three genuinely different answers to the question D3 exists to ask. That satisfies the diversity
requirement, and I am recording the first-phase seam collision here as the specification directs.

### 5.5 First reserves, and why B2 and A3 are not in the top 3

**A3 (3.366, rank 4)** is the strongest *slice* in the corpus — *"A3 is the cheapest first slice and
composes with A1 or A2 (same events, richer store)"* — and its honest-progress line is the best in
the whole submission: *"'upcoming' is a `run_scheduled` event emitted by `spawn_poll` / the
filesystem rescan timer one tick ahead, so the UI shows a real due time and never invents one."* It
loses on D3 (3): *"the past state is not retained"*, so it cannot reconstruct a past graph, which is
half of what was asked. It is the first thing to steal from when the selected trio is sliced.

**B2 (raw 3.066, rank 5 — capped to 2.000)** is the only design in the corpus that makes runs
durable, branchable, and revertable, and its *"`transform/dream` while `main` tracks the raw
import`"* model is the cleanest answer to the requester's "dream consolidation" that anyone gave —
it is the only proposal in which a failed "dream" is *one revert away*. It is capped out by three
findings, all of them fixable: **CK-008** (*"commit-per-run of a 1M-node snapshot is heavy (snapshot
dedupe by content hash mitigates)"* — a full graph per run with no retention window), **CK-009** (no
cap handling, no run status), and the **CK-019** pitfall (*"the standalone graph-api runs the same
scheduler struct in-process"*). A retention/prune policy on the branch, an `outcome` field, and
driving the schedule from `WatchPlan::Poll` is an afternoon's work. **That is a finding, not a
selection** — and B2 should be re-scored after those three edits, because on the fixed version its
D1 (4) and D3 (≥3) would put it back in contention with A1 and C1.

**The disqualified runner-up finding: A5 and B6 are out on CK-001, not on merit.** Both are the most
interesting ideas in their sets (A5's *predicted futures*; B6's *Nix never sees the data*). Both
leave the existing engine contract, which is CK-001's whole purpose. If the requester wants
predicted-future or DSL semantics, that has to be re-proposed as a layer **on top of** a
CK-001-compliant spine — not as a replacement for it.

---

## 6. Issues, in priority order

| # | priority | issue | evidence | impact | suggestion |
|---|---|---|---|---|---|
| 1 | ~~**High**~~ **RESOLVED** | The three proposal files were written to the requester's repo (`/Users/casazza/.config/nixos-config/.specs/research/`) rather than the jump-cannon `.specs/research/` they were reported written to, because the explorers ran with that `cwd` | `glob` over both repos; the jump-cannon `.specs/research/` initially held only the spec yaml | Phase 2 would have read from the wrong tree, and the artifacts were untracked in a config repo | **Done** — Main copied all three into the jump-cannon `.specs/research/`. I verified the copies are byte-identical to the originals I scored (`sha256` a: `238295f8…`, b: `1e3bbca4…`, c: `850851a5…`), so every quote and line reference in this report resolves at either path. |
| 2 | **High** | 12 of 18 proposals never say what a run looks like when a tvix cap is hit | the only complete answers are b.md:76 and c.md:74/86; the other twelve are silent | a runaway periodic transform hangs the `spawn_blocking` thread with no record — the spec's "default failure mode, not an edge case" | Make CK-009 a required field on the run record: `outcome: Failed{reason}` where `reason` is the cap message, and state that a cap breach never truncates |
| 3 | **High** | No proposal states miss/overlap/coalesce semantics | `grep -i "coalesc\|overlap\|missed\|skipp"` → zero scheduling hits across all three files | on a `WatchPlan` tick longer than the interval, runs pile up or overlap; the UI's "upcoming" becomes a lie | add one sentence: missed runs coalesce to one, no overlap, and the plan is `enum RunPlan { Manual, Every(Duration), Daily(TimeOfDay) }` evaluated by the existing tick |
| 4 | **Medium** | No proposal ships a phase list | a.md:337-338, b.md:97 and c.md:152 are the only sequencing statements | every design lands as one indivisible change, which is the failure mode D7 exists to catch | cut P1 = node-content root-cause fix + one manual run + run record; P2 = deltas; P3 = schedule; P4 = animation; P5 = provenance fields |
| 5 | **Medium** | Two thirds of the corpus omits the knowledge-map update | the string `knowledge` is absent from sets A and C entirely | AGENTS.md requires the affected notes and wikilinks in the same change | name the note in the first phase, not the last |
| 6 | **Medium** | The specification's scoring formula contradicts its own stated range | `formula: "total = sum(weight_d * score_d) / 5, giving 1.0-5.0"` — with `Σw = 1` that yields `[0.2, 1.0]` | two judges reporting on different readings would produce incomparable numbers (the ranking is unaffected) | drop the `/ 5`; keep `essential_fail_cap: 2.0` |
| 7 | **Low** | `b.md:84` proposes a branch named `transform/dream` | b.md:84 | the requester's own word in a design artifact; harmless in a proposal, not in a PR | rename to `transform/consolidate` before the change is written up |
| 8 | **Low** | Six proposals fail CK-006 solely because set A's constraint block omits panel-kit loading stores | a.md:45 vs b.md:20 and c.md:16, c.md:31 | the same omission will recur in any proposal drafted from that template | add "panel-kit `loading_store` + `LoadingGate`" to the standing constraint template |

## 7. Rules generated

Root-cause analysis on issues 3 and 4 (the two systemic, cross-task ones) — Five Whys:

- **Issue 3 (no miss/overlap semantics).** Why? → the design states a *cadence* but never asks what
  happens when a run's duration exceeds its interval. → because no checklist item in the evaluation
  specification forces the question, so drafting stops at "where does upcoming come from". → root
  cause: **a missing convention** — "a declared schedule is incomplete without a miss/overlap
  policy". Cross-task recurrence: YES, any scheduled-work design. Cross-project: YES. Frequency:
  YES. Actionable with contrastive examples: YES. Token-justified: YES.
- **Issue 4 (no phases).** Why? → each approach is written as a single coherent design. → because the
  prompt asked for six *approaches*, so the effort went into differentiation, not slicing. → root
  cause: **a missing convention** — "a design proposal for a cross-cutting feature must carry a
  phase list whose phases each land on an existing seam". Cross-task: YES. Actionable: YES.
- **Issue 1 (wrong-repo writes)** is a one-off cwd mismatch, not a rule candidate. **Issue 2** is
  already covered by CK-009 once that item is fixed in the spec. **Issues 5–8** are project-specific
  or spec-level, better fixed in the spec and the template than in a rule.

Rules to be created by the orchestrating agent (I do not hold a mandate to write to
`.claude/rules/` from this assignment, and creating rules is the meta judge's pipeline's step):
`.claude/rules/schedule-needs-miss-policy.md` and `.claude/rules/design-proposal-needs-phases.md`.

---

## 8. Self-verification

| # | question | answer | adjustment |
|---|---|---|---|
| 1 | **Evidence completeness** — did I examine all relevant sections? | Yes. I read all three proposal files end to end (a.md 340 lines including the 300-340 tail; b.md 158 lines including the four 768-char-truncated lines re-read raw; c.md 152 lines including the truncated 15/74/100/151 re-read raw), the full 314-line specification, and independently verified four load-bearing spec claims against the repo (`AGENTS.md:121-133,199-200`; `crates/tvix-wasm/src/lib.rs:27-44,53-83`; `crates/graph-api/src/server.rs:751` `spawn_blocking`; `crates/session-manager/Cargo.toml` proving `session-manager` is a real crate). | Mid-report I mis-transcribed the cap status of A1, A3 and B2, which briefly produced a wrong survivor list. I re-derived the essential-item audit line by line from each proposal's own checklist table for all 18, found **four** survivors (A1, A3, B1, C1), and rewrote §3.2, §4's affected blocks, §5.1 and the vote. B2 was then re-checked against its own table, found to carry CK-008 and CK-009 NOs, and moved back into the capped group. The final vote B1 / A1 / C1 rests on the corrected audit, and the correction did not change the top 3. |
| 2 | **Bias check** — am I influenced by length, tone, or formatting? | No. I scored D1–D8 against the quoted anchors only; the three sets are similarly sized (340/158/152 lines) and I gave the *shortest* set's A1 rank 2. I penalised verbosity nowhere and rewarded none. I also refused one available false positive: `session-manager` at b.md:80 and c.md:86 is real repo vocabulary, not overfitting, and I said so explicitly rather than counting it against CK-002. | none |
| 3 | **Anchor fidelity** — did every dimension get a two-sided anchor comparison before its number? | Yes for all 144 dimension evaluations; each row states the closer pole, the quoted artifact text on that side, and the quoted gap against the other pole. I stayed on each dimension's `contrast` line: D2 I scored on envelope-vs-new-engine (not on whether a crate exists), D3 on delta-vs-full-copy (not on durability), D7 on slicing (not on scope size), D8 on landmine specificity (not on optimism). | I revised D4 for both bitemporal proposals (A4, B4) from a drafted 5 to 4: their per-frame mechanism exceeds the anchor, but they *replace* the served wire rather than patch under the existing revision guard, which is not a same-axis respect. |
| 4 | **Comparison integrity** — is my reference result correct? | Yes, and it is independent: I derived it from the user's prompt, `AGENTS.md`, and the spec's grounding facts, not from the artifacts. Two places where the reference was sharper than most of the corpus — a `bound` field on the run record, and an output revision on the run — became the D1 gaps for A1, C1, and B2. | none |
| 5 | **Proportionality** — am I uniformly harsh or lenient? | Not uniform, and the spread is evidence-driven. B1 reaches 5 on two dimensions and 3.800; C6 scores 1 on D4. The 2.000 floor is the *specification's* cap, not my judgement, and I flagged that it erases a 2.100–3.099 raw spread — that is the proportionality caveat a reader most needs. My own scoring is 1–5 across the range with no dimension above 5 and none below 1. | I added §5.1's note that the 2.000 tie is a gate artefact, so the tie-break chain is doing the work among the fourteen capped proposals rather than their measured quality. |

## 9. Confidence

- **evidence_strength: Strong** — every YES/NO and every score carries a quoted line; the artifacts were read raw including truncated segments.
- **criterion_clarity: Clear** — the specification's anchors and contrast lines are unambiguous, and each dimension is scored on exactly one axis.
- **specification_quality: Partial** — the `formula`/`giving 1.0-5.0` contradiction (§1.3) and the absence of any cap rule for `important` items are real defects. Neither changes the top 3, because the gate structure is unambiguous and the ranking is monotone under either reading of the total.
