# Solution A — Engine Capability Manifests (expansion of C2)

**Core approach:** Every layout engine ships a capability manifest — which dimensions it consumes, which the data owns, which controls are meaningful at what graph sizes — and the Layout panel renders itself from manifest ∧ live graph state. Inapplicable controls are never constructed. Provenance is shown per-dimension via a why-capsule (judge condition CK-006). Local manifests derive from existing serde schemas (judge condition CK-005).

---

## 1. Information architecture

The panel keeps its four current sections (Status / Presets / Physics / Cooling & Halt) as *visual anchors* but each row's existence and editability are computed:

```
┌ Layout ────────────────────────────────┐
│ engine   [gpu-force          ▾]        │
│ regime   Molecular · UFF · why ▸       │
├────────────────────────────────────────┤
│ ▸ 25/25 rests from UFF  (override)     │  ← collapsed dead dimension
│ Spread     ●────────────  ×1.0         │  ← manifest-emitted, dimensionless
│ Stiffness  ────●────────  ×1.0         │
│ Weight     ──────●──────  ×1.0 (atoms) │
│ ☑ Keep authored 3D                     │
├────────────────────────────────────────┤
│ Settle     ──────●──────  ×1.0         │
│ halt 0.08 (1.00) · · · · [wake] [pause]│
│ Advanced ▸  (manifest-filtered form)   │
└────────────────────────────────────────┘
```

**Row kinds, all manifest-driven:**
1. **Dimension rows** — one per manifest dimension whose applicability predicate is true. Rendered as the control type the manifest declares (`multiplier`, `toggle`, `enum`, `absolute`).
2. **Collapsed-data rows** — a dimension whose predicate is false *because data owns it* renders as a read-only capsule: `25/25 rests from UFF  (override)`. Override expands it into an editing row and marks it `custom`.
3. **Absent rows** — a dimension whose predicate is false for capability reasons (backend selector at n=24) is *not rendered at all*. No ghost, no disabled control.
4. **Advanced** — the manifest-filtered complete form: every dimension of the engine, each either a live control or a collapsed-data capsule. Dead knobs cannot appear here either (CK-202).

**Why-capsule** (`regime   Molecular · UFF · why ▸`): one line naming the resolved base — regime name + data-ownership summary. Expanding it shows the full provenance table:

```
why ▸
  base      molecular-uff (auto: 25/25 bonds UFF-typed)
  rests     UFF bond table · 25/25 edges
  weights   UFF atom weights · 24/24 nodes
  presets   fast/balanced/pretty hidden (vault-tuned)
  yours     no overrides
```

This is C1's regime capsule adopted as C2's provenance display — the merge point both sets of judges named.

## 2. The manifest

Per engine, JSON. Local engines embed it (`include_str!`); remote engines serve it alongside `/compute/engines`.

```json
{
  "engine": "gpu-force",
  "schema_version": 1,
  "dimensions": [
    { "id": "spring_len", "label": "Bond scale", "control": "multiplier",
      "range": [0.5, 2.0], "base": "resolved_rest_scale",
      "applicability": { "not_when": { "typed_bond_coverage": 1.0 } },
      "owned_by": "uff bond table",
      "note": "scales data-owned rests proportionally when partial" },
    { "id": "repulsion", "label": "Spread", "control": "multiplier",
      "range": [0.5, 2.0], "base": "resolved_repulsion",
      "applicability": { "always": true } },
    { "id": "repulsion_backend", "label": "Backend", "control": "enum",
      "options": ["exact", "barnes-hut", "ns-grid"],
      "applicability": { "min_nodes": 500 },
      "note": "meaningless below 500 nodes" },
    { "id": "gravity", "label": "Gravity", "control": "absolute",
      "range": [0.0, 0.5], "applicability": { "always": true },
      "caution": "pulls authored coordinates to origin" }
  ],
  "regimes": ["vault-large", "vault-small", "molecular-uff", "fast", "balanced", "pretty"]
}
```

**Key properties:**
- `applicability` predicates are a closed, boring set: `always`, `min_nodes`, `max_nodes`, `typed_bond_coverage` (float, 1.0 = full), `has_authored_positions`, `engine_kind`. Evaluated at render time against the current snapshot (`TYPED_FORCE_SUMMARY`, `for_n_nodes` result, source kind) — reload-safe by construction.
- `owned_by` names the data owner for the capsule text; `not_when` converts the dimension into a collapsed-data row rather than a live control.
- Controls default to **dimensionless multipliers** around a resolved base (adopting B1's write-through semantics where the manifest says `multiplier`); `absolute` survives only where the value is truly unit-free in the engine (gravity coefficient, cooling rate).

### 2.1 Deriving local manifests from serde schemas (CK-005)

No hand-written per-engine tables. Mechanically:

1. `GpuForceOptions` (and each engine's options struct) gains `schemars::JsonSchema` derives — serde schema generation already in the dependency tree.
2. A build-time generator walks the schema: each field → one manifest dimension with `id`, `label` (from doc comment), numeric range (from existing `#[validate]` / slider-range attributes where present, else a declared per-field default table *in the options struct's impl*, colocated with the type, not a new UI table).
3. Applicability and ownership can't be derived from types — they're declared once per field as `#[capability(owned_by = "uff", not_when = "typed_full")]`-style attributes next to the field in the options struct. The truth lives with the type that consumes it; the manifest is a *projection* of the Rust source, regenerated at build, so manifest/Rust drift is a compile error, not a runtime surprise.

Remote engines: same JSON shape, served from the compute broker. Remote engines without a manifest get the **honest fallback form** (judge condition 3): the engine's existing settings JSON rendered generically (one row per scalar key, type-inferred control) with a header `settings as declared by <engine> — applicability unknown`, and zero Jump Cannon regime/provenance claims. No fabricated capsules.

## 3. Data flow

```
GraphSnapshot swap (or engine switch)
        │
        ▼
capability state (memo, keyed on snapshot rev):
  typed_coverage = TYPED_FORCE_SUMMARY.bonds_typed / bonds
  n_nodes, has_authored, engine_kind
        │
        ▼
render: for each manifest dimension
  applicability ∧ capability state ──► live row | collapsed-data capsule | absent
        │
        ▼
user edit ──► write-through into engine options (multiplier × resolved base)
        │
        ▼
persistence: PanelState stores { regime_id, overrides: { dim_id → multiplier|value } }
             — never absolutes for multiplier dims, so vault values can't
               leak onto ångström geometry across reloads
```

The existing `jc_layout_v1` localStorage record migrates: absolute slider values are reinterpreted once as multipliers against the *vault* base (their historic meaning) and stored under the vault regime.

## 4. Six use-case walkthroughs

1. **Vault 10.2k nodes, live sim.** Manifest predicates: `typed_bond_coverage` 0 → spring_len renders as live Bond-scale multiplier; n=10.2k → Backend enum renders; regime capsule: `Vault · large · why ▸` (auto: n>1000). Fast/Balanced/Pretty listed in regime picker. Gravity renders with its caution chip.
2. **Molecular caffeine, 24 nodes, 25/25 UFF-typed, authored 3D.** spring_len predicate false (`not_when` full coverage) → collapsed capsule `25/25 rests from UFF`; Backend absent (n<500); regime capsule `Molecular · UFF · why ▸`; Keep-authored-3D toggle renders (`has_authored`); the false "sliders scale on top" banner is gone — replaced by per-row truth.
3. **Generated grid, 12×12.** Same as vault-small; regime `Generated · why ▸` (source kind generate). No authored positions → no keep-authored toggle.
4. **Untyped importer graph (httpjson package).** Coverage 0, unknown natural scale → regime `Imported · why ▸` with base from for_n_nodes; a note chip `untyped edges — global rest scale applies`.
5. **Remote engine with manifest.** Panel renders from the served manifest exactly as local; regime picker lists the remote engine's declared regimes; broker unreachable → Status section shows the existing backoff indicator, controls render from the last cached manifest marked `stale`.
6. **Remote engine without manifest + static solver.** Fallback generic form (settings JSON, `applicability unknown` header). Static one-shot solver: its manifest declares `"execution": "one_shot"` — the panel renders Solve + quality enum, no live-sim rows (no Settle, no pause), and run-state line says `static · last solved 12:03`.

## 5. Persistence & migration

- **Stored:** `{ engine, regime_id, overrides }` where overrides are multiplier-keyed by dimension id. Nothing about absolute constants persists for multiplier dims.
- **Migration from `jc_layout_v1`:** one-shot; legacy absolutes ÷ vault base → vault-regime overrides; molecule users keep `?config` boot path untouched.
- **Override lifecycle:** override survives regime switch only if the manifest dimension's applicability holds in the new regime's base; otherwise parked and shown in why-capsule as `2 parked overrides`.

## 6. Kaizen phasing

1. **Increment 1 (smallest):** manifest for gpu-force only, hand-checked against `GpuForceOptions`; panel renders spring_len capsule + backend absence + why-capsule. Kills the two measured lies. No other UI change.
2. **Increment 2:** multiplier controls (Spread/Stiffness/Weight/Settle) replacing absolute sliders; persistence migration.
3. **Increment 3:** build-time manifest generation from schemars + capability attributes; other local engines adopt.
4. **Increment 4:** remote manifest serving + fallback form; regime picker populated from manifest `regimes` (converging toward C1's registry if merged).

## 7. Judge-condition responses

- **CK-006 (provenance text, not just absence):** why-capsule + per-row `owned_by` capsules; override counts; parked-override surfacing.
- **CK-005 (no new compiled tables):** manifests are projections of serde schemas + colocated capability attributes; generation is build-time, drift is a compile error.
- **Remote fallback:** generic settings-JSON form with `applicability unknown` header — honest, never fabricates regime claims.
- **CK-202 (no dead knobs anywhere):** Advanced is manifest-filtered; collapsed-data rows require explicit override to edit.

## 8. Mergeability

Designed to merge with C1: C1's `regimes/*.yaml` becomes the source of the `regimes` array + the why-capsule content; C2's manifest supplies per-engine dimension truth. With C3: C3's Molecular/Vault/Remote/Static forms are simply the renderings this manifest produces under those capability states — C2 is the mechanism, C3 the observed shapes.
