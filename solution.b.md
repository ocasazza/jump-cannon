# Solution B — Per-Graph-Class Forms (expansion of C3)

**Core approach:** The Layout panel switches between a bounded set of purpose-built forms — **Vault, Molecular, Remote, Static** — selected by graph class with manual override. Each form shows only what its class needs. The *pattern* is the class switch plus shared components, not one adaptive mega-form.

---

## 1. Class detection (with manual override)

Ordered, first-match-wins predicates over the current `GraphSnapshot`:

| Order | Class | Predicate |
|---|---|---|
| 1 | Remote | engine kind is remote (`--compute-url` engine selected) |
| 2 | Static | engine declares one-shot execution (no live-sim controls) |
| 3 | Molecular | `TYPED_FORCE_SUMMARY.bonds_typed / bonds ≥ 0.5` OR source kind declares chemical data |
| 4 | Vault | default (covers vault, generated, untyped importer) |

Detection result is shown as the form's title chip (`Molecular ▾`); the ▾ is the manual override menu listing all four forms + `auto`. Partially typed graphs (12/25 bonds) land in Molecular via the ≥0.5 threshold — hybrid behavior specified in §3.2.

## 2. The four forms

### 2.1 Vault form (cases 1, 3, 4)

Essentially today's panel, minus the lies that don't apply here (they're all true for vaults):

```
┌ Layout · Vault ────────────────────────┐
│ engine  [gpu-force ▾]   10,234 nodes   │
├────────────────────────────────────────┤
│ presets  [fast] [balanced●] [pretty]   │
│ spring len ─────●──── 53.6 (n-tuned)   │
│ spring k   ────●───── 60               │
│ repulsion  ──────●─── 535              │
│ gravity    ●──────── 0.05              │
├────────────────────────────────────────┤
│ cooling 0.94 · damp 0.82 · steps 16    │
│ backend [barnes-hut ▾] · seed [auto]   │
│ halt 0.08 (1.00) · · · [wake] [pause]  │
└────────────────────────────────────────┘
```

Every knob is meaningful here: untyped edges, n-scale-tuned defaults, backend matters at n≥500. Changes vs today: the `n-tuned` provenance tag on spring_len; the misleading banner absent (it was false only for typed graphs, but per-class forms make it unnecessary everywhere); untyped importer graphs get one extra chip: `untyped edges — global rest scale applies to all`.

### 2.2 Molecular form (case 2)

Purpose-built around what UFF-typed data actually leaves tunable:

```
┌ Layout · Molecular ────────────────────┐
│ engine  [gpu-force ▾]   24 atoms       │
│ UFF  25/25 bonds · 24/24 atoms · why ▸ │
├────────────────────────────────────────┤
│ Geometry scale ─────●──── ×1.0         │
│ Repulsion      ──────●── ×1.0 (atoms)  │
│ ☑ Keep authored 3D                     │
├────────────────────────────────────────┤
│ settled ✓ (halt 0.08, 0.4s)  [wake]    │
│ Advanced ▸                             │
└────────────────────────────────────────┘
```

- **Geometry scale** — the single physically meaningful global: multiplies every UFF rest proportionally (B1's Spread, specialized). This is the control that *would have* answered "why is my molecule tiny" honestly.
- **Repulsion (atoms)** — scales the UFF atom weights (steric breathing room).
- **No cooling/halt/backend/seed rows** — converges in <1s on 24 exact pairs; the run state is a one-line `settled ✓` with wake for cursor interactions.
- **`why ▸`** expands the UFF summary: element list (C, H, N, O), bond-order histogram, authored-coordinate source (`sdf.toml seed = "authored"`), preset line `vault presets hidden (would clobber UFF rests)`.
- **Advanced ▸** — read-mostly table of resolved UFF parameters (per-bond rest, per-atom weight) with per-row override toggles. No global spring_len knob exists anywhere in this form — at full coverage it is meaningless, at partial coverage Geometry scale governs the remainder and says so (`scales UFF rests + 13 untyped edges`).

### 2.3 Remote form (case 5)

The remote engine's own settings schema, rendered generically — the panel makes **no applicability claims it can't verify**:

```
┌ Layout · Remote ───────────────────────┐
│ engine  [graph-compute ▾] connected ✓  │
│ settings as declared by ray-force-3d   │
│ (applicability set by engine)          │
├────────────────────────────────────────┤
│ iterations  ────●───── 300             │
│ theta       ──────●─── 0.8             │
│ dt          ─────●──── 0.02            │
├────────────────────────────────────────┤
│ [solve]  cluster: 2×A100 · queue ok    │
└────────────────────────────────────────┘
```

Controls are type-inferred from the engine's settings JSON (number→slider within declared bounds, bool→toggle, enum→select). The header is the honesty contract: `settings as declared by <engine>` — no vault presets, no n-tuned claims, no provenance capsules the broker didn't supply. If the engine *does* serve a capability manifest (Solution A's JSON), the Remote form upgrades: same rendering path, but with applicability filtering and provenance — the forms converge upward rather than fork.

### 2.4 Static form (case 6)

```
┌ Layout · Static ───────────────────────┐
│ engine  [force-atlas ▾]  one-shot      │
│ quality [fast] [balanced●] [pretty]    │
│ [Solve]                                │
│ last solved 12:03 · 1.2s · 10,234 n    │
└────────────────────────────────────────┘
```

Solve + quality preset + last-run line. No live-sim rows (no wake/pause/settle) because there is nothing live to control. Quality maps to the solver's iteration count/theta — the only honest dial a one-shot solver has.

## 3. Cross-cutting specification

### 3.1 Shared components (bounded proliferation, CK-010)

All four forms assemble from one kit: `slider_row(label, value, tag)`, `toggle_row`, `chip(text, kind)`, `why_capsule(lines)`, `run_state_line`. Forms are ~40-line compositions, not bespoke panels. **Seventh-class gate, codified:** a new graph class uses the Vault form (the general default) until *three* distinct sources of that class exist in the catalog; only then does a bespoke form get written. Form count is hard-capped at 5 (the 4 + at most 1 experimental).

### 3.2 Hybrid-class behavior (judge condition 2)

Partially typed graphs (12/25 bonds UFF) render the **Molecular form with a hybrid section**:

```
│ UFF  12/25 bonds · why ▸               │
├────────────────────────────────────────┤
│ Geometry scale ─────●──── ×1.0         │
│   scales UFF rests + 13 untyped edges  │
```

Geometry scale's sub-line states exactly what it governs (UFF rests and untyped edges share the multiplier; per-bond UFF ratios preserved). Below 50% coverage the graph classifies as Vault with an `uff partial` chip — the Vault form's spring_len is genuinely the primary control there, and the chip links to the UFF summary.

### 3.3 Persistence & migration

`PanelState` gains `form_override: Option<LayoutForm>` (None = auto). Per-class settings persist under per-class keys (`jc_layout_v2.vault`, `.molecular`, …) so switching class never clobbers the other class's tuning — fixing today's single-slot persistence that let vault values leak onto molecules. `jc_layout_v1` migrates into `.vault` verbatim (its historic meaning).

## 4. Six use-case walkthroughs

1. **Vault 10.2k** → Vault form, balanced preset, all knobs live and honest (§2.1).
2. **Molecular caffeine** → Molecular form; the three measured lies (dead spring_len, false banner, vault presets) are absent by construction; Geometry scale is the working answer to "molecule too small" (§2.2).
3. **Generated grid 12×12** → Vault form + `generated` chip in the title; n=144 → backend row hidden (n<500 within the Vault form's own applicability rules).
4. **Untyped importer graph** → Vault form + `untyped edges` chip; defaults from for_n_nodes as today, now labeled `n-tuned`.
5. **Remote engine** → Remote form, generic rendering, honesty header; manifest-serving engines get the upgraded filtered rendering (§2.3).
6. **Static solver** → Static form: Solve + quality + last-run (§2.4).

## 5. Kaizen phasing

1. **Increment 1:** Molecular form only (the measured-defect class) + class detection with manual override; other classes keep today's panel as "Vault form". Kills all three lies with one new ~150-line form.
2. **Increment 2:** per-class persistence keys + migration; `n-tuned`/`untyped` chips in Vault form.
3. **Increment 3:** Remote form (generic JSON rendering — small, self-contained).
4. **Increment 4:** Static form; seventh-class gate documented in AGENTS.md.

## 6. Judge-condition responses

- **CK-010 (bespoke proliferation):** shared component kit; form cap at 5; codified three-sources gate for new forms; forms are compositions, not panels.
- **Hybrid classes:** ≥0.5 coverage → Molecular with hybrid section; <0.5 → Vault with `uff partial` chip; Geometry scale sub-line states exactly what it scales (§3.2).
- **Class detection with manual override:** ordered first-match-wins predicates, visible title chip, override menu with `auto` (§1).
- **All six cases:** explicit walkthroughs §4.

## 7. Mergeability

With C2: C2's manifest is the mechanism that would *generate* these forms from data instead of composing them by hand — C3 is the design target (what each class should see), C2 the engine for getting there. With C1: class detection predicates and C1's regime-resolution predicates are the same ordered-ruleset; the Molecular form's `why ▸` is C1's regime capsule. B1's intent multipliers appear as Geometry scale / Repulsion (atoms) in the Molecular form.
