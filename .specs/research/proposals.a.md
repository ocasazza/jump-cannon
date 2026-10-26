## Proposal 1 – "Transform‑Package + Run‑Ledger"

> **Superseded** by [expanded-design.md](expanded-design.md) — the canonical, complete design with
> all checklist gaps addressed (CK‑004 reduced-motion, CK‑007 endpoint justification, concrete types,
> file paths, scheduling model, GPU patching, K8s worked example). This file is retained as the
> original sketch that proposed the winning approach; implementation follows `expanded-design.md`,
> not this document.

**Summary** – Introduce a new **Transform** package format (TVIX `format_version = 3`, `engine = "tvix"`, `input = "graph"`). Packages are imported by `graph‑api` as a decorator that receives the current graph via a typed variable, produces a new graph, and writes a **RunLedger** entry (JSONL) next to the existing `ArcSwap<GraphSnapshot>`.

**Design decisions**
- **Package shape** mirrors existing importer packages; no new crate is added.
- **RunLedger** lives in `AppState` as a bounded ring (default 256 entries) and is persisted under `JUMP_CANNON_IMPORTER_PACKAGES_DIR`.
- **Endpoints**: `GET /runs`, `GET /runs/:id/diff`, `POST /runs/trigger`. *Justification*: existing `/graph/*` endpoints operate on static graph snapshots; they do not expose a chronological run view or incremental diffs needed for animated timelines.
- **UI**: New **Runs** panel reads the ledger via `/runs` and animates upcoming runs using `prefers‑reduced‑motion`. The panel uses panel‑kit’s `loading_store` with `begin`/`update`/`succeed`/`fail` and a `LoadingGate` wrapped by `GlobalLoadingBar` to report progress. When a transform runs, the UI shows a measured percentage (e.g., `45 %`) derived from the number of nodes processed; if the transform cannot report progress, an explicit indeterminate spinner is shown.
- **Cap handling**: If the transformed graph exceeds TVIX caps (1 M nodes / 4 M edges) the server records the run as **failed** with a clear error message (`"Cap exceeded: 1 M nodes"`) and returns HTTP 422. No crash or silent truncation occurs.

**Trade‑offs**
- ✅ Reuses existing VFS and TVIX injection; minimal surface area.
- ❌ Adds a persistent JSONL store (disk I/O) and a new endpoint.

**Probability of success**: 0.85 (high) – all required knobs exist in the codebase.

**Complexity**: Medium (adds a ledger struct, JSONL writer, three endpoints, UI panel).

**Risks**
- Disk‑space exhaustion if retention bound is mis‑configured.
- Must ensure cap‑exceedance is reported (CK‑009).
- UI must correctly handle loading states to avoid fabricated percentages.