# High‑Level Approaches for Jump‑Cannon Transformation Runs & Live Timeline

**Goal:** Provide six distinct, self‑contained proposals that explore architectural directions for adding transformation‑run scheduling, a run‑journal, and an animated timeline view to Jump‑Cannon. Each proposal includes a concise name, summary, key design decisions, trade‑offs, estimated probability of success, implementation complexity, and primary risks.

---

## Proposal 1 – "Transform‑Package + Run‑Ledger"

**Summary** – Introduce a new **Transform** package format (TVIX `format_version = 3`, `engine = "tvix"`, `input = "graph"`). Packages are imported by `graph‑api` as a decorator that receives the current graph via a typed variable, produces a new graph, and writes a **RunLedger** entry (JSONL) next to the existing `ArcSwap<GraphSnapshot>`.

**Design decisions**
- **Package shape** mirrors existing importer packages; no new crate is added.
- **RunLedger** lives in `AppState` as a bounded ring (default 256 entries) and is persisted under `JUMP_CANNON_IMPORTER_PACKAGES_DIR`.
- **Endpoints**: `GET /runs`, `GET /runs/:id/diff`, `POST /runs/trigger`.
- **UI**: New **Runs** panel reads the ledger via `/runs` and animates upcoming runs using `prefers‑reduced‑motion`.

**Trade‑offs**
- ✅ Reuses existing VFS and TVIX injection; minimal surface area.
- ❌ Adds a persistent JSONL store (disk I/O) and a new endpoint.

**Probability of success**: 0.85 (high) – all required knobs exist in the codebase.

**Complexity**: Medium (adds a ledger struct, JSONL writer, and three endpoints).

**Risks**
- Disk‑space exhaustion if retention bound is mis‑configured.
- Need to ensure cap‑exceedance is reported (CK‑009).

---

## Proposal 2 – "In‑Memory Ring + Snapshot Diff"

**Summary** – Keep run state purely in‑memory as a ring of the last N run descriptors (default 256) and retain only the most recent K full snapshots (default 8). Diffs are generated on‑the‑fly via `graph‑api` when the UI requests `/runs/:id/diff`.

**Design decisions**
- No on‑disk store; all data lives in `ArcSwap<GraphSnapshot>` and an `Arc<RwLock<Vec<RunRecord>>>`.
- **Retention**: bounded by ring size; old entries are dropped.
- **Endpoints**: same as Proposal 1 but `/runs` returns in‑memory data only.
- **UI**: Utilises existing `FrameRing` for animation; no additional storage.

**Trade‑offs**
- ✅ Zero I/O, simpler failure handling.
- ❌ No durability across process restarts; lost runs on crash.

**Probability of success**: 0.78 – straightforward but requires careful concurrency handling.

**Complexity**: Low‑Medium (concurrency primitives, diff generation).

**Risks**
- Loss of audit trail; may violate compliance requirements.
- Must still surface cap‑exceedance (CK‑009).

---

## Proposal 3 – "Scheduler‑Driven Transform Service"

**Summary** – Deploy a lightweight scheduler (based on `tokio::time::interval`) that periodically invokes a **Transform** package on the live graph. Each tick creates a new run entry; the scheduler lives inside `graph‑api` and uses the existing `WatchPlan::Push` mechanism.

**Design decisions**
- Scheduler is a **single binary** (`graph‑api`) component; no external cron.
- Run records stored in the same **RunLedger** as Proposal 1.
- **Endpoints**: same plus `GET /schedule` to expose interval.
- **UI**: Shows next‑run ETA and animates progress.

**Trade‑offs**
- ✅ No external dependencies; deterministic cadence.
- ❌ Introduces a new timing surface; potential for overlapping runs if the transform is slow.

**Probability of success**: 0.70 – timing bugs are common; needs thorough testing.

**Complexity**: Medium (scheduler, back‑pressure handling).

**Risks**
- Overlap/coalesce semantics missing (CK‑005).
- Must enforce per‑run wall‑clock budget (CK‑009).

---

## Proposal 4 – "Hybrid Persistent + In‑Memory Store"

**Summary** – Combine a bounded on‑disk JSONL ledger (as in Proposal 1) with an in‑memory ring cache for fast UI access. The ledger provides durability; the ring gives O(1) reads for the Runs panel.

**Design decisions**
- JSONL writer appends each run; ring cache mirrors the most recent N entries.
- Retention policy: keep K full snapshots on‑disk, purge older ones.
- **Endpoints**: unchanged; UI prefers the in‑memory cache.

**Trade‑offs**
- ✅ Durability + performance.
- ❌ Increased code complexity and potential inconsistency between cache and disk.

**Probability of success**: 0.65 – higher engineering overhead.

**Complexity**: High (cache invalidation, sync logic).

**Risks**
- Cache‑disk divergence bugs.
- Must still expose cap‑exceedance and retention bounds.

---

## Proposal 5 – "Web‑Assembly Transform Worker"

**Summary** – Off‑load the transform execution to a **Wasm** worker compiled with `tvix‑wasm`. The main process streams the graph to the worker, receives the transformed graph, and records the run. This isolates heavy computation and prevents UI stalls.

**Design decisions**
- Worker runs in a separate thread via `wasmtime`.
- Communication via `serde_json` messages; no filesystem/network access inside the worker (CK‑014).
- Run ledger stored in‑memory; optional persistence via JSONL.
- UI shows a spinner while the worker runs; on failure, displays the error.

**Trade‑offs**
- ✅ UI remains responsive; sandboxed execution.
- ❌ Added runtime dependency and serialization overhead.

**Probability of success**: 0.55 – integration of Wasm runtime can be tricky.

**Complexity**: High (Wasm host, message protocol).

**Risks**
- Serialization latency for large graphs.
- Must still handle cap‑exceedance within the worker.

---

## Proposal 6 – "Event‑Sourced Transform Pipeline"

**Summary** – Model each transform as a series of **events** (`AddNode`, `RemoveEdge`, …) stored in an append‑only log (`events.log`). The UI replays events to animate the transition. The run record points to the first and last event IDs.

**Design decisions**
- Log is written to disk; UI reads incremental batches to animate.
- No full‑graph snapshot per run; diff is implicit in the event stream.
- Endpoints: `GET /runs/:id/events`.

**Trade‑offs**
- ✅ Very granular animation; minimal memory usage.
- ❌ Complex event schema; harder to reason about final graph state.

**Probability of success**: 0.45 – requires extensive new tooling.

**Complexity**: Very High (event store, replay engine, UI changes).

**Risks**
- Event ordering bugs could corrupt the graph.
- No built‑in cap handling; must be added manually.

---

## Subtasks

- [X] Generate six distinct high‑level proposals (name, summary, design decisions, trade‑offs, probability, complexity, risks).

---

**Next steps** – Review the six proposals against the checklist (CK‑001 – CK‑016). The orchestrator will select the top three based on the rubric and the diversity requirement (different run‑state locations).
