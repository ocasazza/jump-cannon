# Molecular force layout: edge forces from atoms, bonds, and ions

Status: active (2026-09). Phase 0 shipped — the SDF V3000 importer
(`charts/jump-cannon/packages/sdf.toml`) puts atoms, bonds, elements,
charges, and authored 2D coordinates into the graph, and the app seeds
the sim from those coordinates (rings render as rings). Phase 1 shipped:
bond order is a first-class edge kind end-to-end and the GPU spring
takes per-edge UFF rest lengths. Phase 2a shipped: per-node UFF
well-depth weights drive repulsion (`repulsion × √(wᵢ × wⱼ)`) on the
GPU, and Settings ▸ Layout shows when the molecular parameters are
active. Remaining: Coulomb (ions) and angle terms — "The gaps" below.

## Research references (sdgr internal)

| Source | What it contributes |
|---|---|
| `mmshare/test/schrodinger/canvasphase/mmffld/test_mmffld_bonded_terms.cpp` | **Parity test methodology, borrowed for TDD**: analytic energy vs kernel (`E = fc·(d−r₀)²` stretch, `fc·(θ−θ₀)² + fc_cubic·(θ−θ₀)³` bend), centered finite-difference force `F = −dE/dx` (h = 1e-3, abs tol 1e-4), index-reversal symmetry, displacement/angle fixtures (`{0.75,1.1,0.8}`, `{1.75,0.1,2.8}`, 30°/45°). |
| `crystal_entropy/bunsen_skills/crystal-entropy-cep/references/force_field.md` | The OPLS value chain: FFBuilder OPLSDIRs, `-OPLSDIR`/`OPLS_DIR`, SMARTS coverage check — why OPLS itself is not importable. |
| `bunsen/plugins/matsci/skills/coarse-grained-force-field-builder/SKILL.md` | Prior art for Coulomb handling (dielectric 78 DPD / 15 Martini) and nonbonded cutoffs (6 Å DPD / 12 Å Martini); parameter-treatment modes (fixed / initialize-only / seeded) as a lens vocabulary precedent. |

## Goal

For an imported molecular structure, the force-directed layout's ticks
should behave like the molecule: bond springs rest at real equilibrium bond
lengths (C–C 1.54 Å ≠ C≡C 1.20 Å), non-bonded atoms repel at van der Waals
contact, and ions attract/repel by charge sign. The edge force stops being a
uniform `spring_len` and becomes a function of the two atoms and the bond
between them.

## Why not the Schrödinger repos as flake inputs (verdict: no)

Searched `org:schrodinger` for force-field code (2026-09):

| Repo | What it is | Why it can't be an input |
|---|---|---|
| `schrodinger/mmshare` | OPLS headers (`opls_*.h`) on the canvas C++ stack (`canvas/ChmPreprocessor.hpp`) | Plumbing, not algorithms: version constants, license gates, datafile managers. No standalone symbol a Rust/wgpu engine could call; the closure is the entire mmshare C++ tree. |
| `schrodinger/desmond-gpu-src` | CUDA MD engine + msys system builder (`force_field_builder.cxx`) | A full MD engine: needs msys topology, integrators, periodic boxes, CUDA. Nothing survives extraction into a 2-D/3-D graph layout shader. |
| `schrodinger/ffld-src` | The `ffld` force-field server | Client/server parameter assignment behind the Maestro stack. |
| `schrodinger/crystal_entropy` | OPLS MD workflows; `bunsen_skills/.../force_field.md` | Documents where the actual value lives: **FFBuilder-generated OPLSDIR parameter databases + SMARTS atom typing** (`check_ff_coverage.py`), produced by multi-hour QM torsion scans. |

The crown jewel of OPLS is the parameter database and the atom typer, and
both require Maestro-grade infrastructure to assign. The *functional forms*
(harmonic bonds, Lennard-Jones, Coulomb) are textbook and fifty lines of
Rust. What a graph layout needs is element-only typing with published
parameters — that is exactly the **Universal Force Field** (Rappé et al.,
JACS 1992, 114, 10024): full periodic table, no typer, parameters from
element + hybridization only. UFF is the boring, correct choice for
layout-grade realism; OPLS4 is for free-energy accuracy we are not
computing.

## What exists today (the seams)

- **Import**: `sdf.toml` (pest) — atoms as nodes (element = canvas type AND
  tag), bonds as edges, `CHG=±n` as `cation`/`anion` tags, other atom
  `KEY=value` properties as frontmatter, and the authored 2D depiction as
  initial positions via the pest engine's optional `x`/`y` capture roles.
- **Authored positions end-to-end**: `GraphSnapshot.positions_authored` (the
  circle fallback moved into `GraphSnapshot::build` and only fires when no
  importer authored coordinates), served as `Init.positions_authored`; the
  app seeds the sim from `/graph/positions` — recentered, rescaled so the
  mean edge length matches `spring_len` (`graph_canvas::authored_positions`)
  — instead of sphere + warm-up.
- **Attribute channel**: `graph-api::attribute_resolver::resolve` already
  builds per-node `GraphAttributes` (class/coordination/mass) from lenses —
  `MassLens::Field`, `ClassLens::NodeType`, etc. — and encodes them to the
  app (`encode_proto`). This is the designed path for physics metadata;
  no graph-schema change is needed for per-node quantities.
- **Layout**: `force.wgsl` runs a uniform spring (`stretch = dist -
  params.spring_len`) over the virtualized CSR; the geometric engine
  already maps per-edge *structural strength* to per-edge rest lengths
  (`edge_strength_spread`), proving per-edge rest-length plumbing is a
  small, precedented change.

## The gaps (honest)

1. ~~**`VaultEdge` is `{source, target}` — no kind.**~~ **Shipped.**
   `VaultEdge.kind: Option<String>` (serde default); the pest engine maps
   grammar-rule labels (`edge_kind_labels`, mirroring `tag_labels`) onto
   it, and the json engine fills it from `EdgeRule.kind` /
   `EdgeListRules.kind_pointer`. `sdf.toml` splits the V3000 bond-type
   code into one rule per order → `single|double|triple|aromatic`.
2. ~~**The GPU spring has one global `spring_len`.**~~ **Shipped.**
   `force.wgsl` `spring_step` reads a per-half-edge `edge_rests` buffer
   (binding 3, group 3) aligned with `edge_neighbors`; `precompute` fills
   it from edge `rest` metadata and falls back to the global spring
3. **No Coulomb term.** Repulsion is charge-blind.
4. ~~**Per-node element parameters are bond-length-only.**~~ **Shipped
   (well depths).** `uff.rs` now also carries UFF nonbond well depths
   (`D_i`); the app attaches a per-node `repulsion` weight
   (`D_i / D_carbon`) and `force.wgsl` `force_step` mixes each pair
   with UFF's geometric-mean rule on the freed group(0) binding 8.
   Untyped nodes/graphs are weight 1.0 — byte-identical behavior.
   Per-node masses and vdW *radii* (contact distances) remain open.

## Shipped wire contract (phase 1)

Instead of the `MolecularLens`/attribute-proto route sketched below, the
shipped path is two typed-attribute endpoint pairs, symmetric for nodes
and edges:

| Endpoint | Content |
|---|---|
| `GET /graph/nodes/types` | JSON: sorted distinct node types (`meta.doctype`) + revision |
| `GET /graph/nodes/types.bin` | u32 per node (id order) indexing the table; `u32::MAX` = untyped |
| `GET /graph/edges/kinds` | JSON: sorted distinct edge kinds + revision |
| `GET /graph/edges/kinds.bin` | u32 per edge (`/graph/edges` order) indexing the table; `u32::MAX` = untyped |

The app (`graph_canvas::typed_force_params`) computes per-edge rests as
`uff::bond_rest_length(element(src), element(tgt), bond_order(kind))`
and per-node weights as `uff::repulsion_weight(element)`, revision-checks
both tables against `Init`, and hands them to `build_topology_graph` as
edge `rest` / node `repulsion` metadata. Any failure disables the
feature; a graph without bond kinds is byte-identical in behavior to
before. The vault/snapshot conversions (`graph_data_from_vault`) compute
the same values in-process for browser-local packages.

## Design: UFF-derived molecular attributes (original sketch, phase 2+)

The sketch below predates the shipped wire contract; the `MolecularLens`
route was not taken for rest lengths (wire buffers won on simplicity),
but it remains the natural path for per-node mass/vdW/charge attributes.

New module `graph-layouts/src/uff.rs` (plain data + pure functions, no I/O,
wasm-clean):

```rust
pub struct ElementParams {
    pub symbol: &'static str,
    pub atomic_mass: f32,   // amu → MassLens
    pub covalent_radius: f32,  // Å, UFF r_i
    pub vdw_distance: f32,  // Å, UFF x_i (non-bonded contact)
    pub vdw_well: f32,      // kcal/mol, UFF D_i (repulsion scale)
    pub electronegativity: f32, // UFF chi_i (r_EN correction)
}
pub fn element(symbol: &str) -> Option<&'static ElementParams>;
/// UFF eq. 2-4: r0 = r_i + r_j + r_BO(order) + r_EN(chi_i, chi_j)
pub fn bond_rest_length(a: &str, b: &str, order: BondOrder) -> f32;
pub fn bond_stiffness(order: BondOrder) -> f32; // k/2 by order, clamped
```

TDD: the `uff.rs` tests are written first, ported from mmshare's
`test_mmffld_bonded_terms.cpp` methodology (see Research references):
analytic energy vs implementation at abs tol 1e-4, centered
finite-difference force consistency (h = 1e-3), index-reversal symmetry,
same displacement/angle fixtures.

### Force terms (what changes in the sim)

| Term | Form | Data path | Change |
|---|---|---|---|
| Bond stretch | harmonic `½k(r−r₀)²`, per-edge `r₀`, `k` from UFF eq. 2-4 + order | per-edge `rest: array<f32>` buffer indexed like the CSR edges | one new binding in `force.wgsl` `spring_step`; host-side attribute encode |
| Angle (1–3) | per-triple rest angle by hybridization (sp³ 109.5°, sp² 120°, sp 180°) | angle list built at import | v2 — `angle_stiffness` already exists uniform |
| Non-bonded | vdW contact: repulsion scaled by `(x_i+x_j)/2`, well `√(D_iD_j)` | per-node radius/strength attributes (class-table precedent) | per-node attr + shader scale |
| Electrostatics | `C·q_i·q_j/r²`, damped (`ε`), opposite charges attract | per-node signed charge attr (SDF `CHG`) | one signed term in the repulsion branch |

Stability: the existing integrator constraint (`K·dt² ≲ 2`, see
`geometric.rs`) caps usable bond stiffness; UFF k values are clamped into
the stable band and the imbalance absorbed into `dt` scaling, exactly as
the dynamic-bond fields already do.

### Import-side changes

1. **`VaultEdge.kind: Option<String>`** (serde default — backward
   compatible). json engine fills from `EdgeRule.kind` (the kind it
   already declares and drops); pest gains an optional edge `kind`
   capture role; `sdf.toml` maps `bond_type` → `single|double|triple|
   aromatic`. Edges to the app stay index pairs; kind rides the existing
   per-edge attribute proto.
2. **`sdf.toml` emits `mass`/charge frontmatter** — pest `property`
   captures already cover `CHG`; atomic mass needs no import (resolver
   maps element → mass from the UFF table).
3. **`attribute_resolver` gains a `MolecularLens`** — when the graph's
   nodes are element-typed (doctype ∈ periodic table), it emits:
   per-node mass (UFF), per-node vdW radius, per-node charge (from the
   `cation`/`anion` tags or `CHG` frontmatter), and per-edge rest length +
   stiffness (UFF eq. 2-4 over endpoint elements + edge kind).

### Milestones

1. ~~`VaultEdge.kind` + json/pest fill + SDF bond kinds~~ **Done** — via
   pest `edge_kind_labels` + json `EdgeRule.kind`/`kind_pointer`; no
   proto regen was needed (kinds ride the new wire buffers, not proto).
2. ~~`uff.rs` table + per-edge rest-length buffer in `force.wgsl`~~
   **Done** (without `MolecularLens`; wire buffers instead) → the sim
   relaxes typed graphs to real bond lengths.
3. ~~Per-node well-depth repulsion~~ **Done** — `force.wgsl`
   `force_step` mixes `repulsion × √(wᵢ × wⱼ)` from the
   `node_repulsion` buffer (group 0 binding 8, the freed `mass` slot;
   force_step is now AT Chrome's 10-storage cap — see the BGL comment).
   Settings ▸ Layout shows a "molecular parameters active" hint via
   `graph_canvas::TYPED_FORCE_SUMMARY`.
4. Charge term (ions) — zwitterion carboxylate/ammonium visibly attract.
5. Angle terms (v2) — hybridization-correct ring shapes.

## Verified so far

- `cargo test -p importer` (70) — shipped `sdf.toml` parses the
  glycine-zwitterion example (`cation` + `anion` tags); pest `x`/`y`
  capture tests pin authored positions, the f32-overflow edge, and the
  zero default for packages without the roles;
  `edge_kind_labels_land_on_vault_edges` pins labeled bond orders onto
  `VaultEdge.kind` (plus duplicate-label validation).
- `cargo test -p graph-layouts` (69 lib + suites) — `uff.rs` UFF eq. 3
  anchors (C–C 1.514 / C=C 1.374 / C≡C 1.293 / aromatic 1.432 / C–H
  1.091, symmetry, case normalization, unknown-element fallback) and
  `precompute_edge_rests_align_with_neighbors` (CSR-aligned per-half-edge
  rests, metadata fallback, non-finite rejection). GPU sim tests green.
- `cargo test -p graph-api --test regressions` (23) —
  `typed_edge_discovery_endpoints_reflect_the_snapshot` pins all four
  typed endpoints: JSON tables with revision, per-node/per-edge index
  buffers (`u32::MAX` untyped sentinel), table alignment.
- Live: caffeine via `--source pest … sdf.toml` → 24 atoms / 25 bonds,
  element tags (C 8, H 10, N 4, O 2), fused 6/5-ring core from the
  authored depiction; bonds carry kinds and the sim now relaxes toward
  per-edge UFF lengths instead of one uniform `spring_len`.
- `cargo test -p graph-layouts` (71 lib) — `uff::repulsion_weight`
  anchors (C = 1.0 reference, O ≈ 0.57, Si ≈ 3.8, normalization,
  unknown fallback) and
  `precompute_node_repulsion_aligns_with_node_order` (metadata weight,
  NaN rejection, untyped 1.0 fallback). GPU sim tests green; the Rust
  browser suite (`just test browser-rust`) passes with force_step at
  the 10-storage cap.
- `just test scenarios` (`crates/test-scenarios`) — YAML CaC test bed
  `scenarios/caffeine-uff.yaml` against the committed fixture
  `charts/jump-cannon/packages/examples/sdf-caffeine.txt`: precision
  (identical reruns bit-identical on-device), accuracy (25/25 bonds
  within 30% of UFF targets at 5.4% mean; ring interior-angle sums exact
  at 720°/540°; authored planarity exact), stochastic (12 seeded
  σ=0.3 jitter runs recover the gates; p95 max bond err 61.5%), and
  robustness (4 random-ball starts stay finite and bounded — the
  angle-free field cannot fold full noise, so that gate is a
  boundedness tripwire, not an accuracy claim). Molecular-scale config:
  `spring_len ≈ mean UFF rest / repulsion 0.02 / gravity 0 / dt 0.1 /
  damping 0.9`; those options ship as the `molecular-uff` **registry
  regime** (`app/configs/regimes/molecular-uff.yaml`, embedded in the app
  bundle), which any graph with ≥50% UFF-typed bonds resolves to
  automatically — `?config=molecular-uff` pins it explicitly, and the
  retired `app/configs/caffeine-uff.yaml` boot preset is gone.
  Live boot verified (browser suite's molecular-regime scenario):
  `--source pest --importer-manifest packages/sdf.toml
  --importer-input packages/examples/sdf-caffeine.txt` with **no**
  `?config=` renders the full molecule (fused 6/5-ring core, methyl tails,
  two oxygens). The graph load seeds authored coordinates rescaled to the
  *mean typed UFF rest* when the graph carries one (otherwise
  `panels::layout::active_spring_len`) — seeding at the
  `GpuForceOptions::default()` 400 against ångström UFF rests collapsed
  the molecule to a single point, and the regime's `spring_len: null`
  declares the dimension data-owned so no slider claims to scale it.
