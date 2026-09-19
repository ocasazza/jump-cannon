# AmpC β-Lactamase: False Positive Filtration via Explicit Solvent

## The AmpC Decoy Problem

AmpC β-lactamase is a classic test case for scoring function validation because it has a well-characterized binding site with:
- A deep, largely hydrophobic pocket that accommodates the β-lactam ring
- Several crystallographic water molecules in hydrogen-bonding positions near the catalytic Ser64
- Published decoy sets (from DUD-E) that exploit the hydrophobic bias of implicit-solvent scoring

### The False Positive Mechanism

Docking decoys with isopropyl, chloropropyl, or tert-butyl groups score well in Glide SP/XP because they bury nonpolar surface area in the hydrophobic pocket. The empirical scoring function rewards this burial without checking whether the buried volume displaces waters that *cannot* be displaced.

**Crystallographic evidence**: The pocket contains 2–3 structurally conserved waters that form H-bonds with Ser64, Lys67, and Tyr150. These waters are observed in >80% of AmpC structures. A decoy's alkyl group that overlaps these water positions cannot form the compensating H-bonds — but the implicit-solvent scoring function never checks.

### Nwat-MMGBSA Filtration

The Maffucci protocol correctly identifies these false positives because the explicit water shell reveals the steric clash:

1. **MD simulation**: The decoy's chloropropyl group sterically overlaps the conserved water positions
2. **closest water selection**: The N=30 closest waters include the displaced conserved waters (now in higher-energy positions)
3. **MM-GBSA evaluation**: The energy of these displaced waters is higher than in the apo simulation → unfavorable ΔΔG
4. **Result**: The decoy receives a poor Nwat-MMGBSA score despite a good docking score

## Jump-Cannon Parallel: Repulsion as Steric Penalty

The force-directed layout's repulsion term is the direct analog of the steric clash penalty:

```wgsl
// force.wgsl: repulsion computation
// Under SpringElectrical:
//   repulsion = K_rep * mass_j / (dist^2 + floor) * direction
// Under t-FDP with r = dist/spring_len:
//   repulsion = r / (1 + r^2)^gamma * direction
```

A node placed where it overlaps a dense region experiences strong repulsion — this is the graph-layout equivalent of a steric clash with a conserved water. The `repulsion_radius` parameter in `GpuForceOptions` is directly analogous to the van der Waals radius cutoff in molecular mechanics.

### The Penalty Mechanism

| Molecular Penalty | Jump-Cannon Penalty |
|---|---|
| Steric clash with conserved water | Node overlaps dense octree cell |
| Cost: +2–5 kcal/mol per clash | Cost: repulsion force displaces node outward |
| Detected by: WaterMap ΔG_hyd > 0 | Detected by: Barnes-Hut COM with large mass |
| False positive: ligand that "fits" geometrically but clashes with water | Node that has many edges (looks "connected") but sits in wrong region |

### The "Magic Methyl" as Optimal Repulsion Displacement

The Glide WS "magic methyl" detection — finding a single methyl group that displaces a high-energy water for +1–2 kcal/mol — maps to finding a node whose single coordinate adjustment (a "methyl group" of positional change) resolves multiple local overlaps simultaneously. This is what the `geometric` engine's coordination constraint does: a single angle adjustment can satisfy multiple neighbor constraints at once.
