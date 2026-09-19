# The Schrodinger Platform: Glide, WaterMap, and FEP+

## Overview

Schrodinger Inc. produces the commercial software platform that enables the complete water-energetic docking workflow: Glide (SP/XP/WS) for docking, WaterMap for hydration thermodynamics, Desmond for GPU MD, and FEP+ for rigorous free energy calculations.

## Component Map

### Glide (Grid-based LIgand Docking with Energetics)

The docking engine. Three tiers: SP (2004), XP (2006), WS (2024).

Key technology: Hierarchical docking with pre-computed receptor grids. The grid stores van der Waals and electrostatic potentials on a 0.2-0.4 A lattice, enabling constant-time energy evaluation during the pose search.

### WaterMap

Hydration thermodynamics engine. Runs Grand Canonical Monte Carlo water sampling in the Desmond MD engine.

Key technology: GCMC simulation -> IST (Inhomogeneous Solvation Theory) analysis -> hydration site thermodynamics. The output (dG_hyd, dH, -TdS per water site) feeds directly into Glide WS scoring.

### Desmond

GPU-accelerated MD engine. Powers both WaterMap and FEP+.

Key technology: Specialized MD code optimized for Nvidia GPUs. Achieves 500+ ns/day on modern hardware for standard protein systems. Uses a custom data layout that maximizes GPU memory coalescing.

### FEP+ (Free Energy Perturbation Plus)

Alchemical free energy calculations for rigorous binding affinity prediction.

Key technology: REST2 (Replica Exchange with Solute Tempering) enhanced sampling. Maps a ligand into another via a series of alchemical intermediate states. MUE ~0.84 kcal/mol vs experiment for drug-like compounds.

### Prime/VSGB

Implicit solvent model for MM-GBSA calculations. Schrodinger's equivalent of the Generalized Born model in AMBER.

Key technology: VSGB (Variable Surface Generalized Born) 2.0. Uses a residue-dependent surface tension that accounts for partial burial.

### MacroModel

Molecular mechanics engine for ligand strain energy and conformational analysis.

Key technology: OPLS4 force field. Used for ligand preparation and strain energy calculation in Glide XP.

### Maestro

The graphical user interface. All Schrodinger tools are scriptable (Python API) but Maestro provides the visual interface for medicinal chemists.

## Licensing and Cost

| Component | License Model | Approximate Annual Cost |
|---|---|---|
| Glide (SP+XP) | Named user | $15-25K |
| Glide WS | Add-on to Glide | +$10-15K |
| WaterMap | Add-on | +$10-15K |
| FEP+ | Separate license | $50-100K |
| Desmond | Included with Glide | -- |
| Full platform | Site license | $200-500K/year |

## Integration with Open-Source Tools

| Schrodinger Component | Open-Source Equivalent | Notes |
|---|---|---|
| Glide | AutoDock Vina, Smina, GNINA | Schrodinger consistently outperforms in benchmarks |
| WaterMap | GIST (cpptraj), SSTMap | GIST uses MD, not GCMC |
| Desmond/GPU MD | pmemd.cuda, OpenMM | Comparable performance |
| FEP+ | pmemd.cuda TI, GROMACS alchemy | FEP+ has better automation and REST2 |
| Prime/VSGB | MMPBSA.py (igb=5) | Results are comparable (+/-1 kcal/mol) |

## References

- Schrodinger LLC, New York, NY (2024). Schrodinger Release 2024-1.
- Bowers et al. (2006). "Scalable Algorithms for Molecular Dynamics Simulations on Commodity Clusters." SC '06.
- Wang et al. (2015). "Accurate and Reliable Prediction of Relative Ligand Binding Potency." JACS 137(7): 2695-2703.
