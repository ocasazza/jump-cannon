# Molecular force layout: edge forces from atoms, bonds, and ions

Status: design (2026-09). Phase 0 shipped — the SDF V3000 importer
(`charts/jump-cannon/packages/sdf.toml`) puts atoms, bonds, elements, and
formal charges into the graph; everything below is the follow-up that makes
the layout physics molecular.

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

- **Import**: `sdf.toml` (pest) — atoms as nodes (element = canvas type),
  bonds as edges, `CHG=±n` as `cation`/`anion` tags, other atom
  `KEY=value` properties as frontmatter. Verified live: caffeine, 24
  atoms / 25 bonds; glycine zwitterion example parses in CI.
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

1. **`VaultEdge` is `{source, target}` — no kind.** Bond order is parsed
   by the SDF grammar and dropped. The json engine likewise declares edge
   kinds in the discovery schema and drops them at mapping.
2. **The GPU spring has one global `spring_len`.** No per-edge buffer.
3. **No Coulomb term.** Repulsion is charge-blind.
4. **Element-aware parameters don't exist anywhere** — no periodic table,
   radii, or masses in the workspace.

## Design: UFF-derived molecular attributes

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

1. `VaultEdge.kind` + json/pest fill + SDF bond kinds (core change,
   moderate blast radius: proto regen via `just app-proto`).
2. `uff.rs` table + `MolecularLens` + per-edge rest-length buffer in
   `force.wgsl` → caffeine renders with real bond lengths (ring geometry
   visibly correct: 6-ring wider than 5-ring, methyl C–H short).
3. Charge term (ions) — zwitterion carboxylate/ammonium visibly attract.
4. Angle terms (v2) — hybridization-correct ring shapes.

## Verified so far

- `cargo test -p importer` — 67 tests incl. shipped `sdf.toml` parsing the
  glycine-zwitterion example (`cation` + `anion` tags from `CHG=1`/`CHG=-1`).
- Live: `graph-api --source pest --importer-manifest sdf.toml
  --importer-input caffeine.sdf` → 24 atom nodes, 25 bond edges, elements
  as canvas types, rendered in the Dioxus canvas.
