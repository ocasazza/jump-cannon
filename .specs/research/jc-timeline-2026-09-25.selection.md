# Pruning selection — jump-cannon transformation timeline (jc-timeline)

Date: 2026-09-25
Spec: `.specs/research/jc-timeline-pruning-spec.yaml` (meta-judge, 22 checklist items, 8 weighted dimensions, essential_fail_cap 2.0)
Proposals: 18 (6 each from ExplorerA/B/C)
Judges: 3, each applying the same spec independently

## Vote tally (ranked choice 3/2/1)

| Proposal | Name | J1 | J2 | J3 | Pts |
|---|---|---|---|---|---|
| **B1** | Transform packages + run ledger in graph-api | 1st | 1st | 1st | **9** |
| **A1** | Transform packages + in-memory run journal in graph-api | 2nd | 2nd | 3rd | **5** |
| **C1** | Transform packages as importer decorator + run ledger | 3rd | 3rd | 2nd | **4** |
| B2 | Runs as graph-vcs commits; timeline is the branch log | 3rd | — | — | 1 |

**Unanimous top-3 set** (B1, A1, C1) across all three judges; only the internal order differs. All 18 survive disqualification (0 shortfalls): the corpus clears CK-001/002/003/014/016 wholesale — the package-model law, the no-overfitting line, Rust-only, pure-tvix feasibility, and "this is a proposal not a patch".

## Consensus rationale

All three winners are the same spine seen through three lenses: **a transformation is a `format_version = 3` tvix package that takes `graph` as a declared input, applied by graph-api, recorded in a bounded run ledger, with the canvas animated from recorded deltas.** They differ in where run state lives and how the transform is applied, which is exactly the diversity axis the spec demanded.

## Recorded collision (spec: diversity_requirement)

Judge 2 (§5.4): all three land their first increment on the **same seam** — the tvix package gaining a graph input, applied as a stage of `rebuild_snapshot`. The diversity requirement is satisfied by the divergence in *run-state location*, not in the first seam: B1 keeps a bounded ring + deltas in the ledger, A1 keeps an in-memory journal with the snapshot, C1 makes the transform an importer decorator so its runs belong to the source. **The synthesizer must keep that divergence explicit rather than collapsing all three onto one run store.**

## Cross-cutting defects the judges found (feed these into expansion)

1. **CK-009 — terminal failure path is unspecified in 15-16 of 18.** The spec calls a runaway periodic transform "the default failure mode, not an edge case", yet almost nobody wrote what a user sees when a run exceeds `JC_MAX_NODES`/`JC_MAX_EDGES`, times out, or throws. A failed run must be a recorded, bounded outcome — never a crash, hang, or silently truncated graph.
2. **CK-008 — retention is unstated in 11-13 of 18.** A store introduced without a bound in the same sentence as the store.
3. **CK-015 — fail/timeout/empty-graph UI states** unspecified in ~11.
4. **CK-013 — the `charts/jump-cannon/knowledge/` map update** is named by 6 of 18; AGENTS.md requires it in the same change.
5. **CK-006 — panel-kit `loading_store` hydration** is named by only 12; set A never names it.
6. **D5 (scheduling, 13.4% weight) is capped at 3 for all 18** — not one proposal states miss/overlap/coalesce semantics for a declared run plan.
7. **D7 (incremental deliverability) ≤ 3 for all 18** — not one ships a real phase list.
8. **CK-018 was a NO for all 18** — the one pitfall nobody fell into: every set correctly distinguishes `panels/timeline.rs` (a layout scrubber) from run history.

## Spec defects reported by judges (not repaired here)

- `scoring.formula` says `sum(weight*score)/5, giving 1.0-5.0`, which is self-contradictory (weights sum to 1.0, so the sum already yields 1.0-5.0). All three judges used the 1.0-5.0 sum; ranking is monotone under either reading.
- The `essential_fail_cap 2.0` fires for 16 of 18 (CK-009), flattening them into an exact tie and pushing the real work onto the spec's tie-break chain (D1 → D3 → D6). Judges disclosed the pre-cap totals alongside.
- No consequence is defined for an `important` NO.

## Next

Phase 3 expands B1, A1, C1 with the defect list above as mandatory addressees; Phase 4 judges the three; the synthesizer combines them, keeping the run-state divergence explicit.
