---
title: Layout panel UX — capability-honest regimes
status: design (selected 2026-09-12 via tree-of-thoughts; see .specs/research/layout-ux-2026-09-12.*.md)
---

# Layout UX: regimes as data, capabilities as truth, intents as controls

The Layout tab's unit of thought changes from **physics constant** to **regime**. Named
regimes are YAML data resolved automatically from the loaded graph; engines declare
capability manifests that decide which controls exist; user-facing controls are
dimensionless intents that multiply the resolved base. Data-owned simulation dimensions
(UFF-typed bonds/atoms) are unrepresentable as live knobs — on every surface, including
Advanced.

Selected by 3/3 evaluation judges (solution A, 3.88 avg) from 18 proposals; merges the
C1 registry and C3 copy/governance per judge merge maps. Judge-verified engine facts
below are grep-anchored; where this document and the code disagree, the code wins.

## 1. The problem (measured, 2026-09-12)

The current Layout tab hardcodes `GpuForceOptions` sliders and shows them for every
engine and graph. For a UFF-typed molecule (caffeine, 24 nodes / 25 edges, typed via
`h O 1.09 / h N 1.01 / h C 1.36–1.43`):

| Lie | Mechanism | Evidence |
|---|---|---|
| Dead knob | `spring_len` slider (53.56, vault-tuned) governs only untyped/invalid edges; all 25 caffeine bonds resolve from the UFF table. The slider moves, nothing happens. | `gpu_force.rs:1350-1351` (typed rests win outright; spring_len is the untyped fallback) |
| False banner | "sliders scale on top of importer-provided parameters" is emitted iff any typed data exists — the multipliers do **not** scale typed rests | `layout.rs:3606-3609` |
| Poisoned defaults | Fast/Balanced/Pretty = spring_len 40/60/80, repulsion 600/400/200, gravity 0.3 — vault presets offered to graphs whose geometry is ångströms | `layout.rs:1079-1147` (LayoutPreset + detect fingerprinting) |

Root cause: **four silent sources of truth** — `for_n_nodes` (graph-layouts), the
`?config=` boot preset (caffeine-uff.yaml), `LayoutPreset` (app), `PanelState`
localStorage (`jc_layout_v1`). The panel displays none of them, so users cannot tell
which source is in effect, and unit scales collide (53.56 vault spring_len vs 1.36–1.43
UFF rests; repulsion 535 vs UFF weights 1.0–1.08; `n≈1500` cutoff at a 24-node graph).

User stories the panel must answer: *"my molecule is tiny/collapsed"*; *"big graph,
layout too slow"*; *"this knob does nothing"*; *"sim never stops / stops too early"*.

## 2. The pattern

Three layers, each replacing one source of silent truth:

1. **Regimes as data** (from C1): `app/configs/regimes/*.yaml` — named parameter bases
   with ordered applicability predicates. `caffeine-uff.yaml` generalizes from one-off
   boot preset to a registry entry; Fast/Balanced/Pretty migrate from hardcoded Rust to
   YAML; `LayoutPreset` + its detect heuristic are deleted (clean cutover). One YAML
   schema serves registry and `?config=` boot presets — one loader, one validation path
   (`deny_unknown_fields`, options must deserialize against the engine's options struct).
2. **Capabilities as truth** (from C2): each engine ships a capability manifest — a
   closed set of applicability predicates per control (`min_nodes`,
   `typed_bond_coverage`, `has_authored_positions`, `engine_kind`, `execution`). The
   panel constructs a control only when manifest ∧ live graph state say it is
   meaningful. Local manifests are **projections of the Rust options structs**
   (schemars + colocated capability attributes), regenerated at build — manifest/Rust
   drift is a compile error, not a runtime lie.
3. **Intents as controls** (from B1): primary controls are dimensionless multipliers
   (×0.5–×2.0 around a resolved base), never absolute constants, so the
   vault-unit/ångström collision is unrepresentable. Raw constants remain in an
   Advanced disclosure — manifest-filtered, so no dead knobs there either (judges'
   CK-202 hard rule).

Provenance is per-row and structural: a `why ▸` capsule names the resolved regime, the
resolution reason, and what the data owns; data-owned dimensions render as collapsed
capsules (`25/25 rests from UFF · override ▸`), never as live sliders.

## 3. Engine truth: what controls are honest (judge-verified)

These constraints come from grep-verified engine behavior, and they **bind the design**:

- **Typed rests win outright.** There is no engine mechanism to globally scale UFF-typed
  bond rests (`gpu_force.rs:1350-1351`, `:3542`). Therefore the molecular regime declares
  **no geometry-scale control**: at full typed coverage the capsule `25/25 rests from
  UFF` is terminal truth. A global geometry multiplier over typed rests is a *possible
  later engine change* (scaled rest in the shader mix), explicitly out of scope for the
  core pattern and tracked as backlog — the UI must not promise it.
- **Repulsion is the honest typed multiplier.** Global repulsion strength mixes with
  per-atom UFF weights via √(wᵢ×wⱼ) (`gpu_force.rs:1233`), so a Repulsion intent ×N
  scales steric spacing on typed atoms truthfully.
- **`TYPED_FORCE_SUMMARY`** is `Signal<Option<(usize, usize)>>` = (typed_nodes,
  typed_edges) (`graph_canvas.rs:206`). Coverage predicates compute
  `typed_edges / edge_count` (and node analog); documents or code asserting a
  `bonds_typed` field are wrong.
- **Authored positions** are preserved by `seed_mode: none` (not `seed: authored`;
  `gpu_force.rs:96-143`, `caffeine-uff.yaml:22`).
- **Remote engines**: `/compute/engines` today serves only
  `{id, display_name, description, kind}` (`layout.rs:131,420`). There is no settings
  schema on the wire. Remote-manifest support is **priced future broker work** (phase 4):
  until an engine serves a manifest, the remote surface is its existing settings JSON
  rendered generically under the header `settings as declared by <engine> — applicability
  unknown`. No fabricated regime claims, no vault presets.

## 4. Registry

```
app/configs/regimes/
  vault-large.yaml      vault-small.yaml      generated.yaml
  molecular-uff.yaml    fast.yaml  balanced.yaml  pretty.yaml
  force-atlas-static.yaml                       (execution: one_shot)
```

```yaml
# molecular-uff.yaml
id: molecular-uff
label: "Molecular · UFF"
engine: gpu-force
execution: live                        # live | one_shot
applicability:                         # closed predicate set; specificity-sorted,
  typed_bond_coverage: { gte: 0.5 }    # first match wins — no expression language
options:                               # must fit GpuForceOptions (serde-checked)
  spring_len: null                     # null = data-owned → capsule, not control
  spring_k: 0.2
  gravity: 0.0
  seed_mode: none                      # keep authored 3D
controls:
  - { id: repulsion, kind: multiplier, label: "Repulsion (atoms)", range: [0.5, 2.0] }
  - { id: keep_authored, kind: toggle, label: "Keep authored 3D" }
presets_hidden: [fast, balanced, pretty]     # vault-tuned; quarantined here
```

Resolution state record (shared with class detection; five fields only):
`{ typed_bond_coverage, n_nodes, has_authored, source_kind, engine_kind }`.
The regime line always shows the reason: `Molecular · UFF · auto: 25/25 bonds UFF-typed`.

## 5. Panel specification

The Layout surface lives in the unified Settings panel; the existing engine gallery
(This Device / Compute Cluster cards) stays as the engine layer. Everything below it
changes. One panel, regime-rendered; the four shapes it takes:

**Vault** (coverage < 0.5; vault/generated/untyped-importer graphs) — today's controls,
honest tags: `spring len ───●─── 53.6 · n-tuned`, presets row (now YAML regimes),
backend enum only when n ≥ 500, `untyped edges — global rest scale applies` chip for
importer graphs, `uff partial` chip + hybrid sub-line when 0 < coverage < 0.5
(`spring len governs 13 untyped edges; 12 from UFF`).

**Molecular** (coverage ≥ 0.5):

```
┌ Layout ────────────────────────────────┐
│ engine  gpu-force · 24 atoms           │
│ regime  [Molecular · UFF          ▾]   │
│         auto: 25/25 bonds UFF-typed    │
├────────────────────────────────────────┤
│ ▸ 25/25 rests from UFF    (override)   │
│ Repulsion  ──────●── ×1.0 (atoms)      │
│ ☑ Keep authored 3D                     │
├────────────────────────────────────────┤
│ settled ✓ 0.4s · [wake] · Advanced ▸   │
└────────────────────────────────────────┘
```

No spring_len slider, no presets row, no cooling/halt/backend rows (converges <1s on
24 exact pairs; halt threshold still runs internally). `why ▸` expands: element/bond
summary, resolution reason, `vault presets hidden (would clobber UFF rests)`, override
count. Advanced shows resolved UFF values read-only with per-row override toggles.

**Remote** — generic settings-JSON form + `applicability unknown` header until the
broker serves manifests (phase 4). **Static** (`execution: one_shot`) — Solve button,
quality enum with declared iteration mapping, `last solved 12:03 · 1.2s`; no live rows.

Governance (from C3): the regime set is bounded — a new graph class resolves to the
nearest existing regime until **three** distinct sources of that class exist in the
catalog; only then a bespoke regime is authored. Registry hard-capped at ~8 entries.

## 6. Six use cases

1. **Vault 1k–100k+** → `vault-large`/`vault-small` (n-tuned base, presets row, backend
   enum, Settle intent packages cooling/damping/halt).
2. **Molecular UFF + authored coords** → `molecular-uff` (§5); the three lies are
   absent by construction; "molecule too small" is answered by Repulsion (atoms) and —
   if ever needed — the backlog engine change, not a lying slider.
3. **Generated graphs** → `generated` regime (source_kind predicate) + `generated` chip.
4. **Untyped importer graphs** → vault regimes + `untyped edges` chip; base from
   `for_n_nodes`, labeled `n-tuned`.
5. **Remote engines** → remote surface (§3, §5); no Jump Cannon constants fabricated;
   manifest-serving engines upgrade to full filtering in phase 4.
6. **Static one-shot solvers** → `execution: one_shot` regimes; Solve + quality +
   last-run line alongside live-sim engines in the same gallery.

## 7. Persistence & migration

- `jc_layout_v2`: `{ regime_id | "custom", overrides: {control_id → multiplier|value} }`.
  Overrides are stored **against the regime base**, so a registry YAML edit propagates
  to every user who didn't override that control — presets become deployable data.
- Overrides whose applicability lapses after a regime switch are **parked**, surfaced in
  `why ▸` as `2 parked overrides`, and restored if the regime returns.
- Migration: legacy `jc_layout_v1` absolutes ÷ vault base → overrides under `vault-large`
  (their historic meaning). `?config=` loads through the registry schema (id or path);
  `caffeine-uff.yaml` becomes `regimes/molecular-uff.yaml`.

## 8. Kaizen phasing

1. **Kill the lies (smallest):** gpu-force manifest hand-checked against
   `GpuForceOptions`; panel gains regime capsule, collapsed-data row for full typed
   coverage, backend absence below 500 nodes, banner deleted. Registry loader +
   `molecular-uff.yaml`; resolver covers only `typed_bond_coverage`. *One increment,
   all three measured defects gone.*
2. **Presets as data:** fast/balanced/pretty + vault-large/small → YAML; `LayoutPreset`
   and detect deleted; `jc_layout_v2` migration.
3. **Intents:** multiplier controls replace absolute sliders (Repulsion first — the
   verified-honest typed multiplier; Spread/Stiffness/Settle for untyped regimes);
   Advanced disclosure manifest-filtered.
4. **Remote/static:** broker work to serve settings schemas/manifests from
   `/compute/engines` (priced, cross-repo: graph-compute + graph-api); `one_shot`
   regimes; generic fallback form until then.
5. **Backlog (engine change, needs its own justification):** global geometry scale over
   typed rests; manifest generation via schemars + capability attributes replacing the
   hand-checked manifest.

## 9. Alternatives considered

18 proposals in three leans (info-architecture A1–A6, interaction B1–B6, system C1–C6;
`.specs/research/layout-ux-2026-09-12.proposals.*.md`), pruned by 3 judges to C1/C2/C3,
expanded, then evaluated by 3 judges. Unanimous: capability manifests primary; registry
and per-class copy merged. Rejected with cause: direct-manipulation gestures (gesture
collision, B2), live A/B preview thumbnails (preview/full divergence, B3), goal-based
auto-tuning (controller stability, B4), wizard flow (panel idiom violation, A5),
spreadsheet table (density, A4), eliminating settings (regresses vault tuning, C5),
Monaco recipe-as-truth (sync complexity, C6), units normalization in engine (migration
risk, C4 — subsumed by intents without engine change).

## 10. Verification

1. *Does the molecule still boot correctly without `?config=`?* Resolver:
   coverage 25/25 ≥ 0.5 → `molecular-uff` → `seed_mode: none`, `spring_k` 0.2,
   `gravity` 0 — matches caffeine-uff.yaml's tuned values; the `?config` path loads the
   same YAML through the same schema.
2. *Can a dead knob reappear?* Controls are constructed only from manifest ∧
   graph state; Advanced is manifest-filtered; data-owned dims are capsules on every
   surface (CK-202). Browser-suite gate: caffeine scenario asserts no `spring_len`
   input exists in the Layout tab.
3. *Can vault values leak onto ångström geometry?* Overrides persist as multipliers
   against the resolved base; per-regime storage; migration divides legacy absolutes by
   the vault base. No absolute slider value crosses a regime boundary.
4. *What breaks when `/compute/engines` has no manifest?* Nothing — generic fallback
   form with `applicability unknown` header; phase 4 adds manifests without changing
   the panel contract.
5. *What breaks on a partially typed graph?* 0 < coverage < 0.5 → vault regime +
   hybrid sub-line naming exactly what spring_len governs; ≥ 0.5 → molecular regime
   with `uff partial` note. Threshold behavior is in the registry YAML, not code.
