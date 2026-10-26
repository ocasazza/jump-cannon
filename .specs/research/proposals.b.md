## Proposal 2 – "In‑Memory Ring + Snapshot Diff"

**Summary** – Keep run state purely in‑memory as a ring of the last N run descriptors (default 256) and retain only the most recent K full snapshots (default 8). Diffs are generated on‑the‑fly via `graph‑api` when the UI requests `/runs/:id/diff`.

**Design decisions**
- No on‑disk store; all data lives in `ArcSwap<GraphSnapshot>` and an `Arc<RwLock<Vec<RunRecord>>>` ring.
- **Retention**: bounded by ring size; old entries are dropped automatically.
- **Endpoints**: `GET /runs`, `GET /runs/:id/diff`, `POST /runs/trigger`. *Justification*: the existing `/graph/*` endpoints cannot return a chronological list of runs or incremental diffs; a dedicated endpoint is required for the UI timeline.
- **UI**: The **Runs** panel uses panel‑kit’s `loading_store` with `begin`/`update`/`succeed`/`fail` and a `LoadingGate`. Progress is reported as a measured percentage based on the number of nodes processed; if the transform does not expose progress, an explicit indeterminate state is displayed.
- **Cap handling**: When a transform would exceed TVIX caps, the server aborts the run, records a **failed** entry in the ring with the error message, and returns HTTP 422. The UI shows a red error badge on the run entry.

 **Package format** – a TVIX package with `format_version = 3`, `engine = "tvix"`, `input = "graph"`; the transform is a function of the current graph and bound variables, matching the existing importer contract.
 **Reduced‑motion variant** – the Runs panel respects `prefers‑reduced‑motion`; when disabled it shows static node‑state snapshots instead of animated transitions.
- ✅ Zero I/O, simpler failure handling.
- ❌ No durability across process restarts; runs are lost on crash.

**Probability of success**: 0.78 – straightforward but requires careful concurrency handling.

**Complexity**: Low‑Medium (concurrency primitives, diff generation, UI loading store).

**Risks**
- Loss of audit trail; may violate compliance requirements.
- Must still surface cap‑exceedance (CK‑009).
- UI must avoid fabricated percentages; use explicit indeterminate when unavailable.