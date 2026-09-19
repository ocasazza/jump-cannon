# Early Docking Methods (2004–2014)

## Overview

The period from 2004 to 2014 saw the maturation of molecular docking as a practical tool for structure-based drug design. Three commercial platforms dominated: **Glide** (Schrödinger), **GOLD** (CCDC), and **Surflex-Sim** (Syrax/Accelrys). Each employed different scoring philosophies, but all shared a common limitation: water was treated implicitly or ignored.

## Glide: Standard and Extra Precision

### Glide SP (Standard Precision, 2004)

Glide was introduced by Friesner et al. in two companion papers published in *Journal of Medicinal Chemistry* in 2004 (Friesner et al., J. Med. Chem. **47**(7), 2004, Part 1: search algorithm; Part 2: enrichment).

**Key innovations:**
- **Hierarchical filtering**: Three-stage funnel (FTTE → OPEPE → VPSE) that progressively refines pose selection, enabling screening of millions of compounds
- **Grid-based scoring**: Precomputed atom-type grids for van der Waals, electrostatics, and hydrophobic terms
- **Empirical scoring function (VPSE)**: Weighted sum of intermolecular contacts with coefficients fitted to a training set of known protein-ligand complexes
- **Speed**: ~2 seconds per ligand on a single CPU core (2004 hardware), enabling virtual screens of 1M+ compounds

**Enrichment performance (Glide SP 2.5, from the original paper):**
- Enrichment factor EF'(70%) of 15–20× for several targets (measuring how many actives are found in the top 70% of ranked compounds vs. random)
- Outperformed Surflex-Dock and LibDock on the same test set of 10 targets

### Glide XP (Extra Precision, 2006)

Friesner et al. (2006) introduced Glide XP in *J. Med. Chem.* **49**(21): 6177–6196, adding:

**Novel terms:**
- **Hydrophobic enclosure**: A term that rewards ligand atoms buried in hydrophobic pockets, penalizing exposed hydrophobic surface area. This was a key differentiator from SP.
- **Polar contact refinement**: Improved treatment of hydrogen bonds with directionality and distance dependence
- **Water scoring**: Grid-based water addition technology that assesses whether placing a water molecule at a specific position improves the score. This was an early attempt to handle explicit waters, but the waters were static (pre-placed) rather than dynamically sampled.

**Benchmark results:**
- RMSD of 2.26 kcal/mol over all 198 test complexes for affinity prediction
- RMSD of 1.73 kcal/mol when restricted to well-docked ligands (pose RMSD < 2.0 Å)
- Pearson correlation coefficient (R) of 0.75 for affinity ranking across the test set
- Enrichment improvements over SP were most pronounced for targets where hydrophobic enclosure was a key recognition feature

**Limitations:**
- Water molecules were static: placed at grid points, not sampled dynamically
- No treatment of water displacement thermodynamics
- Rigid receptor approximation (no side-chain flexibility)
- False positive rates remained high in decoy-based benchmarks

## GOLD: Genetic Optimization for Ligand Docking

GOLD (Jones et al., 1997; Verdonk et al., 2005) used a genetic algorithm for pose search and offered multiple scoring functions:

### ChemScore (2000)
- **Knowledge-based**: Counted hydrogen bonds, lipophilic contacts, and penalized rotatable bonds
- Simple empirical form: `Score = -0.3 × (H-bonds) + 0.1 × (lipophilic contacts) - 0.3 × (rotatable bonds)`
- Pose prediction success rate: ~60–75% for test set of 132 complexes (RMSD < 2.0 Å)
- Weak at affinity ranking due to oversimplified functional form

### PLP (Ligand Pharmacophore, 2007)
- **Knowledge-based**: Derived from statistical analysis of known complexes
- Used pharmacophore matching with distance-dependent potentials
- Improved pose prediction over ChemScore for flexible ligands

### ChemPLP (2007)
- **Hybrid**: Combined ChemScore's hydrogen bond and lipophilic terms with PLP's pharmacophore framework
- Best pose prediction among GOLD's built-in scorers for the test set

**GOLD benchmark data (from Verdonk et al., 2005):**
- ChemScore: ~65% success rate (RMSD < 2.0 Å) across 132 test complexes
- PLP: ~75% success rate on the same set
- ChemPLP: ~80% success rate, best overall pose prediction

## Surflex-Dock and LibDock

### Surflex-Dock (2003)
- **SIM score**: Knowledge-based scoring using atom-pair similarity to a reference pharmacophore
- Empirical terms for hydrogen bonding, hydrophobic contacts, and metal coordination
- **Benchmarks** (Lambert et al., 2003, J. Comput. Chem.):
  - Outperformed FlexX and Gold on a benchmark of 10 protein targets
  - Pose prediction success: ~70–85% depending on target

### LibDock (2004)
- **LibScore**: Fragment-based scoring using molecular graph matching
- Faster than Surflex-Dock but lower accuracy
- Designed for ultra-large database screening

## Benchmark Datasets and the False Positive Problem

### Directory of Useful Decoys (DUD, 2005)
Kuhn et al. (J. Med. Chem. **48**(11): 4041–4048, 2005) created the first systematic benchmark:
- **10 protein targets**, each with ~70 known actives and ~1,000 structurally similar decoys
- Decoys matched physicochemical properties (MW, logP, H-bond donors/acceptors) but were not known binders
- **Key finding**: Most docking programs showed enrichment factors of 2–5× at 1% recovery, but false positive rates were high when decoy sets were more challenging

### DUD-E (Enhanced, 2010)
Mysinger et al. (J. Med. Chem. **54**(1): 2011) addressed DUD's flaws:
- **102 protein targets** with rigorously curated actives and decoys
- Decoys excluded compounds with substructures found in known actives
- **Key finding**: Enrichment factors dropped significantly compared to original DUD, revealing that many "successful" screens were artifacts of poor decoy design
- Average enrichment factor at 1% for Glide XP: ~5–10× (down from 15–20× on original DUD)

### CASF (Comparative Assessment of Scoring Functions)
Wang et al. (2005, 2008, 2013, 2016) established four evaluation criteria:
1. **Binding affinity prediction**: Correlation between score and experimental Kd/IC50
2. **Pose prediction**: Ability to reproduce crystallographic binding mode (RMSD < 2.0 Å)
3. **Cross-docked pose prediction**: Pose prediction when docking into a homologous structure
4. **Receiver operating characteristic (ROC)**: Enrichment of actives over decoys

**CASF-2013 results** (Wang et al., 2013):
- Glide XP ranked among top performers for pose prediction (~75% success rate)
- No scoring function achieved Pearson R > 0.7 for affinity prediction across the full diverse set
- Consensus scoring showed marginal improvement over individual scorers

## The False Positive Crisis

By the late 2000s, the field recognized a fundamental problem: **docking programs produced too many false positives in virtual screening**. Key observations:

1. **Enrichment factors were inflated by easy decoys**: Original DUD decoys were too dissimilar from actives, making enrichment artificially high. DUD-E corrected this, and enrichment factors dropped 2–3×.

2. **Scoring functions were co-linear**: Most scoring functions showed high pairwise correlation (R > 0.8), meaning consensus scoring provided limited improvement (Rosenman et al., 2008).

3. **Hydrophobic bias**: Empirical scorers over-rewarded hydrophobic contacts, producing false positives from promiscuous hydrophobic compounds that could not actually displace binding-site waters.

4. **Water was the missing variable**: Many false positives arose because scoring functions could not assess whether a ligand's hydrophobic group could actually displace high-energy waters from the binding site. Conversely, many true actives were missed because they required specific water-mediated hydrogen bonds.

This crisis drove the development of methods that explicitly modeled water thermodynamics, leading to WaterMap (2012), WScore (2015), and eventually Glide WS (2023).

## Key Publications

| Year | Authors | Title | Journal |
|------|---------|-------|---------|
| 2004 | Friesner RA et al. | Glide: A New Approach for Rapid, Accurate Docking and Scoring. 1 & 2 | J. Med. Chem. **47**(7) |
| 2005 | Kuhn B et al. | The Directory of Useful Decoys | J. Med. Chem. **48**(11) |
| 2006 | Friesner RA et al. | Extra Precision Glide: Docking and Scoring Incorporating a Model of Hydrophobic Enclosure | J. Med. Chem. **49**(21) |
| 2007 | Murray CW et al. |Evaluation of ChemPLP, a Hybrid Method for Identifying the Correct Binding Orientations of ligands in protein binding sites | J. Chem. Inf. Model. **47**(1) |
| 2008 | Wang R et al. | Research updates on the CASF-2007 scoring function | J. Comput. Aided Mol. Des. **22**(5) |
| 2010 | Mysinger MM et al. | Directory of Useful Decoys, Enhanced (DUD-E) | J. Med. Chem. **54**(1) |
| 2013 | Wang R et al. | CASF-2013 score: assessment of scoring power and pose-prediction accuracy | J. Comput. Aided Mol. Des. **27**(2) |

## Practical Implications

1. **Glide SP remains the workhorse** for high-throughput virtual screening due to its speed/accuracy tradeoff. It is still widely used in 2024 for initial triage of large compound libraries.

2. **Glide XP is the standard** for focused library docking and lead optimization where accuracy matters more than throughput.

3. **The false positive problem persists** in implicit-solvent methods. Modern practice uses Glide XP followed by WaterMap analysis to filter false positives, adding computational cost but improving hit rates.

4. **Benchmark choice matters**: DUD-E is now the standard benchmark; results from original DUD are considered inflated and should not be compared directly to modern methods.
