# EvalJudge3 scratchpad — Layout UX comparative evaluation

## Codebase verification results (grep-verified)
- TYPED_FORCE_SUMMARY: graph_canvas.rs:206 `GlobalSignal<Option<(usize,usize)>>` = (typed_nodes, typed_edges); set :279-282, :486. CONFIRMED. (Docs A/B write `.bonds_typed / bonds` — pseudo-field naming, concept consistent.)
- PanelState: layout.rs:76-83 `{active, settings: BTreeMap<String,Value>, seed_strategy, seed_custom}` at `jc_layout_v1` (layout.rs:53). CONFIRMED.
- GpuForceOptions: gpu_force.rs:173, 19 serialized fields (:400-420); for_n_nodes :334; seed_mode :234; custom serde impls. CONFIRMED.
- /compute/engines + per-engine settings JSON: layout.rs:131-133, :420. CONFIRMED.
- ?config= caffeine-uff.yaml: app/configs/caffeine-uff.yaml (spring_len 1.4, boot via ?config). CONFIRMED.
- LayoutPreset::{Fast,Balanced,Pretty} + detect fingerprint: layout.rs:1079-1147, :3584. CONFIRMED (C's "detect-fingerprinting heuristic" accurate).
- False banner: layout.rs:3606-3609 "sliders below scale on top of them". CONFIRMED.
- schemars in dependency tree: app/Cargo.lock multiple entries. CONFIRMED (A's claim accurate).

## CRITICAL engine-truth finding
- edge_rests: per-edge rest = typed metadata if valid, ELSE spring_len fallback (gpu_force.rs:1350-1351; test doc :3542 "fall back to the global spring length for untyped or invalid rests"). NO global scale factor on typed rests exists. spring_len secondary effects: seeding extent (:1290), repulsion_radius in for_n_nodes (:368).
- repulsion mixes multiplicatively: `repulsion × √(wᵢ×wⱼ)` (:1233) → global repulsion multiplier over typed weights is HONEST, no engine change.
- ⇒ Any "Geometry scale multiplies UFF rests" control is impossible without engine change; dead knob at full typed coverage.
  - A: one manifest note clause ("scales data-owned rests proportionally when partial") + override path. Non-load-bearing.
  - B: Geometry scale is the Molecular form's centerpiece, user-facing sub-line "scales UFF rests + 13 untyped edges" (§2.2, §3.2). LOAD-BEARING false claim → CK-207 NO → gate cap 2.0.
  - C: `applies_to: [spring_len, typed_rests]` in flagship regime YAML. Defect, but architecture/score does not rest on it → CK-207 YES, penalized in Control Honesty dimension.

## Scores (derived after anchor placement; see report)
- A: 4/4/4/4/4/3/4/4 → 3.88, no gates/penalties → 3.88 SELECT
- B: 4/2/3/3/3/3/4/4 → 3.16, CK-207 essential NO → cap 2.00 → REJECT
- C: 4/3/4/3/4/4/4/4 → 3.70, CK-219 pitfall YES (-0.25) → 3.45 HOLD

## Self-verification
1. Evidence completeness: all docs + spec + both ground-truth files read fully; 12 codebase claims grep-verified. OK.
2. Bias check: C is longest/most polished yet scored below A on evidence (pitfall, dense vault default, geometry-scale defect) — no length/tone reward. OK.
3. Anchor fidelity: every dimension placed against both anchors before number; A Provenance dropped 5→4 after rechecking the false scaling note against "strictly better" requirement. ADJUSTED.
4. Comparison integrity: reference result cross-checked against engine code; the geometry-scale finding verified in shader-precompute path, not assumed. OK.
5. Proportionality: B's reject is gate-mechanical from a documented load-bearing contradiction, not disposition; A vs C gap (0.43) tracks specific quoted defects. OK.
