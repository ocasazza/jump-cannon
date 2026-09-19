# Alternative Docking Tools with Explicit Water Support

## Overview

While Schrodinger's Glide WS is the most mature water-aware docking platform, several alternative tools provide varying degrees of explicit water support.

## AutoDock Family

### AutoDock Vina

No explicit water support. Standard docking with implicit solvent scoring.

### AutoDock4 (with water extension)

Roberts & Mancera (2009). Allows user-specified explicit water molecules at fixed positions. Waters are treated as part of the receptor with special van der Waals parameters.

Limitations: waters are static (no sampling), user must specify positions, no thermodynamic quality assessment.

### Smina / Smina Vinardo

Fork of Vina with customizable scoring. Vinardo scoring function was designed with water displacement in mind but remains implicit solvent.

## GNINA

Deep learning-enhanced docking. Uses a 3D CNN scoring function trained on cross-docked PDBbind structures.

Water handling: Implicit. The CNN may indirectly learn water-mediated interaction patterns from structural data, but no explicit water thermodynamics.

## Dock (UCSF)

### Dock 3.8

Footprint-based scoring with implicit solvent. The "desolvation" term uses an atomic contact-based model.

### Dock 6

Supports fixed-position explicit waters. Waters are part of the receptor and scored with standard Lennard-Jones + Coulomb terms. No thermodynamic quality assessment.

## GOLD (CCDC)

### GoldScore / ChemScore

Standard implicit-solvent scoring functions.

### Gold with water displacement

GOLD 2023+ allows per-water displacement assessment using a simplified version of the WaterMap concept: waters are scored by their crystallographic B-factor (flexibility) and local protein environment. Waters with high B-factors are considered "displaceable." This is a heuristic approximation of WaterMap thermodynamics.

## rDock

Open-source docking originally from Vernalis. Supports explicit waters in the receptor cavity as part of the docking search. Waters can be toggled on/off during the search. No thermodynamic quality assessment.

## Rosetta Ligand

Rosetta's docking protocol uses a full-atom energy function (Rosetta Energy Function 2015, REF15) with explicit solvent via the Lazaridis-Karplus implicit solvation model (EEF1). Explicit waters can be included as part of the protein structure during docking.

## Comparison Table

| Tool | Explicit Water | Dynamic Sampling | Water dG | Magic Methyl | Open Source |
|---|---|---|---|---|---|
| Glide WS | Yes (WaterMap) | Yes (during docking) | dG_hyd, dH, -TdS | Yes | No (commercial) |
| GOLD | Yes (crystal waters) | No (fixed) | B-factor only | No | No (commercial) |
| GNINA | No (implicit) | -- | -- | No | Yes |
| AutoDock Vina | No | -- | -- | No | Yes |
| rDock | Yes (static) | Toggle on/off | No | No | Yes |
| Dock 6 | Yes (static) | No (fixed) | No | No | Yes (academic) |
| Rosetta Ligand | Optional (static) | No (fixed) | No | No | Yes (academic) |

## The Open-Source Gap

No open-source docking tool provides the equivalent of Glide WS's WaterMap-informed water sampling with thermodynamic quality assessment. The closest alternatives are:

1. **Post-docking rescoring with Nwat-MMGBSA** (AmberTools, open source) -- the most practical open-source alternative to Glide WS. Run any docking tool, then rescore with Nwat-MMGBSA.

2. **rDock + Nwat-MMGBSA**: Use rDock's water toggle during docking, then rescore with Nwat-MMGBSA.

3. **GIST analysis** (cpptraj, open source) -- MD-based hydration thermodynamics analysis that could theoretically be integrated into a docking pipeline, but no existing tool does this.

## Jump-Cannon Connection

The open-source water-docking gap mirrors a common pattern: the commercial platform (Schrodinger) achieves integration that the open-source ecosystem provides as separate tools. Jump-cannon fills a similar role for GPU graph layout: it provides an integrated GPU pipeline that would otherwise require assembling CPU layout, GPU rendering, and analytics tools separately.

## References

- Eberhardt et al. (2021). "AutoDock Vina 1.2.0: New Docking Methods..." J. Chem. Inf. Model. 61(8): 3891-3898.
- McNutt et al. (2021). "GNINA 1.0: molecular docking with deep learning." J. Cheminformatics 13:43.
- Jones et al. (2021). "GOLD: The Cambridge Structural Database docking program." Acta Cryst. D.
- Ruiz-Carmona et al. (2014). "rDock: A Fast, Versatile and Open Source Program for Docking." PLoS Comput. Biol. 10(4): e1003571.
