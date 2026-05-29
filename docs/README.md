# Jump Cannon — architecture & research docs

Docs-as-code lives next to the code it describes (module-level `//!` comments,
`AGENTS.md` for the repo-wide map). This `docs/` directory is reserved for the
**cross-crate, higher-level** material that doesn't belong to any single
module — research syntheses, architecture diagrams, and design rationale.

| Doc | What it covers |
|---|---|
| [`layout-algorithms.md`](layout-algorithms.md) | Survey of GPU/distributed graph-layout algorithm families (force-directed, stress, multilevel, DR-embedding, distributed), with complexity, GPU/shard suitability, visual behavior, and citations. The "why we picked these" reference. |
| [`compute-architecture.md`](compute-architecture.md) | Current `graph-compute` state, the proposed layout-registry + wire-format refactor, and the distributed-sharding model (CSR partition + ghost-node halo exchange / BSP supersteps). ASCII diagrams. |

Both documents were seeded by a verified deep-research pass (2024–2025 + classic
references). Every load-bearing claim links to a primary source; see each doc's
**References** section.
