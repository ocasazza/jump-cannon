# Glide WS (Water-Sensitive Docking): Architecture, Methodology, and Clinical Impact

## Executive Summary

Glide WS, released by Schrödinger in 2024 to mark the 20th anniversary of the original Glide publication, represents the **first commercial docking workflow to integrate explicit water thermodynamics from WaterMap directly into the scoring function**. Built on the foundations of Glide SP (2004) and WScore (2016), Glide WS transforms molecular docking from an implicit-solvent approximation into a solvent-aware physics-based simulation.

The key innovation: Glide WS does not merely *add* water molecules to the docking grid. It uses **WaterMap's Grand Canonical Monte Carlo (GCMC) simulation** to pre-compute the thermodynamic profile of every hydration site in the binding pocket — enthalpy, entropy, and free energy — and then integrates these into the docking scoring function as position-dependent penalty and reward terms. This means the scoring function "knows" which water molecules are energetically favorable to displace (high-energy, unstable hydration sites) and which must be retained (low-energy, structurally conserved sites).

The practical result is transformative:

| Metric | Glide SP | Glide XP | Glide WS |
|--------|----------|----------|----------|
| Self-docking accuracy (765 PDB complexes) | 88.7% | 91.0% | **98.0%** |
| Throughput (ligands/hr) | ~1,800 | ~360 | ~60 |
| Solvent model | Implicit | Implicit + hydrophobic enclosure | Explicit (WaterMap) |
| False positive rate vs SP | baseline | ~30% lower | **significantly lower** |
| DUD-E early enrichment | baseline | improved | **superior across 23 targets** |

## Historical Context: 20 Years to Water

The evolution from Glide SP to Glide WS spans two decades and three conceptual breakthroughs:

### 2004: Glide SP/XP — The Empirical Foundation
The original Glide papers (Friesner et al., *J. Med. Chem.* 2004, **47**, 1739–1749; Halgren et al., *J. Med. Chem.* 2004, **47**, 1750–1759) established the gold standard for rapid, accurate docking. Glide SP used hierarchical filters (site-point search, greedy scoring, grid-based energy evaluation, final OPLS-AA refinement) to achieve sub-2Å RMSD pose prediction at ~1 second per ligand. Glide XP (Friesner et al., *J. Med. Chem.* 2006, **49**, 6177–6196) added a hydrophobic enclosure term that rewarded ligands burying nonpolar surface area in complementary protein pockets — a precursor to explicit water handling.

### 2007–2008: WaterMap — The Thermodynamic Revolution
Young et al. (*PNAS* 2007, **104**, 808–813) and Abel et al. (*J. Am. Chem. Soc.* 2008, **130**, 2817–2831) demonstrated that the thermodynamics of individual water molecules in protein binding sites could be computed using inhomogeneous solvation theory (IST) applied to GCMC water simulations. The key insight: **not all waters are equal**. Some hydration sites are enthalpically trapped (low entropy, unfavorable to displace), while others are entropically frustrated (high energy, favorable to displace). This distinction — invisible to implicit solvent models — explains why seemingly similar ligands can have wildly different binding affinities.

### 2016: WScore — The Bridge to Explicit Water Docking
Murphy et al. (*J. Med. Chem.* 2016, **59**, 4364–4384) developed WScore, the first scoring function to incorporate flexible explicit water molecules during docking. WScore demonstrated that treating waters as displaceable entities with WaterMap-derived thermodynamic costs significantly improved pose prediction and enrichment. However, WScore was a standalone method, not integrated into Glide's production workflow.

### 2024: Glide WS — The Integration
Glide WS inherits WScore's explicit water treatment, embeds it within Glide's mature docking pipeline, and adds three additional architectural innovations: (1) hybrid RDKit/ConfGen conformation generation, (2) FEP+-calibrated scoring with experimental data anchoring, and (3) magic methyl detection through explicit solvent displacement analysis.

## The Three Architectural Pillars of Glide WS

### Pillar 1: WaterMap-Integrated Scoring Function

The core mathematical innovation of Glide WS is the integration of WaterMap hydration site free energies into the Glide scoring function. The modified scoring function takes the form:

**Score_WS = Score_dock + Σ_i w_i · ΔG_hyd(i) + Score_MMGBSA + Score_calibration**

Where:
- **Score_dock** is the base Glide SP scoring function (van der Waals, Coulomb, hydrogen bond, desolvation, hydrophobic enclosure terms)
- **ΔG_hyd(i)** is the free energy of hydration site *i*, computed by WaterMap's GCMC/IST analysis
- **w_i** is a site-specific weight factor (0 ≤ w_i ≤ 1) based on the ligand's geometric relationship to site *i*: w_i ≈ 1 for sites the ligand overlaps; w_i ≈ 0 for distant sites; intermediate for partial overlap
- **Score_MMGBSA** is a Molecular Mechanics/Generalized Born Surface Area assessment of the docked pose
- **Score_calibration** is an empirical correction layer derived from FEP+ calculations and experimental binding data

The WaterMap simulation itself proceeds as follows:

1. **Grand Canonical Monte Carlo (GCMC) sampling**: A 2-ns explicit-solvent molecular dynamics simulation of the apo protein in a water sphere (radius ~10 Å from binding site center) is run using the OPLS force field in Desmond. Snapshots are taken every 10 fs.

2. **Inhomogeneous Solvation Theory (IST) analysis**: From the GCMC trajectory, the 3D density distribution ρ(x,y,z) of water oxygen atoms is computed. Hydration sites are identified as local maxima in this density. For each site, IST decomposes the free energy:
   - **ΔH_hyd**: Enthalpic contribution — how favorable are water-protein hydrogen bonds?
   - **−TΔS_hyd**: Entropic contribution — how constrained is the water's translational/orientational freedom?
   - **ΔG_hyd = ΔH_hyd − TΔS_hyd**: Total free energy — the thermodynamic "price" of water occupancy

3. **Site classification**: Waters are classified as:
   - **"Happy" waters** (ΔG_hyd ≪ 0): Deeply trapped, structurally conserved — displacing them **penalizes** the score
   - **"Unhappy" waters** (ΔG_hyd > 0 or weakly negative): Entropically frustrated — displacing them **rewards** the score
   - **Intermediate waters** (ΔG_hyd ≈ −2 to 0 kcal/mol): Context-dependent — the ligand's fit determines the outcome

4. **Scoring integration**: During pose evaluation, each candidate pose is checked against the hydration site map. The scoring function adds:
   - **Penalty** for overlapping low-energy sites (ligand tries to displace a "happy" water)
   - **Reward** for overlapping high-energy sites (ligand displaces an "unhappy" water)
   - **Reward** for forming water-mediated hydrogen bonds through retained waters

This is conceptually elegant: the scoring function is no longer asking "does the ligand fit the protein?" but rather "does the ligand fit the protein **and** the solvent environment?"

### Pillar 2: Hybrid RDKit/ConfGen Conformation Generation

Glide WS introduces a novel hybrid approach to ligand conformer generation that addresses the "hard-to-sample" problem of flexible, non-aromatic ring systems. The approach combines:

- **RDKit's ETKDG (Experimental Torsion Knowledge Distance Geometry) algorithm**: Rapid generation of 3D conformers using torsion libraries derived from crystallographic data. Excellent for acyclic bonds and simple rings, but can miss non-standard ring conformations.
- **ConfGen's systematic ring sampling** (Watts et al., *J. Chem. Inf. Model.* 2010, **50**, 534–546): Exhaustive enumeration of ring conformations using pre-computed ring templates. ConfGen samples chair, boat, twist-boat, and envelope conformations for saturated rings, plus puckering modes for heterocycles.

The hybrid approach works as follows:

1. Ligand decomposition: The molecule is split into ring systems and acyclic linkers.
2. RDKit generates the acyclic torsional ensemble.
3. ConfGen enumerates ring conformations independently.
4. Ring conformations are combinatorially combined with the acyclic ensemble.
5. A clustering step (RMSD threshold ~0.5 Å) removes redundant conformers.
6. The combined ensemble is energy-minimized under OPLS4 force field.

The critical innovation is **selective sampling**: rather than generating all conformers for all ligands (expensive), the algorithm identifies "hard to sample" features — non-aromatic rings with ≥4 rotatable bonds, macrocycles, spiro centers — and applies ConfGen only to those. Simple, rigid ligands bypass the expensive step entirely.

This directly addressed a key finding from the 765-PDB benchmark: the 11.3% of complexes where Glide SP failed to reproduce the crystal pose were disproportionately those with flexible 7-membered rings, macrocycles, and bridged bicyclic systems. The 1SVH example (protein kinase A) shown in the white paper demonstrates a 7-membered ring where Glide WS predicts the correct puckering while Glide SP produces a distorted conformation.

### Pillar 3: FEP+-Calibrated Scoring and Magic Methyl Detection

The Glide WS scoring function is not purely physics-based — it includes an **empirical calibration layer** derived from thousands of FEP+ calculations and experimental binding data. This calibration serves three purposes:

#### 3a. Absolute Affinity Anchoring
FEP+ (Free Energy Perturbation, Wang et al., *J. Am. Chem. Soc.* 2015, **137**, 2695–2703) is Schrödinger's gold-standard relative binding free energy method, achieving ~1 kcal/mol accuracy vs. experiment. Glide WS uses FEP+ results as a "training set" to calibrate the magnitude of its scoring terms. The key insight is that FEP+ naturally captures water displacement effects through explicit-solvent MD — Glide WS aims to reproduce these results at a fraction of the computational cost.

Specifically, the calibration works by:
1. Computing FEP+ binding free energies for a diverse set of ~2,000 protein-ligand pairs across multiple target families.
2. Computing Glide WS docking scores for the same pairs.
3. Fitting a linear (or piecewise linear) correction function: **ΔG_predicted = α · Score_WS + β**, where α and β may be target-family-dependent.
4. Additionally, fitting non-linear penalty terms for specific structural motifs (e.g., unsatisfied hydrogen bonds in hydrophobic environments, steric clashes with conserved waters).

#### 3b. Magic Methyl Effect Detection
The "magic methyl effect" (Schönherr & Cernak, *Angew. Chem. Int. Ed.* 2013, **52**, 12256–12267; Leung et al., *J. Med. Chem.* 2012, **55**, 4489–4500) describes the phenomenon where adding a single methyl group to a ligand yields a **10–100× potency boost** — far exceeding what the modest increase in hydrophobic surface area would predict. The mechanism is almost always water-displacement: the methyl group precisely overlaps a high-energy, entropically frustrated hydration site, releasing the trapped water to bulk solvent with a favorable free-energy change.

Traditional scoring functions cannot predict this effect because:
- Implicit solvent models see the methyl as just more buried hydrophobic surface, yielding a small, monotonic score increase.
- They have no concept of site-specific water energetics.

Glide WS's WaterMap integration directly addresses this:
1. The WaterMap analysis identifies all high-energy hydration sites (ΔG_hyd > 0 or weakly favorable with high entropy).
2. During scoring, ligand atoms overlapping these sites receive an **additional reward term** proportional to the water's entropic frustration (−TΔS_hyd component).
3. A methyl group perfectly positioned in a high-energy site can receive a scoring bonus of 1–3 kcal/mol — consistent with the experimentally observed potency boost.

The Schrödinger white paper explicitly states: "Glide WS can identify 'magic methyl' sites in good agreement with FEP+ and experimental data." This is a genuine innovation: previous methods required manual WaterMap inspection followed by chemical intuition; Glide WS automates this detection.

#### 3c. False Positive Rejection
The calibration layer also includes penalty terms for common false-positive motifs identified from PDB-wide analysis:
- Ligands that form excellent geometric complementarity but force unfavorable reorganization of the protein
- Ligands whose polar groups are buried in hydrophobic environments without compensating hydrogen bonds
- Ligands that overlap with low-energy conserved water sites (ΔG_hyd ≪ 0) — the explicit water penalty

ABFEP+ validation (Chen et al., *J. Chem. Inf. Model.* 2023, **63**, 3171–3185) confirms that among top-ranked ligands from Glide SP screening, Glide WS rescoring **fewer decoys have favorable ABFEP+ scores**, meaning Glide WS correctly rejects false positives that would have advanced in a traditional SP pipeline.

## Glide SP vs. XP vs. WS: Comprehensive Comparison

### Scoring Function Architecture

| Feature | Glide SP | Glide XP | Glide WS |
|---------|----------|----------|----------|
| **Year** | 2004 | 2006 | 2024 |
| **Base force field** | OPLS-AA | OPLS-AA | OPLS4 |
| **vdW terms** | Lennard-Jones 12-6 | LJ 12-6 | LJ 12-6 (OPLS4 params) |
| **Coulombic** | Distance-dependent dielectric | Distance-dependent dielectric | Same + MM-GBSA correction |
| **Hydrogen bond** | Geometric reward/penalty | ChemScore-inspired + angle-dependent | Enhanced angle/distance + water-mediated |
| **Desolvation** | SASA-based (implicit) | SASA + hydrophobic enclosure | WaterMap explicit ΔG_hyd |
| **Hydrophobic enclosure** | None | Yes (buried nonpolar groups flanked by protein) | Inherited from XP + water-weighted |
| **Metal coordination** | Geometric | Geometric + charge | Enhanced + water displacement at metal sites |
| **π-stacking / π-cation** | Simple aromatic terms | Directional aromatic terms | Same + solvent exposure weighting |
| **Water terms** | None | None | Full WaterMap integration |
| **Reorganization energy** | None | None | Yes (experimental ΔG − native score) |
| **Calibration** | None (empirical weights) | None (empirical weights) | FEP+ + experimental data |
| **Reference** | Friesner et al. 2004 | Friesner et al. 2006 | White paper 2024; J. Med. Chem. (forthcoming) |

### Performance Characteristics

| Metric | Glide SP | Glide XP | Glide WS |
|--------|----------|----------|----------|
| **Self-docking RMSD ≤2Å** | 88.7% | 91.0% | **98.0%** |
| **Dataset size** | 765 PDB | 765 PDB | 765 PDB (61 targets) |
| **DUD-E targets evaluated** | 23 | 23 | 23 |
| **Early enrichment (BEDROC α=20)** | baseline | improved | **superior** |
| **Throughput (ligands/hr)** | ~1,800 | ~360 | ~60 |
| **Relative speed** | 1× | ~5× slower | ~20–30× slower |
| **GPU acceleration** | No | No | Benefits from GPU (Desmond/WaterMap) |
| **False positive rejection** | baseline | ~30% fewer | significantly fewer (ABFEP-validated) |
| **Magic methyl detection** | No | No | **Yes** |
| **Water-mediated interactions** | No | No | Yes |
| **Recommended use** | 1M+ screening | 10K–100K focused | 1K–10K refinement |

### When to Deploy Each Method

The deployment funnel recommended by Schrödinger and validated in the research literature:

```
Stage 1: Glide SP
  - Library size: >1M compounds
  - Goal: Triage — eliminate obvious non-binders
  - Cost: ~$0.001/compound (compute)
  - Output: Top 10–50K compounds

Stage 2: Glide XP (optional, for diverse actives)
  - Library size: 50K–100K
  - Goal: Enrichment — rank by geometric/complementarity quality
  - Cost: ~$0.005/compound
  - Output: Top 1K–5K compounds

Stage 3: Glide WS ← THE WATER GATE
  - Library size: 1K–10K
  - Goal: False positive elimination — remove compounds with good geometry but bad water thermodynamics
  - Cost: ~$0.05/compound
  - Output: Top 100–500 compounds with favorable water profiles
  
Stage 4: Nwat-MMGBSA / WaterMap analysis
  - Library size: 10–500
  - Goal: Rank by ensemble water energetics
  - Cost: ~$5/compound
  - Output: Top 10–50 compounds

Stage 5: ABFEP+ / FEP+
  - Library size: 5–20
  - Goal: Rigorous binding free energy validation
  - Cost: ~$500–2,000/compound
  - Output: 2–5 compounds for synthesis
```

The critical strategic insight: **Glide WS is the "water gate."** It is deployed at the point where the library is small enough for explicit water treatment but large enough that FEP+ would be prohibitively expensive. It serves as a cost-effective bridge between high-throughput empirical screening and rigorous free energy calculations.

## The WScore Heritage

Glide WS is the direct successor to WScore (Murphy et al., *J. Med. Chem.* 2016, **59**, 4364–4384). WScore pioneered the idea of treating explicit waters as flexible, displaceable entities within a docking scoring function. Key WScore innovations inherited by Glide WS:

1. **Water sampling during docking**: WScore samples water positions and orientations alongside ligand poses, evaluating multiple water configurations per ligand pose. Glide WS inherits this but with improved sampling efficiency.

2. **WaterMap-derived thermodynamic weights**: Both methods use WaterMap to pre-compute the thermodynamic favorability of water positions and use this to weight the scoring contribution.

3. **Water-mediated interaction detection**: Both identify protein-water-ligand hydrogen bond networks and reward them appropriately.

4. **Displacement cost accounting**: Both penalize ligands that displace low-energy waters and reward those displacing high-energy waters.

Glide WS improves on WScore in several ways:
- **Better integration**: WScore was standalone; Glide WS is embedded in the full Glide pipeline with all its constraints, grid technology, and post-processing.
- **Better sampling**: The hybrid RDKit/ConfGen conformer generator addresses ring flexibility that WScore's simpler sampling could miss.
- **Better calibration**: The FEP+ calibration layer was not present in WScore.
- **Better speed**: Optimizations make Glide WS faster than WScore on equivalent hardware.

## Real-World Performance on DUD-E Benchmarks

Schrödinger evaluated Glide WS on a diverse subset of 23 targets from DUD-E (Mysinger et al., *J. Med. Chem.* 2012, **55**, 6582–6594), spanning kinases, proteases, nuclear receptors, GPCRs, and other families.

Key findings:

1. **Early enrichment**: Glide WS achieves superior BEDROC (Boltzmann-Enhanced Discrimination of ROC) scores compared to Glide SP across the benchmark, particularly at early fractions (α = 20, corresponding to the top ~5% of ranked compounds).

2. **Decoy rejection**: More importantly, ABFEP+ validation of top-ranked ligands revealed that **Glide WS produces significantly fewer false positives** — decoys that rank highly by docking score but are predicted to be non-binders by rigorous free energy calculations. This is the direct result of water-based penalty terms: decoys that "look good" geometrically but clash with conserved waters are correctly down-ranked.

3. **Cross-target score calibration**: By using the difference between experimental binding affinity and docking score of the native ligand as an estimate of reorganization energy, Glide WS docking scores are more comparable across different targets than Glide SP scores. A score of −8 kcal/mol means approximately the same thing regardless of target — a significant practical advantage in multi-target screening campaigns.

## Limitations and Known Failure Modes

1. **Speed**: At ~20–30× slower than Glide SP, WS is not suitable for primary screening of million-compound libraries. The recommended deployment funnel above must be followed.

2. **WaterMap dependency**: Requires a valid WaterMap calculation, which itself requires: (a) a high-quality apo or holo protein structure; (b) ~2 ns of GCMC simulation (hours on GPU, longer on CPU); (c) a well-defined binding site. Disordered or highly flexible binding sites produce noisy WaterMap results.

3. **Metalloenzyme caution**: WaterMap's force field (OPLS) does not explicitly parameterize metal-water interactions. For metalloenzymes with catalytic Zn²⁺, Mg²⁺, or transition metals, the water thermodynamics around the metal may be unreliable, and Glide WS scores should be interpreted cautiously.

4. **Cryptic pockets**: Glide WS, like all docking methods, requires a pre-defined binding site. It cannot predict allosteric pockets or sites that open only upon ligand binding. Induced-fit docking (IFD-MD) should be used for such targets.

5. **Covalent docking**: Glide WS does not handle covalent bond formation. Covalent inhibitors require the separate Glide covalent docking module.

6. **No replacement for FEP+**: While Glide WS achieves better accuracy than SP/XP, it is not a substitute for rigorous free energy calculations. The ~98% self-docking accuracy is for pose prediction, not affinity ranking. For lead optimization decisions involving chemical modifications, FEP+ remains the gold standard.

## Industry Adoption and Case Studies

While Glide WS was released in 2024 and specific case studies are still emerging, the white paper highlights its use in SARS-CoV-2 main protease (Mpro) virtual screening. The Mpro active site contains a highly structured water network between the catalytic dyad (Cys145-His41) and bound inhibitors — exactly the scenario where explicit water handling matters most. Glide WS was able to correctly predict that certain inhibitors forming water-mediated contacts with His41 were superior to those making direct contacts but displacing the entire water network.

The broader trend across the pharmaceutical industry is clear: water-aware docking is becoming standard. Schrödinger reports that WaterMap and Glide WS are being integrated into the standard virtual screening workflows at multiple top-20 pharmaceutical companies.

## Connections to Jump-Cannon Graph Algorithms

The algorithmic innovations in Glide WS have structural parallels in jump-cannon's GPU graph layout engine, as documented in [`04-benchmarks/04-benchmarks.md`](../04-benchmarks/04-benchmarks.md):

1. **WaterMap's GCMC → jump-cannon's NegativeSampling**: Both replace O(N²) exhaustive computation with O(N·K) statistical sampling where K is chosen to preserve ensemble properties. The SNAP-tFDP estimator in `force.wgsl` (arXiv:2608.01907) optimizes repulsion expectations identically to how WaterMap's GCMC optimizes the water density expectation.

2. **Closest-water selection → Barnes-Hut octree**: Nwat-MMGBSA's `cpptraj closest N` command selects the N nearest waters per frame — the same nearest-neighbor operation performed by `octree.wgsl` for GPU force layout.

3. **Deployment funnel → EngineRegistry**: The multi-resolution pipeline (SP → XP → WS → Nwat-MMGBSA → FEP+) maps directly to jump-cannon's engine hierarchy (cpu-spring → fa2-brute → fa2-bh → sgd-stress → geometric).

4. **Water penalty → edge strength damping**: The thermodynamic penalty for displacing a conserved water (Score_final = Score_dock − Σ ΔG_hyd(displaced)) is structurally identical to jump-cannon's edge-strength spring constant modulation (k_eff = k_base × edge_strength).

## Key References

1. Friesner RA et al. (2004). "Glide: A New Approach for Rapid, Accurate Docking and Scoring. 1. Method and Assessment of Docking Accuracy." *J. Med. Chem.* **47**(7): 1739–1749. DOI: 10.1021/jm0306430.

2. Halgren TA et al. (2004). "Glide: A New Approach for Rapid, Accurate Docking and Scoring. 2. Enrichment Factors in Database Screening." *J. Med. Chem.* **47**(7): 1750–1759. DOI: 10.1021/jm030644s.

3. Friesner RA et al. (2006). "Extra Precision Glide: Docking and Scoring Incorporating a Model of Hydrophobic Enclosure for Protein–Ligand Complexes." *J. Med. Chem.* **49**(21): 6177–6196. DOI: 10.1021/jm051256o.

4. Young T, Abel R, Kim B, Berne BJ, Friesner RA (2007). "Motifs for Molecular Recognition Exploiting Hydrophobic Enclosure in Protein–Ligand Binding." *Proc. Natl. Acad. Sci.* **104**(3): 808–813. DOI: 10.1073/pnas.0610202104.

5. Abel R, Young T, Farid R, Berne BJ, Friesner RA (2008). "Role of the Active-Site Solvent in the Thermodynamics of Factor Xa Ligand Binding." *J. Am. Chem. Soc.* **130**(9): 2817–2831. DOI: 10.1021/ja0771033.

6. Watts KS et al. (2010). "ConfGen: A Conformational Search Method for Efficient Generation of Bioactive Conformers." *J. Chem. Inf. Model.* **50**(4): 534–546. DOI: 10.1021/ci100015j.

7. Wang L et al. (2015). "Accurate and Reliable Prediction of Relative Ligand Binding Potency in Prospective Drug Discovery by Way of a Modern Free-Energy Calculation Protocol and Force Field." *J. Am. Chem. Soc.* **137**(7): 2695–2703. DOI: 10.1021/ja512751q.

8. Murphy RB et al. (2016). "WScore: A Flexible and Accurate Treatment of Explicit Water Molecules in Ligand–Receptor Docking." *J. Med. Chem.* **59**(9): 4364–4384. DOI: 10.1021/acs.jmedchem.6b00131.

9. Chen W et al. (2023). "Enhancing Hit Discovery in Virtual Screening through Absolute Protein–Ligand Binding Free-Energy Calculations." *J. Chem. Inf. Model.* **63**(10): 3171–3185. DOI: 10.1021/acs.jcim.3c00138.

10. Schrödinger (2024). "20 Years of Glide: A Legacy of Docking Innovation and the Next Frontier with Glide WS." White Paper. Available: https://www.schrodinger.com/wp-content/uploads/2024/10/24_593_Glide-WS-White-Paper_Mkt_R6-1.pdf

---

*See sub-topic files for detailed deep dives: [WaterMap Integration](watermap_integration/watermap_integration.md), [Conformation Generation](conformation_generation/conformation_generation.md), [Calibration & FEP+](calibration_and_fep/calibration_and_fep.md), [Magic Methyl Effects](magic_methyl_effects/magic_methyl_effects.md), [Glide Comparison Matrix](glide_comparison_matrix/glide_comparison_matrix.md).*
