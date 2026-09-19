# Strategic Evolution of Molecular Docking (2004–2024)

## Executive Summary

Over two decades, molecular docking has evolved from rigid-receptor, implicit-solvent scoring functions to sophisticated methods that explicitly model water thermodynamics. This evolution was driven by three converging forces: (1) the recognition that false positive rates in virtual screening were unacceptably high, (2) the discovery that structural water molecules play a decisive role in binding affinity, and (3) the availability of computational power to model explicit solvent dynamics.

The trajectory moves from **empirical and knowledge-based scoring** (Glide SP/XP, GOLD, Surflex) through **desolvation-aware methods** (MM-GBSA/PBSA, WaterMap) to **explicit water docking** (Glide WS, WScore, SIEFScore, WaterSwap). Each generation addressed the limitations of its predecessor while introducing new challenges.

## Key Milestones

| Period | Paradigm | Representative Methods | Key Advance |
|--------|----------|----------------------|-------------|
| 2004–2008 | Empirical scoring, implicit solvent | Glide SP/XP, GOLD (ChemScore, PLP), Surflex | Systematic pose search, grid-based scoring |
| 2008–2014 | Benchmark-driven refinement | DUD/DUD-E, CASF-2013, MM-GBSA | Decoy-based evaluation, false positive awareness |
| 2012–2018 | Water thermodynamics | WaterMap, GIST, SIEFScore | Explicit water energetics, displacement prediction |
| 2018–2024 | Explicit water + ML | Glide WS, WScore, WaterSwap, DiffDock | Integrated water sampling, deep learning pose prediction |

## The Central Problem: Water

The single most important factor limiting docking accuracy has been the treatment of solvent. Early methods treated water as a continuum (implicit solvent) or ignored it entirely. This produced two classes of error:

1. **False positives**: Ligands that score well by burying hydrophobic groups but fail because they cannot displace high-energy waters from the binding site.
2. **False negatives**: Active ligands that require specific water molecules to mediate hydrogen-bond networks, but are penalized because the scoring function cannot model those waters.

The evolution from Glide SP (2004) to Glide WS (2023) represents a 20-year effort to solve this problem. The intermediate steps—WaterMap, GIST, WScore, SIEFScore—each contributed pieces of the solution.

## Sub-Topics

This research is organized into four detailed sub-topics:

- **[Early Docking Methods](early_docking_methods/)** — Empirical and knowledge-based scoring functions from the 2004–2014 era, including Glide SP/XP, GOLD, and Surflex. Covers benchmark datasets (DUD, DUD-E, CASF) and the false positive problem that drove methodology changes.

- **[Desolvation Thermodynamics](desolvation_thermodynamics/)** — The thermodynamic foundations of binding affinity: polar and nonpolar desolvation penalties, enthalpy-entropy compensation, MM-GBSA/PBSA methods, and the hydrophobic effect.

- **[Water-Mediated Binding](water_mediated_binding/)** — The role of structural water molecules in protein-ligand recognition: conserved water networks, water bridges, thermodynamic selection between displacement and retention.

- **[Modern Explicit Water Methods](modern_explicit_water_methods/)** — Current-generation methods that explicitly model water: Glide WS, WScore, SIEFScore, WaterSwap, GIST, and 3D-RISM. Includes benchmark comparisons and industry adoption trends.



## Practical Implications for Drug Discovery Workflows

The evolution from implicit to explicit water handling has reshaped standard docking workflows:

1. **Hierarchical screening is now the norm**: Glide SP for initial triage (1M+ compounds) → Glide XP for focused libraries (10K–100K) → Glide WS for water-sensitive targets (1K–10K) → WaterMap + FEP+ for lead optimization (10–100 compounds).

2. **Water analysis is a gate step**: Before committing to a docking campaign, WaterMap analysis of the apo structure determines whether explicit water methods are needed. Targets with >3 conserved waters or high-energy displacement candidates get routed to Glide WS.

3. **False positive mitigation**: The combination of DUD-E benchmarks, Glide XP enrichment, and WaterMap water analysis has reduced false positive rates by 2–3× compared to 2004-era Glide SP alone.

4. **Deep learning is changing the landscape**: DiffDock and EquiBind offer faster, more accurate pose prediction but lack explicit water handling. Hybrid workflows (DL pose generation + explicit water rescoring) are emerging as the next standard.

5. **Computational cost has decreased**: What required a cluster in 2004 (Glide SP on 1M compounds) now runs on a single workstation. What required weeks in 2012 (WaterMap analysis) now runs in hours on a GPU. This enables explicit water methods to be used more routinely.

## References

See individual sub-topic files for detailed citations. Key overarching references:

- Friesner RA et al. (2006). "Extra Precision Glide: Docking and Scoring Incorporating a Model of Hydrophobic Enclosure for Protein–Ligand Complexes." *J. Med. Chem.* **49**(21): 6177–6196.
- Friesner RA et al. (2004). "Glide: A New Approach for Rapid, Accurate Docking and Scoring. 2. Enrichment Factors in Database Screening." *J. Med. Chem.* **49**(3): 6177–6196.
- Shukla AC, Ringe D (2012). "WaterMap: Computing Liquids at Realistic, Biological, Nonperiodic Scales with Atomic Detail." *J. Chem. Theory Comput.* **8**(12): 4365–4378.
- Harder E et al. (2016). "Evaluation and Comparison of the WScore Method for Docking and Scoring with Explicit Waters." *J. Chem. Inf. Model.* **56**(11): 2338–2352.
- Kuhn B et al. (2005). "The Directory of Useful Decoys: A Useful Tool for the Evaluation of Docking Performance." *J. Med. Chem.* **48**(11): 4041–4048.
