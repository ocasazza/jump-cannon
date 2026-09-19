# Nwat-MMGBSA: Explicit-Water Rescoring for Docking

## Quick Reference

| Attribute | Value |
|---|---|
| **Primary source** | Maffucci et al., *Front. Chem.* **6**:43 (2018). doi:10.3389/fchem.2018.00043 |
| **Method type** | Post-docking rescoring via MD + MM-GBSA with explicit interfacial water |
| **Input** | Protein-ligand or protein-protein docking poses |
| **Output** | Rescored binding free energies (ΔG binding) |
| **ROC AUC gain** | +20 to 30% over docking scores; +17 to 35% over standard MM-GBSA (Nwat = 0) |
| **GPU throughput** | ~1.5 h per compound on single GeForce GTX TITAN Black (~20 compounds/day on one GPU) |
| **Key tunable** | Nwat (number of closest waters retained per frame) |
| **Default ensemble** | NVT at 300 K |
| **Implicit solvent** | GB-Neck2 (igb = 8), salt concentration 0.15 M |
| **Published** | 5 March 2018 |

## What It Is

Nwat-MMGBSA is a variant of the molecular mechanics Generalized Born surface
area (MM-GBSA) method. It adds a fixed number of the closest explicit water
molecules to the receptor in each frame of a molecular dynamics trajectory.
"Closest" means closest to the ligand in every frame.

This makes it a hybrid explicit/implicit solvation approach: the bulk solvent
is modeled implicitly through GB, but the critical first-shell water molecules
are treated as part of the receptor. This captures water-mediated
ligand-receptor interactions that purely implicit methods miss.

## Why It Matters

Standard MM-GBSA uses only implicit solvent. This averages away the energetic
contribution of individual water molecules that bridge ligand and receptor
contacts. Since roughly two-thirds of crystallographic complexes show at least
one water-mediated contact between binding partners (Hendlich et al., 2003),
losing those contributions degrades correlation with experiment.

Nwat-MMGBSA recovers them without requiring high-resolution crystal structures
to pre-identify conserved waters. It selects waters dynamically from each frame
of the MD trajectory, so it works equally well on homology models and NMR
structures.

## Key Findings

### ROC AUC Improvement

In retrospective virtual screening of AmpC β-lactamase (protein-ligand)
and Rac1-Tiam1 (protein-protein), Nwat-MMGBSA rescoring provided:

| System | Docking AUC | Nwat=0 (std MM-GBSA) | Nwat=30 | Nwat=60 | Nwat=100 |
|---|---|---|---|---|---|
| AmpC β-lactamase | 0.72 | 0.76 | 0.88** | 0.88*** | — |
| Rac1-Tiam1 (PPI) | 0.59 | 0.56 | 0.53 | 0.71* | 0.76** |

- Δ% vs docking: +4.9% (Nwat=30), +22.7% (Nwat=60) for AmpC
- Δ% vs standard MM-GBSA: +17.0% (Nwat=30), +16.0% (Nwat=60) for AmpC
- Δ% vs docking for Rac1: +29.1% (Nwat = 100)
- Δ% vs standard MM-GBSA for Rac1: +34.7% (Nwat = 100)

Statistical significance (t-test): *P < 0.05; **P < 0.01; ***P < 0.001

### Protocol Optimization Results

Three test systems were used to optimize the protocol:

| System | r² at Nwat=0 | Best r² | Best Nwat | Key finding |
|---|---|---|---|---|
| Penicillopepsin | ~0.3 | ~0.8 | 10–100 | Large gain from any Nwat > 0 |
| HIV-1 protease | ~0.3–0.5 | ~0.6–0.7 | 30–70 | Needs substantial hydration shell |
| BCL-XL | ~0.7 | ~0.7 | N/A | No degradation when waters don't matter |

### GPU vs CPU

- **Tesla C1060 (2010)**: 8.7 ns/day on a Rac1 complex
- **GeForce GTX TITAN Black (~2014)**: 59.3 ns/day on the same system
- **Full pipeline** (parameterization + minimization + equilibration + 1 ns
  production + Nwat-MMGBSA): ~1.5 h per compound on single GPU
- **HPC equivalent**: 12 nodes × 2 octa-core processors gave same wall-clock
  time
- **Statistical equivalence**: triplicate GPU and CPU runs showed no
  significant difference in correlations

### Trajectory Length

No relevant differences in correlation to experiments were observed between
analyses performed on 1 ns and 4 ns trajectories. This makes the method
practical for medium-throughput applications.

## Methodological Choices

### Why NVT Instead of NPT

The production run uses the NVT (constant Number, Volume, Temperature)
ensemble at 300 K rather than NPT (constant Pressure). This provides a **30%
reduction in overall MD simulation time** without significant variation in
results, as demonstrated by the authors.

The mathematical rationale is that MM-GBSA energies are evaluated on
individual trajectory frames. NPT allows the box volume to fluctuate, which
can introduce artifacts in the GB energy when the solute cavity radius
implicitly depends on system density. NVT holds the volume constant, giving
more consistent GB energies across frames. Additionally, after proper NPT
equilibration, the equilibrium density reached in NPT is preserved in the
subsequent NVT production run.

### Why AM1-BCC Charges

AM1-BCC (Austin Model 1 - Bond Charge Correction) is a semi-empirical QM
method that generates partial charges in seconds per ligand. It has been shown
to behave comparably to more sophisticated methods (RESP at HF/6-31G* level)
in MM-PB/GBSA calculations (Xu et al., 2013; Sun et al., 2014). This is
critical for virtual screening throughput: RESP charges from full DFT
calculations would take hours per compound, defeating the purpose of
medium-throughput rescoring.

### Why GB Instead of PB

Generalized Born (GB) implicit solvent is used by default rather than
Poisson-Boltzmann (PB). GB provides outcomes comparable to PB at a fraction of
the computational cost, especially when relatively short MD trajectories are
used for MM-PB/GBSA calculations (Hou et al., 2011a,b; Maffucci & Contini,
2013, 2015, 2016). The GB-Neck2 model (igb = 8) was specifically chosen.
PB can be requested by the user if desired.

### Neglect of Entropy

Entropy contributions are deliberately neglected. The benefits of including
entropy remain controversial (Weis et al., 2006; Hou et al., 2011a; Wallnoefer
et al., 2011; Yang et al., 2011), and normal mode calculations are extremely
time-consuming. Neglecting entropy is acceptable when comparing ligands of
similar size and structure, though it may introduce errors for structurally
diverse ligand sets.

## The Full Workflow

The complete protocol is implemented as three coordinated scripts:

1. **VScreen** — Library preparation, tautomer/stereoisomer
   generation, docking with PLANTS
2. **autoMD** — Automated MD simulation setup and execution
3. **Nwat-MMGBSA** — Trajectory processing, closest water
   selection, MM-GBSA energy evaluation

Each script is independent but passes outputs sequentially. The system is
fully automatic from library set-up through final rescored rankings.

### Step-by-Step

1. **Library preparation**: Generate tautomers, stereoisomers, ring
   conformations, and protonation states (UNICON + SPORES)
2. **Docking**: PLANTS with CHEMPLP scoring function, speed 1 (high accuracy)
3. **Top-N selection**: Select top percentile of ranked ligands for rescoring
4. **Ligand parameterization**: Antechamber with AM1-BCC charges, GAFF atom
   types
5. **Complex preparation**: tleap with ff14SB (protein) + GAFF (ligand),
   neutralize, solvate with TIP3P (10 Å buffer)
6. **MD simulation** (per complex):
   - Hydrogen-only minimization (1000 SD + 5000 CG, restraints on heavy atoms)
   - Solvent equilibration: 100 ps NVT + 100 ps NPT at 300 K (restraints on solute)
   - Backbone-restrained minimization (2500 SD + 5000 CG × 2 cycles, decreasing restraints)
   - Heating: 6 steps of 5 ps each (50→300 K, decreasing restraints)
   - Equilibration: 1.6 ns (100 ps NVT + 1200 ps NPT with stepwise restraint reduction + 500 ps unrestrained NVT)
   - Production: NVT at 300 K, 1–4 ns
7. **Trajectory processing**: cpptraj extracts 100 frames per ns (interval = 10), selects Nwat closest water molecules to ligand per frame
8. **MM-GBSA analysis**: MMPBSA.py with GB-Neck2, 0.15 M salt, entropy neglected
9. **Ranking**: Ligands ranked by calculated ΔG binding; ROC AUC computed

## Applicability and Limitations

### When Nwat-MMGBSA Helps

- Protein-ligand systems where water mediates contacts (penicillopepsin, HIV-1 protease)
- Protein-protein interactions (Rac1-Tiam1)
- Systems without high-resolution crystal structures (works on homology models and NMR structures)
- Hit-to-lead and lead optimization phases of drug discovery

### When It Does Not

- Systems where water does not play a specific bridging role (BCL-XL). However, applying Nwat-MMGBSA is not detrimental — it simply does not improve over standard MM-GBSA.
- No universal "ideal Nwat" exists: Nwat = 30 works for many systems, but large, solvent-exposed sites may need 60–100 waters; narrow sites with small interfaces may need only 10

### Throughput

Processing more than 20 compounds per day on a single-GPU workstation (octa-core + one GPU). While slower than docking (thousands/day), it is orders of magnitude faster than alchemical free energy methods. The authors note that GPU hardware improvement should make MD-based rescoring even faster.

## Relationship to Other Methods

- **Standard MM-GBSA (Nwat = 0)**: The baseline; Nwat-MMGBSA improves correlation by up to 50% in favorable cases
- **MM-PBSA with crystallographic waters**: Requires high-resolution crystal structures; Nwat-MMGBSA does not
- **WScore / WaterMap / Grid inhomogeneous solvation theory (GIST)**: Computationally more expensive; evaluate water thermodynamics at each grid point rather than using explicit waters in MM-GBSA
- **Alchemical free energy (FEP/TI)**: Gold standard for accuracy but impractical for rescoring hundreds of compounds
- **Aldeghi et al. (2017)** independently validated Nwat-MMGBSA on bromodomains with thorough statistical analysis

## Primary References

- **Maffucci I, Contini A.** (2016). Improved Computation of Protein-Protein Relative Binding Energies with the Nwat-MMGBSA Method. *J. Chem. Inf. Model.* 56:1692–1704. doi:10.1021/acs.jcim.6b00196
- **Maffucci I, Hu X, Fumagalli V, Contini A.** (2018). An Efficient Implementation of the Nwat-MMGBSA Method to Rescore Docking Results in Medium-Throughput Virtual Screenings. *Front. Chem.* 6:43. doi:10.3389/fchem.2018.00043
- **Aldeghi M, Bodkin MJ, Knapp S, Biggin PC.** (2017). Statistical analysis on the performance of MMPBSA versus absolute binding free energy calculations: bromodomains as a case study. *J. Chem. Inf. Model.* 57:2203–2221. doi:10.1021/acs.jcim.7b00346
