# Hierarchical self-assembly on the geometric engine — plan

Bridges the deep-research methodology for *hierarchical molecular self-assembly*
(monomers → chains → sheets → tubes → vesicles) onto the existing generic compute
engine. Companion to [`geometric-engine-plan.md`](geometric-engine-plan.md) (the
solver this builds on) and [`compute-architecture.md`](compute-architecture.md)
(where engines plug in).

The goal: drive **emergent multi-level structure** from local stochastic dynamics
of simple particles, and **validate each emergent layer against published
benchmarks** rather than eyeballing it.

---

## 0. What already exists (the foundation is most of the way there)

The `"geometric"` engine (`engines/geometric.rs`) is already a generic molecular
force field in disguise — its own module docs say "a molecular force field is one
instantiation." It ships:

- **Bonds** — harmonic edge-length springs (`edge_rest_len`, `edge_stiffness`).
- **Angles / coordination** — preferred neighbour angle per node (180°/120°/109.5°…),
  the term that makes motifs "crystallize."
- **Non-bonded pair forces** — per-class exclusion radius + an inter-class
  affinity matrix (attract/repel), `O(n²)` with a distance cutoff.
- **Mass / gravity / inertia** + an explicit damped integrator.
- **`observe()`** — decomposed potential energy, kinetic energy, max/RMS residual
  force `‖∇E‖`. The single observable the whole validation harness rests on.
- **A solved-case harness** (`tests/geometric_solver.rs`) — closed-form canaries
  (spring → rest length, 4-cycle+90° → square, K4 → tetrahedron), a golden-master
  regression, a perf budget, and a **CPU↔GPU equivalence gate** reusing the same
  cases. This is the validation *pattern* we extend, not replace.

So we are not starting from zero. We are adding the **three missing physics
primitives** that turn a *minimizer* into a *self-assembling thermal ensemble*,
plus the **order-parameter observables** that detect each emergent layer.

---

## 1. The gap analysis (research → engine)

The deep-research report (high-confidence, 3-0 verified claims) names what's
required. Mapping each onto the current engine:

| Research requirement | In engine today? | Gap |
|---|---|---|
| **Brownian / Langevin dynamics** (thermal noise so structure *emerges* instead of freezing into the seed's local minimum) | ❌ no temperature; pure damped descent | **Langevin thermostat** (§3) |
| **Tunable-range attractive well** (Cooke–Deserno `w_c`: the single dominant knob that sets fluidity / bending stiffness, sweeps micelle→bilayer→vesicle) | ⚠️ affinity is *constant-magnitude, hard-cutoff* — explicitly "not a clean potential" | **Soft attractive well with tunable width** (§4) |
| **Directional / anisotropic interaction** (per-node orientation → sheets from *unbonded* monomers; director–curvature coupling → sphere/prolate/tube) | ⚠️ angle term acts only over *bonded* CSR neighbours; no per-node director for non-bonded pairs | **Per-node orientation + patchy pair term** (§5) |
| **Order parameters** S (nematic), cluster-size dist., curvature/genus, bending rigidity | ⚠️ only energy + residual today | **Self-assembly observables** (§6) |
| **Validation against phase diagram / scaling laws / 3–30 k_BT bending modulus** | ⚠️ canaries are zero-temperature equilibria | **Statistical-mechanics canaries** (§7) |

### Interface standardization (the user's "generic compute engine interface")

Two changes make this generic rather than geometric-only:

1. **Observables become a trait concept, not a concrete method.** Today
   `observe()` returns a geometric-specific `GeometricObservables`. Promote a
   minimal `EngineObservables` (kinetic energy, temperature, plus an optional
   `Vec<(name, f32)>` of engine-specific scalars) to an optional `LayoutEngine`
   method so the renderer's Metrics panel and the validation harness read order
   parameters from *any* backend uniformly. See [[layout-metrics-home]].
2. **Thermostat + stochastic forcing belong in `EngineCtx` / the integrator
   contract**, so any engine (FA2, SGD, geometric) can opt into temperature
   without re-implementing an RNG. Start concrete in geometric; lift to shared
   once a second engine needs it.

---

## 2. Phasing (DAG)

```
   ┌──────────────────────────────────────────────────────────────┐
   │ DONE: geometric force field + observe() + solved-case harness  │
   └───────────────────────────────┬──────────────────────────────┘
                                    │
   (T) Langevin thermostat  ◄── KEYSTONE; everything stochastic depends on it
   temperature + friction + seeded RNG; equipartition canary
                                    │
        ┌───────────────────────────┼───────────────────────────┐
        ▼ (W) tunable well          ▼ (A) anisotropy             ▼ (O) observables
   soft attractive well        per-node director +          nematic S, cluster-size
   width w_c (Cooke-Deserno)   patchy pair potential        dist., radius of gyration
        │                           │                            │
        └─────────────┬─────────────┴──────────────┬─────────────┘
                      ▼ (S) self-assembly canaries  │
              chain/sheet/vesicle statistical-mech tests, phase sweep
                      │                             │
                      ▼ (C) curvature/genus  ◄──────┘
              open sheet vs closed vesicle detection
                      │
        ┌─────────────┴─────────────┐
        ▼ (G) GPU port              ▼ (U) UI + presets
   thermostat + well + director     "self-assembly" preset family in the
   in WGSL (mirror geometric-gpu)   geometric lens panel; live S/cluster HUD
```

(T) is the keystone. (W), (A), (O) are independent once (T) lands. (S) needs
(T)+(W)+(O); (C) needs (A); (G)/(U) are scale/UX follow-ups.

---

## 3. Phase T — Langevin thermostat (KEYSTONE) — *first increment, this session*

**Goal:** give the integrator a temperature so the dynamics sample a thermal
ensemble (Brownian motion) instead of descending to the seed's nearest minimum.

The existing integrator already applies friction (`v ← damping·(v + dt·a)`). A
Langevin thermostat adds the *fluctuation* half of fluctuation–dissipation: an
Ornstein–Uhlenbeck velocity step (the "O" step of a BAOAB splitting),

```
v ← damping·(v + dt·a) + sqrt((1 − damping²)·kT / m)·ξ ,   ξ ~ N(0,1)
```

For a free particle (`a = 0`) the stationary velocity variance is exactly
`kT/m`, so `⟨½ m v²⟩ = ½ kT` per degree of freedom — **equipartition**,
independent of `dt`. That is the closed-form canary.

**Design constraints (keep existing tests green):**
- New `temperature: f32` (kT, reduced units), **default `0.0`** ⇒ noise term
  vanishes ⇒ the golden-master regression and every zero-temperature canary are
  byte-identical. The thermostat is purely additive.
- Deterministic, WASM-safe RNG: a per-`State` SplitMix64 counter + Box–Muller
  Gaussian (matches the existing `seed()` hash style; no new crate, no
  `getrandom` on the hot path). Seed from a `rng_seed` setting so temperature>0
  runs are reproducible for tests.
- Noise is injected in `integrate` (the live `step` path) only — **never** in
  `compute_forces`, so `observe()`'s residual stays the deterministic `∇E`.

**Validation (here, `cargo test -p graph-compute`):** an *equipartition canary* —
free gas (no edges, no gravity, exclusion off), `temperature = kT`, relax to
steady state, assert mean per-particle kinetic energy ≈ `1.5·kT` within a
statistical tolerance; and a `T=0` test asserting zero injected noise (golden
safe). Add to `tests/geometric_solver.rs` as a new canary row.

---

## 4. Phase W — tunable-range attractive well (Cooke–Deserno `w_c`)

**Goal:** replace the constant-magnitude affinity with a proper soft attractive
well whose **width** is a tunable knob — the research's single dominant control
parameter for the fluid-membrane regime.

- Add an attractive tail to the non-bonded pair term: WCA-style repulsion to
  `σ`, then a cosine² (Cooke–Deserno) attractive well of depth `ε` and **width
  `w_c`** out to `σ + w_c`. Gate by class-pair affinity so heads/tails differ.
- This is a *clean potential*, so fold it into `EnergyBreakdown` (the affinity
  term currently can't be, by its own docs) — restores `−∇E == force` for the
  non-bonded term and lets `observe()` track it.
- **Validation target:** sweep `w_c`; bending stiffness / area-per-lipid /
  orientational order must move *monotonically* (JCP checklist). Bending modulus
  in **3–30 k_BT** (PRE benchmark) once a bilayer forms.

## 5. Phase A — per-node orientation + patchy pair term

**Goal:** anisotropy so *unbonded* monomers assemble into sheets, and so
director–curvature coupling can drive sphere→prolate→tube.

- Add a per-node unit director (3 floats/node) integrated under rotational
  Brownian motion. Make the non-bonded well **orientation-dependent** (patchy /
  Gay–Berne-lite, or the 3-bead head+2-tail amphiphile of Cooke–Deserno).
- Resolve the director from a new source (injected, or structural from local
  PCA) on the same composable-lens pattern as `class`/`coordination`.

## 6. Phase O — self-assembly observables (order parameters)

Add to a promoted `EngineObservables` (§1):
- **Nematic order parameter `S`** = largest eigenvalue of `Q = ⟨nn⟩ − I/3`,
  scalar in [0,1]. *Do not bin it into fixed phase thresholds — that framing was
  refuted 0-3; report `S` as a continuous observable.*
- **Cluster-size distribution** (union-find over a contact cutoff) → detects the
  chain/micelle → larger-aggregate progression; the CMC analog.
- **Radius of gyration** per cluster; **lamellar spacing** from the structure
  factor / RDF peak.
- Surface in the renderer Metrics panel + pinning, reusing [[layout-metrics-home]].

## 7. Phase S — statistical-mechanics canaries (the validation methodology)

The existing canaries assert *zero-temperature* closed-form geometry. Self-
assembly needs *statistical* canaries (assert distributions / scaling, with a
fixed RNG seed for reproducibility):

- **Equipartition** (landed in Phase T): `⟨KE⟩ → 1.5 kT`. ✅
- **Ideal-chain scaling** (landed): a bonded chain (no excluded volume) at
  temperature shows `⟨R_g²⟩ ∝ N` — Flory ν=½ — validating bonds + thermostat
  together. Fitted exponent ≈ 0.93. ✅ Note the test learning: each chain in the
  ensemble needs an *independent* `rng_seed`, else the shared noise stream barely
  shrinks the variance and a single chain skews the fit.
- **Morphology ladder:** with the well + anisotropy, a parameter sweep must
  reproduce the *ordered* sequence chain → sheet → tube → vesicle (the primary
  qualitative benchmark), detected by `S` + cluster-size + curvature/genus.
- **Phase-diagram overlay:** simulated (concentration, temperature, `w_c`)
  boundaries overlaid on a literature reference (open question — needs a chosen
  reference surfactant).
- **CPU↔GPU equivalence:** statistical observables must agree across backends to
  within sampling tolerance (mirrors the existing geometric GPU gate). Note: a
  thermostat makes trajectories RNG-dependent, so equivalence is asserted on
  *ensemble averages*, not per-step positions.

---

## 8. Backlog

Tick these off as they land; each is one self-contained PR.

- [x] **Phase T** — geometric: Langevin thermostat (temperature + OU noise) + equipartition canary
- [x] **Phase W** — geometric: tunable-range cosine² attractive well (`well_depth` ε / `well_width` w_c), folded into `EnergyBreakdown::cohesion`; canaries: bound-pair → contact σ, finite range, −∇E==F, deeper-ε ⇒ faster binding, loose-cloud condensation
- [ ] **Phase A** — geometric: per-node orientation/director + patchy (orientation-dependent) pair potential
- [ ] **Phase O** — compute interface: promote `EngineObservables` onto `LayoutEngine` (nematic S, cluster-size, R_g)
- [ ] **Phase S** — geometric: self-assembly statistical canaries (ideal-chain R_g²∝N, morphology ladder)
- [ ] **Phase C** — geometric: curvature/genus from point cloud (open sheet vs closed vesicle)
- [ ] **Phase G** — geometric-gpu: port thermostat + attractive well + director to WGSL
- [ ] **Phase U** — renderer: "self-assembly" preset family + live S/cluster-size HUD
- [ ] **Open question** — pick a reference surfactant phase diagram for quantitative overlay

---

## 9. Open questions (from the research)

- Minimal anisotropy: *how little* directional structure is enough to get a
  bilayer at all on a graph engine? (Phase A is the experiment.)
- Robust curvature/genus from a discrete particle cloud **without meshing**, in
  real time (Phase C).
- Absolute reference values for secondary/tertiary levels (bilayer thickness in
  reduced units, worm-like-micelle persistence length, vesicle size dist.)
  beyond the 3–30 k_BT bending modulus.
- Concrete concentration/temperature phase boundaries for the chosen minimal
  model, to overlay quantitatively rather than only matching the qualitative
  sequence.
