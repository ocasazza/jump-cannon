# Solution C — Regime-as-Data Registry (expansion of C1)

**Core approach:** Layout regimes become declarative YAML packages under `app/configs/regimes/` — generalizing `caffeine-uff.yaml` from one-off boot preset to a *kind of thing*. A resolver auto-selects the regime from the loaded graph; the panel is a regime picker + provenance capsule + the small override surface each regime declares. **Remote engines and static solvers are registry entries too** (the pruning-gate fix).

---

## 1. The registry

```
app/configs/regimes/
  vault-large.yaml      vault-small.yaml
  molecular-uff.yaml    generated.yaml
  fast.yaml  balanced.yaml  pretty.yaml     ← today's hardcoded presets, migrated
  ray-force-3d.yaml                         ← a remote engine regime
  force-atlas-static.yaml                   ← a static one-shot solver regime
```

### 1.1 Unified schema (converges with the `?config` boot-preset format — judge condition 2)

One YAML schema serves both boot presets and the registry; a boot preset is simply a regime referenced by URL instead of by resolution:

```yaml
# app/configs/regimes/molecular-uff.yaml
id: molecular-uff
label: "Molecular · UFF"
description: "UFF-typed bond/atom parameters; keeps authored coordinates"
engine: gpu-force                    # engine id; remote engines name their broker engine
execution: live                      # live | one_shot
applicability:                       # ordered predicates; first match wins (judge condition 3)
  typed_bond_coverage: { gte: 0.5 }
  # closed predicate set: typed_bond_coverage, min_nodes, max_nodes,
  # has_authored_positions, source_kind, engine_kind — no expression language
options:                             # engine options, same serde shape as GpuForceOptions
  spring_len: null                   # null = data-owned (UFF rests); panel shows capsule
  spring_k: 0.2
  gravity: 0.0
  seed: "authored"
controls:                            # what the panel offers, in order
  - { id: geometry_scale, kind: multiplier, label: "Geometry scale",
      applies_to: [spring_len, typed_rests], range: [0.5, 2.0] }
  - { id: repulsion, kind: multiplier, label: "Repulsion (atoms)",
      applies_to: [repulsion_weights], range: [0.5, 2.0] }
  - { id: keep_authored, kind: toggle, label: "Keep authored 3D" }
presets_hidden: [fast, balanced, pretty]   # vault-tuned; quarantined for this regime
```

```yaml
# app/configs/regimes/ray-force-3d.yaml  — REMOTE (pruning-gate coverage)
id: ray-force-3d
label: "Ray force-3D (cluster)"
engine: ray-force-3d
execution: live
applicability: { engine_kind: remote }
settings_schema: broker              # options schema fetched from /compute/engines;
                                     # the registry entry declares NO constants of its own
controls: broker                     # panel renders the broker's schema generically
```

```yaml
# app/configs/regimes/force-atlas-static.yaml  — STATIC (pruning-gate coverage)
id: force-atlas-static
label: "ForceAtlas (one-shot)"
engine: force-atlas
execution: one_shot                  # panel renders Solve + quality, no live rows
applicability: { engine_kind: static }
options: { iterations: 300, theta: 0.8 }
controls:
  - { id: quality, kind: enum, options: [fast, balanced, pretty],
      maps_to: { iterations: [100, 300, 800] } }
```

**Schema rules:** `serde(deny_unknown_fields)` (same discipline as importer packages); `options` must deserialize against the named engine's options struct, or the registry load fails loudly at boot; `execution` drives whether live-sim rows exist at all.

### 1.2 Resolution

Ordered evaluation at snapshot swap (and engine switch):

```
for regime in registry_order:
    if regime.applicability matches snapshot state: resolve(regime)
```

`registry_order`: registry YAML files sorted by specificity (most predicates first), ties by filename. Snapshot state = `{ typed_bond_coverage, n_nodes, has_authored, source_kind, engine_kind }` — same five-field record everywhere (this is the same predicate record as Solution B's class detection; they share one implementation). Manual selection from the picker overrides until the next snapshot swap; `auto` restores resolution. The resolution result is always shown with its reason: `Molecular · UFF (auto: 25/25 bonds UFF-typed)`.

## 2. The panel

One panel, three bands (A2's structure, which the judges' merge notes favored as C1's rendering):

```
┌ Layout ────────────────────────────────┐
│ engine   [gpu-force              ▾]    │
│ regime   [Molecular · UFF        ▾]    │
│          auto: 25/25 bonds UFF-typed   │
├────────────────────────────────────────┤
│ ▸ 25/25 rests from UFF    (override)   │  ← data-owned dimension, collapsed
│ Geometry scale ─────●──── ×1.0         │  ← regime-declared controls only
│ Repulsion    ──────●── ×1.0 (atoms)    │
│ ☑ Keep authored 3D                     │
├────────────────────────────────────────┤
│ settled ✓ 0.4s · [wake]  · Advanced ▸  │
└────────────────────────────────────────┘
```

- **Band 1 — engine + regime.** Two pickers, decoupled: engine selects the algorithm; regime selects the parameter base. The regime line always carries its resolution reason.
- **Band 2 — controls.** Exactly the regime's `controls` list, no more. Data-owned dimensions (option value `null` + typed coverage) render as collapsed capsules with explicit override.
- **Band 3 — run state + Advanced.** Live regimes: settle line + wake/pause. `one_shot` regimes: Solve button + last-run line. Advanced shows the regime's full option set, data-owned entries as capsules — dead knobs unrepresentable everywhere (CK-202).
- The vault regimes (`fast/balanced/pretty` + `vault-large/small`) reproduce today's behavior exactly when resolved — today's panel is the special case where the regime declares the familiar absolute sliders.

## 3. Data flow

```
registry load (boot): parse + validate all regimes/*.yaml
        │  deny_unknown_fields; options must fit engine's serde struct
        ▼
GraphSnapshot swap ──► snapshot state record (5 fields)
        ▼
resolver: first-match regime (or manual override)
        ▼
effective options = regime.options ∘ user_overrides
        │  null + data-owned dims filled from TYPED_FORCE_SUMMARY
        ▼
panel renders regime.controls; edits write overrides
        ▼
persist: jc_layout_v2 = { regime_id | "custom", overrides: {control_id: value} }
```

Overrides are stored **by control id against the regime base**, so a regime YAML edit (tuning vault-large) propagates to every user who didn't override that control — presets become deployable data, honoring packages-not-crates.

## 4. Six use-case walkthroughs

1. **Vault 10.2k:** resolves `vault-large` (n>1000, coverage 0) → today's familiar sliders, presets row = fast/balanced/pretty (now YAML entries), `n-tuned` provenance in the regime line.
2. **Molecular caffeine:** resolves `molecular-uff` (coverage 1.0 ≥ 0.5) → the §2 panel; vault presets hidden by `presets_hidden`; spring_len null → capsule. `?config=caffeine-uff.yaml` still works — the file is now `regimes/molecular-uff.yaml`-shaped and loads through the same schema (no two formats).
3. **Generated grid:** resolves `generated` (source_kind: generate) → vault-small-like options + a `generated` note.
4. **Untyped importer graph:** resolves `vault-small`/`vault-large` by n; regime line adds `untyped edges — global rest scale applies`.
5. **Remote engine:** engine picker selects `ray-force-3d`; the matching registry entry declares `settings_schema: broker` — panel renders the broker's settings JSON generically under the header `settings as declared by ray-force-3d`; no Jump Cannon constants fabricated. Broker down → Status backoff line; cached schema marked `stale`.
6. **Static solver:** `force-atlas-static` (`execution: one_shot`) → Solve + quality enum (mapping declared in YAML) + `last solved 12:03 · 1.2s`; no live-sim rows anywhere.

## 5. Persistence & migration

- `jc_layout_v2`: `{ regime_id, overrides }`. Legacy `jc_layout_v1` absolutes migrate into `custom` regime overrides against `vault-large` (their historic base).
- Today's hardcoded `LayoutPreset::{Fast,Balanced,Pretty}` become `fast.yaml/balanced.yaml/pretty.yaml` with `applicability: { engine_kind: local }` + `presets_hidden` on molecular — the Rust enum and its detect-fingerprinting heuristic are deleted (clean cutover), replaced by regime resolution.
- `caffeine-uff.yaml` moves into the registry as `molecular-uff.yaml`; the `?config` loader accepts any registry id or path — one loader, one schema.

## 6. Kaizen phasing

1. **Increment 1:** registry schema + loader + `molecular-uff.yaml` + resolver covering only the typed-coverage predicate; panel gains regime capsule + collapsed-data rows. Today's sliders otherwise unchanged. Kills the three measured lies; `?config` converges onto the schema.
2. **Increment 2:** fast/balanced/pretty + vault-large/small migrate to YAML; LayoutPreset enum deleted; persistence migration.
3. **Increment 3:** regime-declared `controls` (multiplier kinds) replace hardcoded slider rows.
4. **Increment 4:** remote (`settings_schema: broker`) + static (`execution: one_shot`) regime entries.

## 7. Judge-condition responses

- **Pruning gate (remote/static never named):** both are first-class registry citizens with worked YAML (§1.1) and walkthroughs (§4.5–6). Remote regimes declare *no constants* — schema comes from the broker; the registry never fabricates remote truth.
- **Schema convergence (one format, not two):** boot preset = registry YAML referenced by URL; same loader, same `deny_unknown_fields` validation (§1.1, §5).
- **Predicate discipline:** closed five-field state record, ordered first-match-wins, specificity-sorted; no expression language (§1.2). Predicates shared with Solution B's class detection by design.
- **All six cases + CK-202:** explicit walkthroughs; Advanced is regime-filtered; data-owned dims are capsules everywhere.

## 8. Mergeability

C1 is the *data layer* of the pattern the judges converged on: C2's capability manifests slot in as the per-engine `controls`/applicability truth (a manifest is the engine-side half of a regime entry); B's four forms are what this registry renders for its four engine/execution combinations; B1's intent multipliers are the `kind: multiplier` controls. If synthesis picks one document as primary, C1 contributes: the YAML registry + resolver, schema convergence with `?config`, preset migration, and the override-against-regime persistence model.
