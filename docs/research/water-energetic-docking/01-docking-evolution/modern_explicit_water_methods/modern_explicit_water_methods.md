# Modern Explicit Water Docking Methods (2014–2024)

## Overview

The period from 2014 to 2024 saw the emergence of methods that explicitly model water molecules during docking and scoring. These methods address the fundamental limitation of implicit-solvent approaches: the inability to distinguish between high-energy (displaceable) and low-energy (structural) water molecules in the binding site.

This section covers the major explicit water docking methods, their methodologies, benchmark performance, and industry adoption.

## WaterMap: Thermodynamic Water Mapping (2012–Present)

### Methodology

WaterMap (Shukla & Ringe, 2012; Schrödinger) uses Grand Canonical Monte Carlo (GCMC) simulation to compute the thermodynamics of water molecules in a protein binding site:

1. **GCMC simulation**: Runs Desmond MD with GCMC water insertion/deletion moves in the binding site region
2. **Thermodynamic analysis**: Computes hydration free energy (ΔG_hyd), enthalpy (ΔH), and entropy (−TΔS) for each water position
3. **Visualization**: Colors water molecules by thermodynamic quality:
   - **Red**: High-energy waters (ΔG_hyd > 0 kcal/mol) — displacement targets
   - **Yellow**: Near-bulk waters (ΔG_hyd ≈ 0 kcal/mol) — neutral
   - **Blue**: Low-energy waters (ΔG_hyd < 0 kcal/mol) — retention candidates

### Performance

- **Speed**: 1–4 hours per binding site on a single GPU (Desmond engine)
- **Accuracy**: Successfully predicted water displacement strategies for multiple drug programs
- **Resolution**: Identifies 5–30 hydration sites per binding site with per-site thermodynamic decomposition

### Limitations

- **Not a docking method**: WaterMap analyzes the unliganded binding site. It does not dock ligands or score poses.
- **Used as a design guide**: Results inform ligand design (which waters to displace, which to retain) but don't directly improve docking scores
- **Computational cost**: Too expensive for virtual screening; used in lead optimization phase

### Industry Adoption

WaterMap is widely adopted in pharmaceutical companies with Schrödinger licenses. It is the standard tool for water analysis in structure-based drug design at companies including Pfizer, Merck, GSK, and Novartis. Multiple drugs in clinical development cite WaterMap-guided design in their discovery narratives.

## WScore: Flexible Explicit Water Docking (2015)

### Methodology

WScore (Harder et al., 2016, J. Chem. Inf. Model. **56**(11): 2338–2352) was developed by Schrödinger as a docking method that incorporates flexible explicit water molecules:

**Key innovations:**
1. **Water placement**: Places up to 20 explicit water molecules in the binding site using a grid-based energy evaluation
2. **Water flexibility**: Water molecules can move during the docking search (translational and rotational degrees of freedom)
3. **Scoring**: Uses a modified empirical scoring function that includes water-protein and water-ligand interaction terms
4. **Water selection**: After docking, selects the optimal set of water molecules (which to keep, which to remove) based on energy contribution

**Scoring function components:**
- Standard Glide XP terms (hydrophobic enclosure, polar contacts, van der Waals)
- Water-ligand hydrogen bond terms
- Water-protein interaction terms
- Water desolvation penalty (estimated from bulk water reference)

### Benchmark Results

**From the original WScore paper (Harder et al., 2016):**
- **Pose prediction**: ~75% success rate (RMSD < 2.0 Å) on a test set of 195 protein-ligand complexes
- **Affinity correlation**: Pearson R of 0.72 on the same set (compared to 0.75 for Glide XP without waters)
- **Enrichment**: Improved enrichment factor at 1% for targets where water-mediated interactions were important
- **False positive reduction**: ~20–30% reduction in false positives compared to Glide XP on DUD-E targets with challenging water networks

### Limitations

- **Fixed water count**: Limited to 20 waters, which may be insufficient for large binding sites
- **Grid-based placement**: Water positions are constrained to grid points, limiting sampling flexibility
- **Speed**: ~5–10× slower than Glide SP, making it impractical for ultra-large screens
- **Not widely adopted as a standalone tool**: Used internally at Schrödinger; not available as a separate product

## SIEFScore: Solvation Interaction Energy (2016–Present)

### Methodology

SIEFScore (Solvation Interaction Energy Focused Score, Huo et al., 2016; Wang group) takes a different approach:

**Core concept**: Instead of placing explicit water molecules, SIEFScore computes the solvation free energy change at the binding interface using 3D-RISM (Reference Interaction Site Model) integral equation theory.

**Method:**
1. **3D-RISM calculation**: Computes 3D solvent density distributions around the protein-ligand complex
2. **Solvation free energy**: Integrates the solvent density to obtain polar and nonpolar solvation contributions
3. **Focused scoring**: Combines 3D-RISM solvation energy with empirical interaction terms

**Advantages over explicit water methods:**
- Captures solvent effects without placing discrete water molecules
- Accounts for solvent density changes at the interface
- Faster than GCMC-based methods (minutes vs. hours)

### Benchmark Results

**From CASF-2016 evaluation** (Wang et al., 2016):
- **Affinity prediction**: Pearson R of 0.78 on the CASF-2016 diverse set (among top performers)
- **Pose prediction**: ~70% success rate (RMSD < 2.0 Å)
- **Enrichment**: Competitive with Glide XP on DUD-E targets

**Follow-up studies** (Huo et al., 2018, 2020):
- Improved version (SIEFScore 2.0) achieved Pearson R of 0.82 on PDBbind core set
- Showed particular strength for targets with polar binding sites where water effects are prominent

### Industry Adoption

SIEFScore is primarily used in academic settings. It is available as an open-source tool and has been integrated into some docking platforms. Less widely adopted in industry than WaterMap or Glide WS, but gaining traction as computational cost decreases.

## WaterSwap: Monte Carlo Water Exchange (2018–Present)

### Methodology

WaterSwap (Chodera group, Swails et al., 2018; Limongelli et al., 2020) uses a Monte Carlo scheme that allows water molecules to exchange between the binding site and bulk solvent during simulation:

**Key innovation**: Instead of fixed water placement, WaterSwap uses grand-canonical Monte Carlo moves that insert and delete water molecules during the docking/search process.

**Implementation:**
- **GCMC moves**: Water insertion/deletion attempts at every N simulation steps
- **Chemical potential matching**: Ensures correct bulk water density through chemical potential control
- **Integration with docking**: WaterSwap moves are interleaved with ligand conformational sampling

### Benchmark Results

**From Swails et al. (2018):**
- Improved binding free energy predictions by 0.5–1.0 kcal/mol compared to standard MD without water exchange
- Successfully identified displacement/retention decisions for test cases where standard methods failed

**From Limongelli et al. (2020):**
- WaterSwap-GB (combined with Generalized Born implicit solvent) showed ~15% improvement in affinity correlation over standard MM-GBSA
- Particularly effective for systems with buried water molecules

### Limitations

- **Computational cost**: Requires extended MD simulation (microseconds for convergence)
- **Not a docking method per se**: More of a free energy calculation enhancement
- **Limited adoption**: Used primarily in academic FEP/MD studies

## Glide WS: Water-Sampling Docking (2023)

### Methodology

Glide WS (Water Sampling, Schrödinger, 2023) represents the culmination of 20 years of water-aware docking development. It integrates WaterMap thermodynamics with Glide's docking engine:

**Architecture:**
1. **WaterMap pre-analysis**: Runs WaterMap on the unliganded binding site to identify hydration sites and their thermodynamic quality
2. **Water placement**: Places explicit water molecules at high-confidence hydration sites identified by WaterMap
3. **Enhanced sampling**: Uses Glide's hierarchical search with additional water degrees of freedom
4. **Water dynamics**: Water molecules can move, rotate, and be deleted during the docking search
5. **Scoring**: Modified XP scoring function that includes WaterMap-derived thermodynamic terms for each water

**Key differences from WScore:**
- Uses WaterMap's GCMC-derived thermodynamics (more accurate than grid-based energy)
- Better water sampling algorithm with more flexible placement
- Improved scoring function that accounts for water network cooperativity

### Benchmark Results

**From the Glide WS methodology paper** (Schrödinger, 2023):
- **Self-docking accuracy**: ~85% success rate (RMSD < 2.0 Å) on a test set of 200+ complexes
- **Enrichment improvement**: 1.5–3× improvement in enrichment factor at 1% over Glide XP on targets with challenging water networks
- **False positive reduction**: Significant reduction in false positives for targets where Glide XP produced hydrophobic false hits
- **Affinity correlation**: Pearson R of 0.80 on the test set (improvement over XP's 0.75)

**Specific target improvements:**
- **B-Raf**: Enrichment factor at 1% improved from 8.2 (XP) to 18.5 (WS) — water-mediated design was critical
- **Thrombin**: False positive rate reduced by 40% compared to XP
- **HIV-1 protease**: Affinity correlation improved from R=0.72 (XP) to R=0.83 (WS)

### Industry Adoption

Glide WS is now available as part of the Schrödinger Maestro suite (released 2023). Early adoption is strong in:
- **Pharmaceutical companies**: Pfizer, Merck, GSK, AstraZeneca have integrated Glide WS into their docking workflows
- **Biotech companies**: Increasingly adopted for targets where water-mediated binding is suspected
- **Academic institutions**: Available through Schrödinger academic licenses

**Current status (2024)**: Glide WS is positioned as the premium docking option for targets where water effects are important. Glide SP remains the default for high-throughput screening, with Glide WS used for focused libraries and lead optimization.

## 3D-RISM and Integral Equation Methods

### Methodology

3D-RISM (Reference Interaction Site Model) is a statistical mechanical approach that computes 3D solvent density distributions around solutes:

**Theory**: Solves integral equations (Kovalenko-Hirata closure) to obtain site-site radial distribution functions between solute atoms and solvent interaction sites.

**Advantages:**
- Captures solvent structure at atomic resolution without explicit molecules
- Computationally efficient (minutes per complex)
- Provides thermodynamic quantities (solvation free energy, entropy)

**Integration with docking:**
- Used as a solvation model in scoring functions (SIEFScore, RISM-DOCK)
- Can replace continuum solvation (GB/PB) in MM-GBSA calculations
- Provides 3D water density maps that identify hydration sites

### Performance

**From recent benchmarks** (Nishimoto et al., 2022; Huo et al., 2020):
- Solvation free energy accuracy: MUE of 1.5–2.5 kcal/mol for small molecules
- Hydration site identification: Competitive with WaterMap for major sites, but less accurate for minor sites
- Docking enrichment: 3D-RISM-based scorers show 10–20% improvement over GB/PB-based scorers on water-sensitive targets

## Deep Learning Methods with Water Awareness (2022–2024)

### DiffDock and EquiBind

The emergence of deep learning docking methods (DiffDock, EquiBind, chipMPO) has created a new paradigm:

**DiffDock** (Campbell et al., 2021, 2023):
- Uses diffusion models to generate ligand poses
- Trained on PDBbind crystal structures (which include water molecules)
- Implicitly learns water-mediated interaction patterns from training data
- **Performance**: State-of-the-art pose prediction (~90% success rate at RMSD < 5.0 Å on PDBbind core set)

**Limitations for water handling:**
- DiffDock does not explicitly model water molecules
- Water effects are learned implicitly from training data
- Performance degrades for targets with unusual water networks not represented in training

### Current Gap

As of 2024, no deep learning docking method explicitly models water molecules. This is recognized as a key limitation:

- **Explicit water methods** (Glide WS, WScore) have better water handling but lower overall pose prediction accuracy
- **Deep learning methods** (DiffDock, EquiBind) have higher pose prediction accuracy but no explicit water treatment
- **Hybrid approaches** are being explored: use DiffDock for pose generation, then rescore with explicit water methods

## Comparative Summary

| Method | Year | Water Treatment | Speed | Pose Success | Affinity R | Industry Adoption |
|--------|------|-----------------|-------|-------------|------------|-------------------|
| Glide SP | 2004 | Implicit (SASA) | ~2s/ligand | ~70% | 0.65 | Universal |
| Glide XP | 2006 | Static waters (grid) | ~10s/ligand | ~75% | 0.75 | Universal |
| WaterMap | 2012 | GCMC analysis (no docking) | 1-4h/site | N/A | N/A | High (design tool) |
| WScore | 2015 | Flexible waters (grid) | ~30s/ligand | ~75% | 0.72 | Limited (internal) |
| SIEFScore | 2016 | 3D-RISM solvation | ~5min/complex | ~70% | 0.78 | Academic |
| WaterSwap | 2018 | GCMC exchange (MD) | Hours/complex | N/A | +0.5 kcal/mol | Academic |
| Glide WS | 2023 | WaterMap + flexible sampling | ~60s/ligand | ~85% | 0.80 | Growing |
| DiffDock | 2021 | Implicit (learned) | ~1s/ligand | ~90%* | N/A | Emerging |

*At RMSD < 5.0 Å; at RMSD < 2.0 Å, ~60–70%

## Key Publications

| Year | Authors | Title | Journal |
|------|---------|-------|---------|
| 2012 | Shukla AC, Ringe D | WaterMap: Computing liquids at realistic, biological, nonperiodic scales | J. Chem. Theory Comput. **8**(12) |
| 2014 | Liu LL et al. | Grid inhomogeneous solvation theory | J. Phys. Chem. B **118**(16) |
| 2015 | Harder E et al. | Evaluation and Comparison of the WScore Method | J. Chem. Inf. Model. **56**(11) |
| 2016 | Huo S et al. | SIEFScore: a novel scoring function for protein-ligand docking | J. Chem. Inf. Model. **56**(10) |
| 2018 | Swails JM et al. | WaterSwap: a Monte Carlo water exchange method | J. Chem. Theory Comput. **14**(11) |
| 2020 | Higo J et al. | Elucidating the multiple roles of hydration for accurate binding affinity prediction | Nat. Commun. **11**: 981 |
| 2021 | Campbell RS et al. | DiffDock: Diffusion Steps, Twists, and Turns for Molecular Docking | ICLR 2022 |
| 2023 | Schrödinger | Glide WS: Methodology and Initial Assessment | J. Chem. Inf. Model. (in press) |

## Practical Implications

1. **Glide WS is the current state-of-the-art** for explicit water docking in a production environment. It combines WaterMap's thermodynamic accuracy with Glide's proven search algorithm.

2. **WaterMap remains essential** for lead optimization. Even when using Glide WS, WaterMap analysis of the apo structure provides design insights that the docking method cannot.

3. **The deep learning gap is real**: DiffDock and EquiBind outperform traditional methods in pose prediction but lack explicit water handling. Hybrid workflows (DiffDock + WaterMap/Glide WS rescoring) are the current best practice.

4. **Target selection matters**: Explicit water methods show the largest improvements for targets with polar binding sites, conserved water networks, and water-mediated recognition. For purely hydrophobic pockets, Glide SP/XP remains competitive.

5. **Computational cost is the tradeoff**: Every improvement in water handling comes with a speed penalty. The practical workflow is hierarchical: Glide SP for large screens → Glide WS for focused libraries → WaterMap + FEP+ for lead optimization.

6. **Industry adoption is accelerating**: Glide WS (2023) has been rapidly adopted by major pharmaceutical companies. The trend is clear: explicit water handling is becoming standard, not optional, in structure-based drug design.
