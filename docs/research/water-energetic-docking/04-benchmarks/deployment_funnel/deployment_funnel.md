# Deployment Funnel: Multi-Resolution Pipeline Architecture

## The Hierarchical Screening Strategy

The deployment funnel proposed in the research document is:

```
Millions → Glide SP     → Thousands → Glide WS → Hundreds → Nwat-MMGBSA → Final few → ABFEP+
  (1.5s/lig)              (60s/lig)                (2 hrs/lig)                  (24 hrs/lig)
```

Each stage filters the output of the previous stage with a more expensive but more accurate method. This is the **multigrid method** applied to molecular screening: each level refines the solution at the cost of throughput.

## Jump-Cannon Engine Registry as Deployment Funnel

Jump-cannon's `EngineRegistry` implements the identical architecture for graph layout:

```rust
// engines/mod.rs: the engine registry — a deployment funnel
pub const BUILTIN_ENGINES: &[(&str, fn() -> Box<dyn LayoutEngine>)] = &[
    ("cpu-spring",      || Box::new(CpuSpringEngine::new())),     // Fastest, coarsest
    ("sgd-stress",      || Box::new(SgdStressEngine::new())),     // Medium fidelity
    ("sgd-stress-gpu",  || Box::new(SgdStressGpuEngine::new())),  // GPU stress
    ("fa2-brute",       || Box::new(Fa2BruteEngine::new())),      // Standard, O(n²)
    ("fa2-bh",          || Box::new(Fa2BhEngine::new())),         // Standard, O(n log n)
    ("geometric",       || Box::new(GeometricEngine::new())),     // High fidelity
    ("geometric-gpu",   || Box::new(GeometricGpuEngine::new())),  // GPU geometric
    ("multilevel",      || Box::new(MultilevelEngine::new(…))),   // Cascade wrapper
];
```

The `multilevel` engine is the funnel incarnate: it wraps any inner engine and cascades through coarsening levels, analogous to Glide SP (coarsest level) → Glide WS (medium levels) → Nwat-MMGBSA (finest level).

### Funnel Stage Mapping

| Docking Stage | Jump-Cannon Engine | Why |
|---|---|---|
| Glide SP (fastest, implicit) | `cpu-spring` | Spring-only, no repulsion. Runs anywhere. |
| Glide XP (medium) | `fa2-brute` / `fa2-bh` | Full force model, GPU-accelerated. |
| Glide WS (water-aware) | `geometric` | Class-aware, coordination-constrained. |
| Nwat-MMGBSA (ensemble) | `sgd-stress` | Sparse pivot sampling, ensemble-like. |
| ABFEP+ (gold standard) | `multilevel(geometric)` | Cascade through resolutions. |

## The Multilevel Wrapper as Pipeline Orchestrator

The `multilevel` engine's architecture directly mirrors the docking funnel's orchestration:

```rust
// multilevel.rs: the cascade state machine
enum CascadeState {
    Coarsening,         // Building hierarchy levels  ← Glide SP setup
    AtCoarsest,         // Solving coarsest level       ← Glide SP screening
    Descending(usize),  // Prolonging to level L        ← Glide WS refinement
    AtFinest,           // Refining at level 0          ← Nwat-MMGBSA rescoring
    Continuous,         // Forever refine at level 0    ← ABFEP+ validation
}
```

The `SweepSchedule` — which allocates more relaxation sweeps at coarse levels and fewer at fine levels — is identical to the docking funnel's allocation of compute:
- Glide SP: fast per-ligand, applied to millions → analogous to many sweeps at coarse level (cheap per node)
- Nwat-MMGBSA: expensive per-ligand, applied to hundreds → analogous to few sweeps at fine level (expensive per node)

## The GPU Multilevel as On-Device Funnel

The `multilevel.wgsl` shader (713 lines) implements the entire funnel **on-GPU, without host intervention**:

```
Host: provide CSR graph + positions buffer
GPU:  match_flags → propose → confirm_match → contract_edges
      → sort_dedupe_coarse → scan_coarse → assemble_csr
      → tiny_fr_layout (coarsest) → prolong → jitter
      → repeat level by level
Host: read positions back
```

This is the computational equivalent of a fully GPU-resident docking funnel: the host provides the input structure, and the GPU handles all stages of refinement without readback. If this architecture were applied to molecular docking, the entire Glide SP→XP→WS→MM-GBSA→FEP+ pipeline could run in a single GPU submission.

## Lean Verification of Funnel Correctness

The funnel's correctness can be stated as a **refinement property**:

```lean
-- A funnel stage transforms a set of candidates into a smaller,
-- higher-quality set. Quality is measured by a scoring function.
structure FunnelStage (α : Type) where
  filter : List α → List α
  score : α → Float
  cost : Float  -- computational cost per element

-- Each stage's output is a subset of its input, and the minimum
-- score in the output is ≥ the minimum score in the input.
def valid_stage (s : FunnelStage α) (input output : List α) : Prop :=
  output ⊆ input ∧
  (∀ x ∈ output, s.score x ≥ min_score input)

-- The full funnel is a composition of valid stages.
def funnel_valid (stages : List (FunnelStage α)) (candidates : List α) : Prop :=
  -- The final output contains only candidates that survived all stages,
  -- and the minimum score monotonically increases.
  ...
```

The jump-cannon engine registry satisfies this property: each engine in the cascade produces a layout whose stress is ≤ the previous engine's stress (monotonically improving quality) at higher computational cost.
