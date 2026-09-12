---
title: Layout regimes & capability manifests — engineering spec
status: spec (implements docs/layout-ux.md, selected 2026-09-12)
---

# Layout regimes & capability manifests — engineering spec

Normative companion to `docs/layout-ux.md` (the *why*). This file is the *what*:
schemas, algorithms, and acceptance criteria for phases 1–4. Keywords MUST/SHOULD/MAY
per RFC 2119. Ground-truth anchors are grep-verified as of 2026-09-12; where code and
this spec disagree after implementation begins, file the drift in this doc's changelog.

## 1. Definitions

- **Regime** — a named, versioned parameter base for one layout engine, declared in
  YAML under `app/configs/regimes/`.
- **Capability manifest** — a per-engine JSON document declaring every control
  dimension, its applicability predicates, and its data-ownership.
- **Snapshot state** — the five-field record computed from the current
  `GraphSnapshot`: `{ typed_bond_coverage, n_nodes, has_authored, source_kind,
  engine_kind }`.
- **Override** — a user-set control value, stored as a multiplier or absolute against
  the active regime base.
- **Parked override** — an override whose control's applicability predicate is false
  under the current regime/graph state; retained, not applied, surfaced in `why ▸`.

## 2. Regime YAML schema (normative)

One schema serves the registry and `?config=` boot presets. Parsing MUST use
`serde_yaml` with `deny_unknown_fields`. `options` MUST deserialize against the named
engine's options struct (e.g. `GpuForceOptions`); failure is a loud boot error naming
the file and field.

```yaml
id: molecular-uff            # REQUIRED. ^[a-z0-9][a-z0-9-]*$; unique across registry
schema_version: 1            # REQUIRED. Must be 1.
label: "Molecular · UFF"     # REQUIRED. Display name.
description: "..."           # OPTIONAL. One line, shown in picker tooltip.
engine: gpu-force            # REQUIRED. Engine id (local) or broker engine id (remote).
execution: live              # REQUIRED. live | one_shot.
applicability:               # REQUIRED (may be {}). Closed predicate set, ANDed:
  typed_bond_coverage: { gte: 0.5 }   # f64 in [0,1]; typed_edges / edge_count
  min_nodes: 1000                     # u64
  max_nodes: 100000                   # u64
  has_authored_positions: true        # bool
  source_kind: [obsidian, generate]   # list of SourceKind names, ORed
  engine_kind: [local, remote]        # list, ORed
options:                     # REQUIRED for local engines; MUST be omitted when
  spring_k: 0.2              #   settings_schema: broker. null = data-owned dimension.
  gravity: 0.0
  seed_mode: none
settings_schema: broker      # OPTIONAL; remote engines only. XOR with options.
controls:                    # REQUIRED (may be []). Ordered control declarations:
  - id: repulsion            #   control id; references an option field or virtual intent
    kind: multiplier         #   multiplier | toggle | enum | absolute
    label: "Repulsion (atoms)"
    range: [0.5, 2.0]        #   multiplier/absolute: [min, max]; neutral = 1.0 | default
    options: [fast, balanced]     #  enum only
    maps_to: { iterations: [100, 300] }  # enum→option mapping (one_shot quality)
presets_hidden: [fast, balanced, pretty]  # OPTIONAL. Regime ids quarantined from the picker.
```

Rules:
- R1. A regime with `execution: one_shot` MUST NOT declare live-sim controls
  (cooling, damping, halt-threshold, wake/pause).
- R2. `null` option values are permitted only for fields the engine can source from
  typed data; the resolver MUST reject a `null` that has no data source at runtime
  (fail loud, name the regime).
- R3. Registry load order: files sorted by predicate count (descending), then filename
  (ascending). First match wins. No expression language; new predicates require a
  schema_version bump.
- R4. The registry MUST contain a catch-all regime with empty `applicability`
  (`vault-small`) so resolution never fails.
- R5. A new graph class resolves to the nearest existing regime until three distinct
  sources of that class exist in the catalog (governance gate from docs/layout-ux.md §5).

## 3. Capability manifest schema (normative)

Per engine. Local engines: generated at build time from the options struct via
`schemars` + colocated `#[capability(...)]` attributes (phase 5 mechanization; phase 1
ships a hand-checked manifest for `gpu-force` only, verified by a unit test that
reflects over `GpuForceOptions` field names). Remote engines: served at
`GET /compute/engines/{id}/manifest` (phase 4 broker work).

```json
{
  "engine": "gpu-force",
  "schema_version": 1,
  "execution": "live",
  "dimensions": [
    {
      "id": "spring_len",
      "label": "Bond scale",
      "control": "multiplier",
      "range": [0.5, 2.0],
      "base": "resolved_rest_scale",
      "applicability": { "not_when": { "typed_bond_coverage_gte": 1.0 } },
      "owned_by": "uff bond table",
      "note": "governs untyped edges only when coverage is partial"
    }
  ]
}
```

- M1. `applicability` uses the same closed predicate vocabulary as regime YAML
  (§2) plus `not_when` wrapping one predicate. Evaluation happens at render time
  against the current snapshot state.
- M2. A dimension whose `not_when` fires MUST render as a collapsed-data capsule
  (`<coverage> from <owned_by> · override ▸`), never as a disabled control and never
  as a live knob — including inside Advanced (hard rule, judges' CK-202).
- M3. A dimension failing any other applicability predicate MUST NOT be constructed.
- M4. `TYPED_FORCE_SUMMARY` is `Signal<Option<(usize, usize)>>` =
  (typed_nodes, typed_edges) (`app/ui/src/graph_canvas.rs:206`). Coverage =
  `typed_edges / edge_count` (node coverage analogously). The field name
  `bonds_typed` does not exist; do not invent it.
- M5. Engines without a manifest (remote, pre-phase-4) render the generic fallback
  form: existing settings JSON, type-inferred controls, header
  `settings as declared by <engine> — applicability unknown`. No regime claims.

## 4. Resolver algorithm (normative)

```
on GraphSnapshot swap or engine switch:
    state := {
        typed_bond_coverage: typed_edges / max(edge_count, 1),
        n_nodes, has_authored, source_kind, engine_kind,
    }
    if manual_override active and regime still loads: keep
    else: regime := first regime in registry order whose applicability matches state
    effective := regime.options
    for each option field:
        if value is null: fill from typed data; if none → boot error (R2)
    for each stored override:
        if control applicable under (manifest ∧ state): apply
        else: park (retain; surface count in why ▸)
    emit: regime capsule line = "<label> · auto: <first matching predicate, humanized>"
```

Manual override clears on the next snapshot swap unless the user re-pins; `auto` in
the picker restores resolution immediately.

## 5. Persistence & migration (normative)

- P1. New key `jc_layout_v2`: `{ regime_id | "custom", overrides: { control_id:
  { kind, value } }, form_override: null }`. Overrides for `multiplier` controls store
  the multiplier, never the absolute.
- P2. One-shot migration from `jc_layout_v1` (`app/ui/src/panels/layout.rs:53,76-83`):
  legacy absolutes ÷ the `vault-large` base value for the same field → overrides under
  `vault-large`. Unknown legacy fields are dropped silently. Migration runs once; the
  v1 key is left in place for one release, then removed.
- P3. `?config=<id|path>` loads through the same schema/loader as the registry
  (one code path). `caffeine-uff.yaml` moves to `app/configs/regimes/molecular-uff.yaml`;
  its `seed_mode: none` and tuned values (`docs/molecular-force-layout.md`) are preserved
  byte-for-byte in meaning.
- P4. `LayoutPreset::{Fast,Balanced,Pretty}` and its `detect` fingerprinting
  (`layout.rs:1079-1147`) are deleted in phase 2; the presets become
  `fast.yaml / balanced.yaml / pretty.yaml` with `applicability: { engine_kind: [local] }`
  and `molecular-uff` declaring `presets_hidden` for them.

## 6. Panel contract (normative)

- UI1. The engine gallery (This Device / Compute Cluster cards) is unchanged.
- UI2. Below it: regime picker + resolution-reason line, then exactly the active
  regime's `controls`, then run state (`settled ✓ <t>` / halt meter / `last solved`),
  then Advanced ▸.
- UI3. The banner "sliders scale on top of importer-provided parameters"
  (`layout.rs:3606-3609`) is deleted in phase 1; provenance replaces it per-row.
- UI4. Styling only in `app/ui/assets/app.css` + `panel_kit::CSS`; no JS anywhere.
- UI5. Run-state copy: `settled ✓ 0.4s`, `n-tuned`, `untyped edges — global rest scale
  applies`, `uff partial — spring len governs <k> untyped edges` (copy from C3 per the
  judges' merge map).
- UI6. Static regimes (`execution: one_shot`) render Solve + quality enum + last-run
  line; no live-sim rows (R1).

## 7. Engine-truth constraints (binding)

- E1. Typed rests win outright; `spring_len` is the untyped/invalid fallback
  (`crates/graph-layouts/src/layout/algorithms/gpu_force.rs:1350-1351,3542`). No
  control may claim to scale typed rests. Global geometry scaling over typed rests is
  backlog (phase 5) and requires its own engine-change design.
- E2. Repulsion strength mixes with per-atom UFF weights via √(wᵢ×wⱼ)
  (`gpu_force.rs:1233`) — the Repulsion multiplier is honest for typed atoms.
- E3. Authored positions are preserved by `seed_mode: none` (`gpu_force.rs:96-143`).
- E4. `/compute/engines` serves `{id, display_name, description, kind}` only
  (`layout.rs:131,420`). Any remote-manifest work is cross-repo (graph-compute +
  graph-api) and phase-4.

## 8. Acceptance criteria per phase

**Phase 1 — kill the lies.** caffeine scenario (`just test browser-rust`):
(a) no `spring_len` input exists in the Layout tab when coverage = 1.0;
(b) the capsule `25/25 rests from UFF` is present; (c) the banner is absent;
(d) Fast/Balanced/Pretty are not offered; (e) molecule boots into `molecular-uff`
without `?config=` and matches the authored-seed screenshot baseline;
(f) vault graph unchanged (spring_len live, presets row present, backend enum at n≥500).
Unit test: manifest fields ≡ `GpuForceOptions` fields.

**Phase 2 — presets as data.** `LayoutPreset`/`detect` deleted; presets load from YAML;
`jc_layout_v2` migration unit-tested (legacy absolute ÷ base → multiplier); a registry
YAML edit propagates to non-overriding users.

**Phase 3 — intents.** Repulsion multiplier live (E2-honest); Spread/Stiffness/Settle
for untyped regimes; Advanced manifest-filtered; parked overrides surfaced in `why ▸`.

**Phase 4 — remote/static.** Broker serves `/compute/engines/{id}/manifest`;
`one_shot` regimes render per UI6; fallback form per M5 for unmanifested engines.

## 9. Changelog

- 2026-09-12: initial spec from docs/layout-ux.md + judge merge maps
  (.specs/reports/layout-ux-2026-09-12.{1,2,3}.md).
