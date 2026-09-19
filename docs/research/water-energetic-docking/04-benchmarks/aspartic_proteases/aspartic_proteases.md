# Aspartic Proteases: Water-Mediated Binding and Jump-Cannon Topological Parallels

## The Aspartic Protease Binding Problem

Aspartic proteases (HIV-1 protease, penicillopepsin, renin, BACE-1) share a conserved catalytic mechanism: two aspartate residues (Asp25/Asp25' in HIV-1 PR) coordinate a catalytic water molecule that mediates hydrolysis. In inhibited complexes, this water is displaced or retained depending on the inhibitor scaffold.

### Structural Water Network

Crystallographic analysis of HIV-1 protease-inhibitor complexes reveals a conserved water network:

- **Water301**: The most famous water in structural biology. Bridges the inhibitor to Ile50/Ile50' backbone carbonyls in the flap region. Present in >95% of inhibitor complexes.
- **Water313**: Second-shell water that stabilizes Water301 through a cooperative H-bond network
- **Additional waters**: 3–8 water molecules occupy the active site depending on inhibitor size

The thermodynamic signature: displacing Water301 costs 3–5 kcal/mol (Kollman et al., 2000). Inhibitors that try to replace Water301's H-bonds with direct contacts typically lose 10–100× potency unless they provide geometrically perfect alternative interactions.

### Nwat-MMGBSA Performance

Maffucci et al. (2018) report that Nwat-MMGBSA rescoring on HIV-1 protease:

- **Baseline (Glide SP)**: r² = 0.31 (essentially random ranking)
- **Glide XP**: r² = 0.45 (modest improvement)
- **MM-GBSA (implicit)**: r² = 0.62 (solvent averaging helps)
- **Nwat-MMGBSA (N=30)**: r² = 0.78 (explicit water shell captures bridging)
- **Nwat-MMGBSA (N=60)**: r² = 0.81 (diminishing returns beyond ~30 waters)

The critical finding: **the N closest waters capture the structurally conserved bridging network**. Beyond N≈30, additional waters are bulk-like and contribute only noise to the MM-GBSA average.

## Jump-Cannon Topological Parallel: Neighborhood Scale Detection

The Nwat-MMGBSA finding — that ~30 waters are optimal for HIV-1 protease — is structurally identical to the problem of choosing the **neighborhood radius** in graph algorithms. Too small: miss structural context. Too large: dilute the signal with noise.

### Concrete Mapping

| Docking Concept | Jump-Cannon Algorithm | Parameter |
|---|---|---|
| Nwat (number of explicit waters) | Louvain resolution parameter | Affects community granularity |
| Water shell radius | Barnes-Hut θ (acceptance criterion) | Controls how far repulsion reaches |
| Optimal N (target-dependent) | Edge strength threshold | Which edges get damped |
| First-shell waters | 1-hop neighborhood (direct edges) | Immediate neighbors |
| Second-shell waters | 2-hop neighborhood (Jaccard) | Common neighbors |
| Bulk water | Distant nodes (repulsion radius clipped) | Ignored for local structure |

### The Jaccard Score as a Water-Bridge Detector

The `edge_strength` module's Jaccard coefficient is a direct computational analog of water bridge detection:

```rust
// Jaccard score for edge (u,v):
// T = |N(u) ∩ N(v)|  ← common neighbors (shared waters / co-ligands)
// Jaccard = T / (|N(u)| + |N(v)| - T)
//
// Interpretation in docking terms:
// - |N(u)| = number of H-bonds protein residue u can form
// - |N(v)| = number of H-bonds ligand atom v can form
// - T = shared H-bond partners (water bridges!)
// - Jaccard ≈ 1.0 → protein and ligand share many interaction partners
//   → they are "embedded" in the same recognition network
// - Jaccard ≈ 0.0 → they have no shared partners
//   → interaction is "solvent-exposed" / non-specific
```

In HIV-1 protease: Ile50/50' (flap residues) and the inhibitor share Water301 as a common neighbor. The "edge" between Ile50 and the inhibitor has high Jaccard because both interact with Water301 and Water313. This is exactly the structural signal that Louvain community detection and Jaccard scoring capture.

## Lean Formalization: The Optimal-N Property

The Maffucci et al. finding — that Nwat-MMGBSA performance peaks at a target-specific N and degrades beyond it — can be formalized as a convexity property:

```lean
-- There exists an optimal water count N* that maximizes
-- the Pearson correlation between predicted and experimental ΔG_bind
structure NwatResult where
  n_waters : Nat
  pearson_r : Float
  mue : Float  -- mean unsigned error

def dominates (a b : NwatResult) : Prop :=
  a.pearson_r > b.pearson_r ∧ a.mue < b.mue

-- The optimal N* is not maximal: adding more waters beyond N*
-- introduces noise that degrades correlation.
theorem nwat_not_monotonic :
  ¬ (∀ (r1 r2 : NwatResult), r1.n_waters < r2.n_waters → dominates r2 r1) := ...
```

The jump-cannon analog: the Louvain resolution parameter has an optimal value for a given graph. Too fine (over-fragmented communities) and the layout loses global structure. Too coarse (one giant community) and the layout loses local detail. The "optimal" resolution for jump-cannon's vault graphs is empirically determined, just as the optimal Nwat is target-dependent.
