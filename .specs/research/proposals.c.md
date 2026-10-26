## Proposal 3 – "Scheduler‑Driven Transform Service"

**Summary** – Deploy a lightweight scheduler (based on `tokio::time::interval`) that periodically invokes a **Transform** package on the live graph. Each tick creates a new run entry; the scheduler lives inside `graph‑api` and uses the existing `WatchPlan::Push` mechanism.

**Design decisions**
- Scheduler is a **single binary** (`graph‑api`) component; no external cron.
- Run records stored in the same **RunLedger** as Proposal 1 (bounded JSONL ring).
- **Endpoints**: `GET /runs`, `GET /runs/:id/diff`, `POST /runs/trigger`, `GET /schedule`. *Justification*: the existing `/graph/*` endpoints do not expose a recurring schedule or next‑run ETA, which are required for the UI timeline.
 **Package format** – a TVIX package with `format_version = 3`, `engine = "tvix"`, `input = "graph"`; the transform is a function of the current graph and bound variables, matching the existing importer contract.
 **Reduced‑motion variant** – the Runs panel respects `prefers‑reduced‑motion`; when disabled it shows static node‑state snapshots instead of animated transitions.
- **Cap handling**: If a transform exceeds TVIX caps, the scheduler aborts the run, records a **failed** entry with a clear error message, and continues with the next scheduled tick. The UI shows a red error badge and does not crash.

**Trade‑offs**
- ✅ No external dependencies; deterministic cadence.
- ❌ Introduces a new timing surface; potential for overlapping runs if the transform is slow.

**Probability of success**: 0.70 – timing bugs are common; needs thorough testing.

**Complexity**: Medium (scheduler, back‑pressure handling, UI loading store).

**Risks**
- Overlap/coalesce semantics missing (CK‑005).
- Must enforce per‑run wall‑clock budget (CK‑009).
- UI must avoid fabricated percentages; use explicit indeterminate when progress cannot be measured.