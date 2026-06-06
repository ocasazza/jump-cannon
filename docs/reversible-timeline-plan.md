# Reversible / scrubbable simulation timeline — design & plan

Research-backed plan (deep-research; load-bearing findings 3-0, cited to MD, time-travel-
debugging, and game-engine literature) for a **timeline view that buffers and scrubs**
(seek backward/forward) through a running simulation — on our stochastic GPU engine
(wgpu + Langevin thermostat + dynamic edges).

> Status: DESIGN — awaiting sign-off. Builds on the existing AppState **snapshot timeline**
> (Instances panel) and the **position-frame WebSocket stream**.

---

## 0. Verdict: don't make computation reversible — make it *replayable*

True time-reversal is the **wrong primitive** for this engine (research, 3-0):
- Time-reversible integrators (velocity-Verlet/leapfrog) are only *approximately* reversible
  in floating point (FP non-associativity); exact bitwise reversal needs fixed-point/integer
  state and is **mathematically restricted to conservative Hamiltonian systems** — it
  explicitly does NOT cover velocity-dependent/dissipative forces (Stam/NVIDIA 2022; JANUS,
  Rein & Tamayo MNRAS).
- Discretized **stochastic Langevin dynamics inherently break exact time-reversal** regardless
  of integrator: finite-step Langevin *necessarily* violates microscopic reversibility and
  injects "shadow work" — the backward trajectory is non-bijective (Sivak, Chodera & Crooks,
  PRX 3 011007). Our friction + thermostat noise + displacement clamping + dynamic bond
  add/remove all compound this.

So running the sim *backward* can't reproduce history. The proven answer (rr, GGPO rollback
netcode, MD checkpoint/restart, Jolt `SaveState`) is **deterministic record + replay**.

## 1. Recommended architecture — checkpoint + deterministic re-simulation (+ delta frame buffer)

- **(a) RECORD (per step):** the deterministic *inputs* — the seeded **SplitMix64 RNG state**,
  **settings/lens edits**, and the **dynamic bond add/remove events** (a topology change log).
  This is the enabler: the dynamics are statistically reproducible but the *specific*
  trajectory is RNG-realization-dependent, so it's bit-reproducible **only if the RNG stream is
  captured/replayed** (Farago & Grønbech-Jensen).
- **(b) KEYFRAME (periodically):** snapshot full GPU state read back to CPU — positions,
  velocities, the full edge set, RNG state, settings. Interval trades **memory vs replay
  compute**. (Jolt: `SaveState` captures only what the step *modifies* (positions/velocities);
  the app layer must separately save/restore **externally-changed** state — settings AND
  dynamic edge additions/removals. Maps exactly onto our dynamic-edge engine.)
- **(c) FRAME BUFFER:** a ring buffer of the already-streamed raw f32 position frames,
  **delta-compressed** (keyframe + per-frame position deltas, the "dirty-pages frame delta"
  pattern) for *instant* visual scrub within a keyframe window — no re-sim.
- **(d) SEEK to step T:** restore the nearest keyframe ≤ T, then **headlessly re-simulate
  forward** to T using the recorded RNG stream + topology log + settings (the rr/GGPO pattern).
  For purely visual scrubbing *inside* the buffered window, just index the delta frames (cheap).

## 2. What it takes — making the engine deterministic enough

Determinism run-to-run is a **real engineering requirement, not a given**:
- **GPU FP non-associativity + unordered atomics/reductions cause genuine bit-divergence** even
  with identical inputs/seed (ORNL SC'24: ~1e-13 on a 1M-element sum; 1000 identically-seeded
  models all diverged). Fix: replace any atomic/async reductions with a **fixed hierarchical
  reduction tree** (intra-thread → warp → block → final kernel) whose combination order is
  fixed across runs (NVIDIA CCCL). **We're already well-positioned**: the repo's
  no-f32-atomics / conflict-free Jacobi design (sgd-stress-gpu) and the dynamic-edge stage's
  sort-based, atomics-free build are deterministic by construction. The audit targets are the
  Barnes-Hut **COM/energy reductions** and any `atomicAdd` accumulation.
- **Capture/replay the RNG** (the geometric thermostat's SplitMix64 `State.rng` counter — make
  it part of the keyframe/record).
- **Replay dynamic topology** deterministically (the bond add/remove log; the stage is already
  sort-based + accept/reject, so it's reproducible given the same positions + RNG).

## 3. Phased plan

1. **Phase 1 — reproducible replay (CPU-first):** capture/restore RNG state + settings +
   edge-set deltas; build `snapshot`/`restore` + a **headless single-step** primitive (the
   GGPO triad). Reuse the existing AppState snapshot timeline (Instances panel) as the keyframe
   store. Validate bit-exact round-trip on the **CPU** geometric engine (no GPU nondeterminism
   in the way).
2. **Phase 2 — harden GPU determinism:** audit + fix reductions (fixed hierarchical trees; pin
   accumulation order); confirm the GPU engines replay bit-exactly on the same device.
3. **Phase 3 — smooth scrub:** delta-compressed position-frame ring buffer on the WS stream;
   the timeline UI (scrub bar + play/pause/step + seek = nearest-keyframe-then-replay).

## 4. Validation

- **Bit-exact round-trip:** run forward N steps + snapshot; restore an earlier keyframe and
  replay forward to the same step; assert **bitwise-identical** positions / velocities / edge
  set / RNG state.
- **Same-GPU run-to-run determinism:** two identical seeded runs → bitwise-identical frames.
- **Scrub fuzz:** random seek targets, each validated against a from-genesis re-simulation.
- Start with **tolerance-based** validation, then tighten to bitwise once determinism is in.

## 5. Caveats / open questions (from the research)

- **Bitwise determinism is scoped to the SAME GPU + driver + kernel config.** Cross-device
  bit-exactness is not guaranteed by any source → assume a fixed device, or tolerate small
  drift by re-snapshotting more often (keyframes bound divergence).
- **Only the conservative subset is truly reversible** (geometric/Hamiltonian portion with the
  thermostat OFF) — not a path worth taking; replay covers everything.
- **GPU→CPU readback for full-state keyframes is the main runtime tax** and is engine-specific
  — measure it; tune keyframe interval + ring-buffer depth empirically.
- Begin with statistical/tolerance replay validation before demanding bitwise — any uncaptured
  nondeterminism (driver, async readback timing, an unfixed reduction) breaks bit-exact replay.

### Sources
Stam/NVIDIA arXiv:2207.07695 + JANUS (Rein & Tamayo MNRAS 473) — reversible integration limits;
Sivak/Chodera/Crooks PRX 3 011007 — Langevin breaks reversibility; Farago & Grønbech-Jensen
Physica A 534 — RNG-realization dependence; ORNL arXiv:2408.05148 (SC'24) + NVIDIA CCCL — GPU
reduction (non)determinism; GGPO + rr + Jolt `SaveState` + incremental-rollback — the
record/replay + delta-frame precedent.
