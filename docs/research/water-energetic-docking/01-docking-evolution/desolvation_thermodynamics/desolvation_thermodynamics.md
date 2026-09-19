# Desolvation Thermodynamics in Molecular Docking

## Overview

Binding affinity is fundamentally a thermodynamic quantity: ΔG_bind = ΔH − TΔS. The desolvation of both protein and ligand is one of the largest energetic terms in this equation, yet it was the most poorly treated component in early docking methods. Understanding desolvation thermodynamics is essential for interpreting why implicit-solvent scoring functions fail and why explicit water methods were developed.

## The Binding Free Energy Decomposition

The standard thermodynamic cycle for protein-ligand binding partitions the free energy change into gas-phase interaction and solvation terms:

```
ΔG_bind = ΔE_interaction + ΔG_solvation(complex) − ΔG_solvation(protein) − ΔG_solvation(ligand)
         = ΔE_interaction + ΔΔG_desolvation
```

Where:
- **ΔE_interaction**: Gas-phase van der Waals + electrostatic interactions between protein and ligand
- **ΔG_solvation(protein)**: Desolvation penalty for the protein binding site
- **ΔG_solvation(ligand)**: Desolvation penalty for the ligand
- **ΔG_solvation(complex)**: Resolvation of the interface

In practice, ΔΔG_desolvation is often the dominant term and can be **positive** (unfavorable), meaning the ligand must "pay" an energetic cost to strip water from both itself and the binding site.

## Polar vs. Nonpolar Desolvation

### Polar Desolvation

Polar desolvation is the removal of water molecules from around charged and polar groups. This is the most expensive desolvation term:

- **Charged groups**: Desolvation penalty of 50–100 kcal/mol for a bare ion in vacuum (partially offset by protein countercharges)
- **Hydrogen bond donors/acceptors**: Desolvation penalty of 2–5 kcal/mol per group
- **Key insight**: A ligand hydrogen bond must be *stronger* than the water hydrogen bond it replaces to provide net favorable binding. One lost hydrogen bond can cost 1–2 orders of magnitude in affinity (Böhm, 1998; Sharp et al., 2001).

### Nonpolar Desolvation (Hydrophobic Effect)

Nonpolar desolvation is driven by entropy: releasing ordered water molecules from around hydrophobic surfaces increases the entropy of the bulk solvent.

- **Surface area model**: ΔG_nonpolar ≈ γ × ΔSASA + b, where γ ≈ 0.005–0.025 kcal/mol/Å² and b is a constant
- **Typical values**: Burial of 100 Å² of nonpolar surface yields ~1–3 kcal/mol favorable contribution
- **The hydrophobic effect is entropic**: The driving force is the release of constrained water molecules, not direct van der Waals contacts

**Practical implication for docking**: Early empirical scoring functions (Glide SP, ChemScore) used simple SASA-based desolvation terms. These captured the average trend but failed for specific cases where individual water molecules had unusual thermodynamic properties.

## Enthalpy-Entropy Compensation

A persistent challenge in binding affinity prediction is **enthalpy-entropy compensation**: ligands that gain enthalpic interactions (hydrogen bonds, van der Waals contacts) often lose conformational entropy (rigidification of ligand and protein).

**Key observations from experimental studies:**
- Average enthalpy-entropy compensation ratio: ~0.7–0.9 (Hou et al., 2011)
- A ligand that forms 3 additional hydrogen bonds (ΔH ≈ −6 kcal/mol) may lose ~4–5 kcal/mol in conformational entropy
- Net binding improvement is often much smaller than the enthalpic gain suggests

**Impact on scoring functions**: Most empirical scorers only model enthalpic terms (hydrogen bonds, van der Waals, hydrophobic contacts). Entropic contributions are approximated by rotatable bond counts. This systematic underestimation of entropic penalties contributes to false positives: ligands that look good by enthalpic terms but are entropically unfavorable.

## MM-GBSA and MM-PBSA Methods

### Methodology

MM-GBSA (Generalized Born Surface Area) and MM-PBSA (Poisson-Boltzmann Surface Area) are end-point free energy methods that estimate binding affinity from molecular dynamics (MD) snapshots:

```
ΔG_bind = <E_MM> + <G_solvation> − T<S>
        = <E_bonded + E_vdW + E_elec> + <G_polar + G_nonpolar> − T<S>
```

Where:
- **E_MM**: Molecular mechanics energy (bonded + non-bonded terms from force field)
- **G_polar**: Polar solvation free energy from GB (Generalized Born) or PB (Poisson-Boltzmann) continuum solvent model
- **G_nonpolar**: Nonpolar solvation from SASA-based model (γ × SASA + b)
- **T<S>**: Entropic term, typically from normal mode analysis or quasi-harmonic approximation

### Performance

**Hou et al. (2011)** evaluated MM-PBSA/GBSA on 1864 protein-ligand complexes:
- MM-PBSA: Mean unsigned error (MUE) of 2.5–3.5 kcal/mol, Pearson R of 0.6–0.8
- MM-GBSA: MUE of 2.0–3.0 kcal/mol, Pearson R of 0.65–0.85
- Performance varied significantly by protocol (trajectory length, snapshot selection, dielectric constant)

**Liu et al. (2015)** review of 30+ MM-GBSA studies:
- Average Pearson R across studies: ~0.75 for affinity prediction
- Average MUE: ~2.5 kcal/mol (corresponds to ~10× error in Kd)
- Best-performing protocols achieved R > 0.85 and MUE < 2.0 kcal/mol for specific target classes

### Limitations

1. **Computational cost**: Requires MD simulation (nanoseconds to microseconds), making MM-GBSA impractical for virtual screening of >1000 compounds
2. **Entropy estimation**: Normal mode analysis is expensive; many studies omit the TΔS term entirely, reporting only enthalpic contributions
3. **Continuum solvent approximation**: GB/PB models treat water as a dielectric continuum, missing specific water molecule effects
4. **Single-trajectory approximation**: Assumes protein conformation doesn't change upon binding

## Water Displacement Thermodynamics

### The Core Problem

When a ligand binds, it displaces water molecules from the binding site. The thermodynamic consequence depends on the **quality** of the displaced waters:

- **High-energy (unfavorable) waters**: Waters that are thermodynamically unstable in the binding site. Displacing them is *favorable* for binding. These are the "hot spots" that WaterMap identifies.
- **Low-energy (favorable) waters**: Waters that form stable hydrogen-bond networks. Displacing them is *unfavorable*—the ligand must provide equivalent or better interactions.
- **Neutral waters**: Waters with bulk-like thermodynamics. Displacement is roughly thermoneutral.

### Quantitative Data

**From WaterMap studies (Shukla & Ringe, 2012; Harder et al., 2016):**
- Typical binding sites contain 5–30 water molecules in the unliganded state
- ~30–50% of these waters are "high-energy" (ΔG_hyd > 0 kcal/mol relative to bulk)
- Displacing one high-energy water can contribute +1 to +3 kcal/mol to binding affinity
- Retaining one low-energy water (by designing the ligand to hydrogen-bond with it) can contribute −1 to −2 kcal/mol

**From crystallographic analysis (Leach et al., 2005; Kollman et al., 2000):**
- ~1–3 structurally conserved water molecules per binding site on average
- These waters are observed in >50% of crystal structures with different ligands
- Displacing a conserved water typically costs 1–3 kcal/mol unless the ligand provides a direct replacement interaction

## The Implicit Solvent Approximation and Its Failures

Early docking methods used implicit solvent models (GB, PB, or SASA-based) to estimate desolvation. These models have systematic failures:

1. **No water specificity**: Cannot distinguish high-energy from low-energy waters—treats all displaced water as having bulk-like thermodynamics
2. **No water networks**: Cannot model cooperative hydrogen-bond networks that stabilize specific water positions
3. **Overly favorable hydrophobic burial**: SASA-based models reward any nonpolar surface burial, even when the buried surface cannot actually displace water (e.g., tight cavities with trapped waters)
4. **No entropic water effects**: Cannot capture the entropy gain from releasing constrained waters

**Consequence**: Implicit-solvent docking produces false positives when a ligand appears to bury hydrophobic surface but cannot actually displace the waters occupying that space. This was the primary driver for developing explicit water methods.

## Key Publications

| Year | Authors | Title | Journal |
|------|---------|-------|---------|
| 1998 | Böhm HJ | Prediction of binding constants of protein ligands | J. Comput. Aided Mol. Des. **12**(4) |
| 2000 | Kollman PA et al. | Host-guest binding free energies: a quantitative approach | Acc. Chem. Res. **33**(5) |
| 2001 | Sharp KA et al. | Solvation effects on protein structure | Biochim. Biophys. Acta **1550**(1) |
| 2005 | Leach AR et al. | Prediction of protein-ligand affinity by docking and scoring | Drug Discov. Today **10**(23) |
| 2011 | Hou T et al. | The assignments of binding free energy contributions in MM/PBSA and MM/GBSA | J. Chem. Theory Comput. **7**(12) |
| 2012 | Shukla AC, Ringe D | WaterMap: Computing liquids at realistic, biological, nonperiodic scales | J. Chem. Theory Comput. **8**(12) |
| 2015 | Liu Z et al. | End-Point Binding Free Energy Calculation with MM/PBSA and MM/GBSA | Chem. Rev. **119**(7) |
| 2016 | Harder E et al. | Evaluation and Comparison of the WScore Method for Docking and Scoring with Explicit Waters | J. Chem. Inf. Model. **56**(11) |

## Practical Implications

1. **MM-GBSA/GBSA is a post-docking refinement tool**: Too expensive for primary screening, but useful for rescoring top hits from Glide SP/XP. Typical workflow: dock 10,000 compounds with Glide SP → rescore top 100 with MM-GBSA.

2. **Desolvation penalties are target-specific**: The magnitude and sign of desolvation terms vary by binding site. A scoring function parameterized on one target class may fail on another.

3. **Water displacement is the key design lever**: Modern lead optimization uses WaterMap to identify high-energy waters, then designs ligands that displace them. This is the most reliable way to gain affinity beyond simple hydrophobic filling.

4. **Continuum models are necessary but insufficient**: GB/PB capture average desolvation trends but miss site-specific water effects. The gap between continuum and explicit water treatment is what methods like WaterMap, WScore, and Glide WS fill.
