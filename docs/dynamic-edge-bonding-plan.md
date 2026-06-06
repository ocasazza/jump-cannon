# Dynamic-edge (dynamic bonding) self-assembly — design & plan

Research-backed plan (deep-research; all findings 3-0, cited to MD/SPH + patchy-particle
literature) for **dynamic edges**: the compute engine adds/removes bonds each step under a
geometric constraint, so self-assembly (chain → sheet → tube → closed vesicle) emerges from
Brownian/Langevin motion on an *evolving* graph topology.

> Status: DESIGN — awaiting sign-off before implementation. Builds on the landed
> geometric engine (Langevin thermostat, Cooke–Deserno well, director/anisotropy,
> `observe_assembly`), the Barnes-Hut octree, and the just-landed NaN guards.
> Companion to [`self-assembly-plan.md`](self-assembly-plan.md).

---

## 0. The reframe — bonds are edges; valence + angle *are* the ladder

In a graph engine, self-assembly is most naturally expressed as **dynamic topology**: nodes
form/break edges (bonds) by a proximity+compatibility constraint, and the existing force-
directed/geometric layout arranges the evolving graph. The morphology ladder maps directly
onto **coordination number (valence) + bond angle** (research, 3-0):

| valence | angle | morphology |
|---|---|---|
| 2 | 180° | **chain** |
| 3 | 120° | **honeycomb sheet** |
| 4 | 90° | square net |
| 3–4 + spontaneous curvature | — | **tube** |
| + rim line-tension / bending | — | **closed vesicle** |

Critically: **valence alone does not select morphology — the cap must carry the target
angle** (per-class `max_valence` + `target_angle`). The geometric engine already speaks this:
class-affinity = who-bonds-with-whom, coordination-angle = bond geometry. Dynamic bonding =
"promote an in-range, compatible, under-valence contact into a bond edge."

---

## 1. Neighbor search — O(n) candidate pairs (no O(n²))

- **Uniform grid / cell list**, cell size = bond cutoff `r_break`; scan the particle's cell +
  26 neighbors (27 total) for candidates → **O(n)** (NVIDIA *Particles* whitepaper). Size the
  grid to `r_break`; for soft Langevin particles relax the hard-sphere occupancy bounds and
  size per-cell buffers with headroom (open question (a)).
- **GPU build is atomics-free and deterministic** — the sort-based pipeline `calcHash →
  radix-sort → findCellStart` (count + prefix-sum scatter = one radix pass). This fits
  **WebGPU (no f32 atomics)** and is reproducible run-to-run.
- **Amortize with a Verlet skin**: keep a neighbor list, rebuild only when a particle has
  moved past half the skin (MD practice; cadence tuned empirically). For large size
  disparity an LBVH tree beats stenciled cell lists; near-equal-size dense (<2:1) cell lists
  win — our particles are ~uniform, so cell lists.
- **Share ONE spatial grid** between bonding and the Barnes-Hut far-field repulsion (build the
  grid once per step, consume it twice).

## 2. Bonding as a discrete pre-force STAGE

A new stage interleaved with integration, run every K steps, that rebuilds the dynamic edge
buffer the existing spring/angle/cohesion forces then consume:

1. (re)build the spatial grid (shared).
2. for each particle, scan 27 cells for candidate partners.
3. **bond lifecycle with hysteresis** (research, 3-0): create when `dist < r_bond` AND
   constraint holds (class/type compatible, under valence cap, optional angle ok); break when
   `dist > r_break`, with `r_break ≈ 1.2–1.5 · r_bond` so bonds don't flicker.
4. **valence cap, conflict-free** (WebGPU-safe): deterministic ordering (sorted candidate
   keys) + per-node valence counters + accept/reject, OR a **valence-conserving swap** (a bond
   pivots from a leaving partner to an attacking one; ±1, accepted-only — ~100 kBT barrier).
   For genuinely growing/shrinking valence use the discrete add/break path, not the swap.
5. emit the compacted dynamic edge buffer (stream-compaction) → becomes the CSR the force
   pass reads.

Alternative considered: fold swaps into a **three-body potential** (valence-conserving, no
topology MC) — cheaper/smoother but cannot grow/shrink valence, so it's a complement, not a
replacement.

## 3. Closure physics (the Phase-C unlock)

Research confirms what Phase C found: **open sheets/tubes do NOT close under isotropic
attraction** (you get droplets). Closure needs a **rim LINE-TENSION** and/or bending/tilt:
- The dynamic-edge model gives us the rim *for free*: **under-coordinated nodes (valence <
  cap) are the boundary**. A line-tension pulling rim nodes together drives disk → bowl →
  vesicle — a **first-order transition with hysteresis** (e.g. Noguchi: closes at tilt
  stiffness ~4, reverses ~0.5; abrupt R_g jump).
- Spontaneous curvature (head/tail asymmetry, or a target dihedral) sets vesicle size; higher
  curvature/tension + lower density → smaller vesicles. Cooke–Deserno tail-width / Noguchi
  tilt tune mechanics solvent-free (matches our existing well-width + director knobs).

## 4. Engine integration & determinism

- Implement as a stage in the `LayoutEngine` step (a new `GeometricBondingEngine` variant, or
  a bonding stage inside the geometric engine gated by a `bonding_enabled` setting) — it
  produces the dynamic edge buffer; the existing edge-spring + angle + cohesion forces are
  unchanged consumers.
- **Determinism** via the sort-based grid build + sorted candidate ordering (no atomics, no
  race-dependent results) — important for the solved-case canary methodology.
- Reuse: `observe_assembly` ALREADY computes nematic S, cluster-size, and closure — so the
  validation observables exist. The NaN guards just landed protect the churny geometry.

## 5. Phased plan

1. **P1 — CPU cell-list + bond stage** (no valence cap): neighbor grid + add/remove with
   hysteresis + class compatibility → clusters. Validate cluster-size distribution; O(n) vs
   the O(n²) scan.
2. **P2 — valence cap + bond angle** → **chains & sheets**. Per-class `max_valence` +
   `target_angle`; conflict-free cap. Validate coordination-number histogram (peaks at target
   valence) + nematic S.
3. **P3 — rim line-tension + spontaneous curvature/tilt** → **tubes & vesicles**. Validate the
   R_g closure jump + curvature/closure order parameter; reproduce the disk→bowl→vesicle
   first-order transition (hysteresis). **LANDED** (CPU). Two default-OFF knobs:
   `line_tension` (an attractive seam force between the under-coordinated *rim* nodes — those
   whose dynamic-bond valence is below their class cap; both gated on `bonding_enabled`) and
   `spont_curvature` (a director-tilt torque over the *dynamic-bond* adjacency, the curvature
   half). Canaries in `tests/geometric_solver.rs`: `p3_rim_is_the_under_coordinated_boundary`
   (rim = the geometric boundary of a bonded hex disk, valence-6 interior excluded),
   `p3_line_tension_closes_a_seeded_disk` (line-tension + curvature fold a flat bonded disk
   from closure 0.25→0.59), `p3_closure_is_hysteretic` (BISTABILITY: at one intermediate
   line-tension γ_mid the from-flat branch stays OPEN ≈0.23 while the from-folded branch stays
   CLOSED ≈0.63 — the first-order loop; rim tension does not fold a flat disk but HOLDS a
   folded one shut), `p3_spontaneous_closure_from_a_soup_is_logged_honestly`.
   **Honesty (logged, not faked):** full *stable* seal (closure ≥0.85 held as a T=0 fixed
   point) and *spontaneous* soup→vesicle closure are NOT reached in a unit-test budget — the
   open-disk/vesicle kinetic trap (and a Brownian soup condenses a dense BALL, which the
   ball-vs-shell-blind closure metric reads as "closed"). The CAPABILITY + DETECTOR are
   validated on the seeded disk + the hysteresis loop; spontaneity is logged as not reached.
4. **P4 — GPU port**: sort-based atomics-free grid build, stream-compacted edge buffer, share
   the grid with Barnes-Hut; benchmark O(n) scaling toward ~1M. **LANDED.** WGSL
   `shaders/geometric_bonding.wgsl` + driver `engines/geometric_bonding_gpu.rs`. The device runs
   the parallel work — the O(n) cell hash (`calc_hash`) and the O(n·27) candidate scan over the
   3×3×3 neighbour-cell stencil (`scan_candidates`, sort-based uniform grid, **no f32/u32
   atomics**). The host does the inherently-serial counting sort (`radix/counting-sort →
   findCellStart`, kept host-side so it is exactly reproducible), the hysteretic break of
   over-stretched bonds, and the conflict-free valence-cap accept/reject (one deterministic pass
   over the sorted candidate keys — the WebGPU-safe pattern, design §2.4). A companion
   `spring_step` kernel relaxes a seeded bond config (the dynamic-edge harmonic spring) on GPU.
   CPU↔GPU **equivalence gate** (`tests/geometric_solver.rs`, `p4_*`): the same frozen configs /
   canaries produce the *same* canonical bond set (uncapped P1, valence-2 cap, class
   compatibility, multi-rebuild hysteresis) and a valence-2 bonded chain relaxes to the same
   nearest-neighbour spacing (→ r_bond) on both backends. GPU tests SKIP cleanly (loudly) with no
   wgpu adapter. **CPU-only (gated, logged honestly):** the per-step *integration* of GPU bonds
   into `GeometricGpuEngine::step` (the single-shader force pass would need a dynamic-CSR rebuild
   each rebuild) is NOT yet wired — P4 ports + validates the bonding STAGE and the dynamic-edge
   spring as standalone, equivalence-gated GPU pipelines; the full in-engine GPU self-assembly
   loop (and grid-sharing with Barnes-Hut) is the next increment. The angle/line-tension/curvature
   *bond* terms remain CPU-only on the GPU path for now.
5. **P5 — UI + tvix presets** (ties to the YAML-state + tvix-generator features): the lipid →
   sheet → tube → sphere example states that now actually CLOSE.

Each phase is a separate, verifiable PR; CPU first (unit-testable headless), GPU after.

## 6. Validation

- **Solved-case canaries** (extend the geometric harness): valence-2 → a chain (degree
  histogram all 2 interior); valence-3 @120° → a flat honeycomb patch (S high, mean degree →3);
  rim line-tension → a seeded disk closes to a shell (closure metric crosses the bar; R_g
  jump); reverse hysteresis.
- **Order parameters** (reuse `observe_assembly`): cluster-size distribution, coordination-
  number histogram, nematic S, closure/curvature.
- **Performance**: wall-clock vs n confirming O(n) (cell list) beats the O(n²) baseline;
  steps-to-assemble budget.

## 7. Open questions (carry into implementation)

- (a) Per-cell buffer size + Verlet skin for *soft* (non-hard-sphere) Langevin particles.
- (b) Best conflict-free WebGPU edge add/remove + valence-cap pattern (sort+accept/reject vs
  swap) — prototype both on CPU first.
- (c) Bonding-vs-integration cadence (every K steps) that preserves the first-order closure.
- (d) Orientation/tilt DOFs on the single-bead engine (we have per-node directors from Phase
  A — likely reuse them as the tilt vector for line-tension/curvature).

### Sources
NVIDIA *Particles* whitepaper (GPU cell list); Howard et al. CPC 2016 + Glaser et al. CPC 2015
(HOOMD neighbor lists, scaling); Bianchi et al. PCCP 2017 + JCTC 2024 (valence-limited patchy
bonding → honeycomb/square); LAMMPS `fix bond/swap` + JCP 2024 + arXiv 1912.08569 (bond-swap /
three-body); Noguchi arXiv 1906.02419 / 1010.0389 + Cooke–Deserno cond-mat/0509218 (rim
line-tension / tilt → vesicle closure, solvent-free).
