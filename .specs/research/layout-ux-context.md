# Shared Context: Layout Panel UI/UX Redesign (Tree-of-Thoughts)

# Goal
Design a UI/UX pattern for Jump Cannon's **Layout panel** (the panel that selects and tunes graph layout engines) that fits ALL use cases, replacing the current broken "flat physics constants" mental model. Final output of the overall effort will be `docs/layout-ux.md`.

# The problem (measured, not hypothetical)
The panel renders one flat form of raw physics constants for every graph. For a molecular graph (caffeine, 24 atoms) with importer-typed per-bond UFF rest lengths (25/25 bonds) and per-atom repulsion weights (24/24 atoms), the audit is:
- `spring_len 53.56` slider: DEAD — 25/25 bonds carry UFF rests; spring_len only governs untyped edges (none exist). The slider lies.
- `repulsion 535.65`: live but absurdly scaled — value is `for_n_nodes(24)` vault-unit tuning colliding with ångström geometry.
- `gravity`, `cooling α/floor`, `energy halt`: vault-regime knobs, meaningless-to-harmful for a 24-atom authored structure (the working config sets gravity=0, energy_halt=0, cooling=1).
- backend Grid/BH/NS: irrelevant at 24 nodes (all exact O(n²)).
- Presets Fast/Balanced/Pretty: all vault regimes (spring_len 40/60/80) — every one wrong for a molecule.
- The banner "Molecular parameters active: 24 weights, 25 lengths — sliders scale on top" is false: sliders don't scale typed values, they collide (rest lengths win outright; repulsion mixes multiplicatively at a foreign unit scale).

Root cause: three parameter LAYERS are rendered as one flat form — L0 data-owned (UFF per-bond rests, per-atom weights, authored positions), L1 regime defaults (`for_n_nodes(n)`, `?config=` boot presets, Fast/Balanced/Pretty), L2 user intent ("spread out", "settle faster"). When L0 fully covers a dimension, the L1/L2 knob for it is dead but rendered live.

# Current implementation facts (ground truth, do not contradict)
- App: Rust + Dioxus 0.6 WASM in Tauri/browser (`app/ui/src/panels/layout.rs`, ~4100 lines). NO JavaScript/TypeScript allowed anywhere; styling only in `app/ui/assets/app.css` + `panel_kit::CSS`; palette/font: panel-kit dark theme, Courier Prime monospace.
- Engines: local `gpu-force` (GPU force sim, options struct GpuForceOptions: repulsion, spring_k, spring_len, gravity, damping, dt, steps_per_call, cooling_alpha, cooling_floor, energy_threshold, repulsion_mode Grid/BarnesHut/NegativeSampling, repulsion_radius, repulsion_samples, theta, seed_mode), geometric-gpu, remote bridge engines via graph-compute (`/compute/engines` list with per-engine settings JSON), static solvers.
- `GpuForceOptions::for_n_nodes(n)`: size-tuned spring_len/repulsion/radius/energy_threshold (cube-root scaling anchored at 10k nodes).
- Typed importer data: per-node UFF repulsion weights + per-edge UFF rest lengths flow to the engine; `TYPED_FORCE_SUMMARY` signal holds `(typed_nodes, typed_edges)` and already drives the banner. Authored positions (SDF depictions) seed the sim; `seed_mode: none` keeps them.
- `?config=<name>` boot presets: full AppState YAML served from `GET /configs` (e.g. `caffeine-uff.yaml` pins spring_len 1.4, repulsion 0.02, grid, gravity 0, seed none) applied to localStorage then page reload.
- Panel state: `PanelState { active: engine_id, settings: BTreeMap<engine_id, serde_json::Value>, seed_strategy, seed_custom }` persisted to localStorage `jc_layout_v1`.
- Use cases that must ALL fit: (1) Obsidian vault graphs 1k–100k+ nodes (the default), (2) molecular graphs with UFF-typed bonds/atoms + authored 2D/3D coordinates (SDF/SMILES importers), (3) generated graphs (Generate panel), (4) Kubernetes/OKF/httpjson importer graphs (untyped, vault-like), (5) remote engines with their own settings schemas, (6) static one-shot solvers vs live sims.

# Design vocabulary to apply (from impeccable.style doctrine)
- Purpose modes: this panel is **Operate** mode (frequent task actions) with **Read** elements (understanding what the sim is doing). Optimize for: frequent actions easy to find and carry out; state legible at a glance.
- Anti-slop rules: no nested cards, no icon-tile stacks, no eyebrow chips, no meaningless gradient accents, no decorative motion. Hierarchy/clarity/craft scores are the quality bar.
- "Calm by default. One action per screen." Labels over placeholders; provenance visible; the row that needs a nudge should ask for one.
- Distill/clarify discipline: every visible element must answer "what does the visitor come to do?"

# Constraints
- Panel is one of many dockable panels (panel-kit); width ~340–480px typical; must not require JS, new crates, or engine changes for the core pattern (engine changes only if a phase justifies them).
- Must respect the repo's "packages, not crates" philosophy: prefer configuration/data over new compiled surface.
- Deliverable of THIS phase: 6 high-level approaches per agent, NOT implementations.

# Output contract
Write your assigned file with EXACTLY 6 genuinely distinct approaches. For each: name + one-sentence summary; detailed description (2-3 paragraphs); key design decisions and rationale; trade-offs; probability (first 3 ≥0.80, last 3 <0.10 — sample diverse regions); complexity (low/medium/high); risks and failure modes. Verify diversity before finalizing. Do NOT implement. Skip all formatters/linters/test suites.
