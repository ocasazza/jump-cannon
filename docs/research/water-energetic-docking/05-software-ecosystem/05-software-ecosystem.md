# Software Ecosystem for Water-Energetic Molecular Docking

## Executive Summary

Water-energetic molecular docking is not a single tool — it is a **software pipeline** that combines commercial and open-source tools across multiple stages: protein and ligand preparation, conformational search with explicit water, molecular dynamics sampling, and physics-based rescoring. This document maps the complete software ecosystem, the role of each tool at each pipeline stage, and the operational requirements for running the workflow at production scale.

The ecosystem divides into four layers:

1. **Schrödinger Platform** — Maestro GUI, Glide WS docking engine, WaterMap thermodynamics, LiveDesign collaboration
2. **AmberTools Stack** — Antechamber (ligand parameterization), cpptraj (trajectory analysis), MMPBSA.py (binding free energy)
3. **Alternative Docking Tools** — PLANTS, SPORES, UNICON — complementary and open-source docking engines
4. **Pipeline Automation** — VScreen, autoMD, and custom workflow orchestration for sustained discovery campaigns

## Architecture Overview

```
┌─────────────────────────────────────────────────────────────────┐
│                     PIPELINE AUTOMATION LAYER                     │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌──────────────────┐ │
│  │  VScreen  │  │  autoMD   │  │  KNIME   │  │  Custom Python   │ │
│  │  (Schröd) │  │  (Amber)  │  │  (generic)│  │  (Luigi/Airflow)│ │
│  └────┬─────┘  └────┬─────┘  └────┬─────┘  └────────┬─────────┘ │
│       └──────────────┴─────────────┴─────────────────┘           │
└──────────────────────────────────────────────────────────────────┘
                              │
          ┌───────────────────┼───────────────────┐
          ▼                   ▼                   ▼
┌─────────────────┐  ┌─────────────────┐  ┌─────────────────────┐
│   SCHRÖDINGER    │  │   AMBERTOOLS    │  │ ALTERNATIVE DOCKING │
│   PLATFORM       │  │   STACK         │  │                     │
│                  │  │                 │  │  PLANTS (protein)   │
│  Maestro GUI     │  │  Antechamber    │  │  SPORES (surface)   │
│  Glide WS        │  │  tleap          │  │  UNICON (universal) │
│  WaterMap        │  │  sander/pmemd   │  │                     │
│  LiveDesign      │  │  cpptraj        │  └─────────────────────┘
│                  │  │  MMPBSA.py      │
└─────────────────┘  └─────────────────┘
```

## Pipeline Stages and Tool Assignments

### Stage 1: Protein Preparation

| Tool | Role | Input | Output |
|------|------|-------|--------|
| **Maestro Protein Preparation Wizard** | Add hydrogens, assign protonation states, optimize H-bond network, restrained minimization | PDB structure | Prepared `.mae` / `.maegz` |
| **Maestro** (manual) | Visual inspection of binding site, selection of grid center | Prepared structure | Grid definition |
| **PropKa** (embedded) | pKa prediction for titratable residues at target pH | Structure + pH | Protonation assignments |

**Operational notes**: Protein Preparation Wizard uses OPLS4 force field (Schrödinger's proprietary force field). The wizard runs a series of automated steps: bond order assignment, hydrogen addition, zero-order bond optimization, H-bond network optimization (sampling SER/THR/TYR hydroxyls, ASN/GLN amides, HIS tautomers), and restrained minimization to RMSD 0.30 Å. Typical runtime: 1–5 minutes per structure.

### Stage 2: Water Thermodynamics Mapping

| Tool | Role | Input | Output |
|------|------|-------|--------|
| **WaterMap** | GCMC simulation of binding site waters | Prepared protein (no ligand) | Hydration sites with ΔG, ΔH, −TΔS |
| **Maestro** (visualization) | Color-coded water sites (red = displaceable, blue = structural) | WaterMap results | Design guidance |

**Operational notes**: WaterMap requires a Desmond MD license. Typical runtime: 1–4 hours per binding site on a single GPU. The GCMC simulation uses a sphere of 10–15 Å radius centered on the binding site. Water molecules are inserted/deleted according to Metropolis criterion; chemical potential is calibrated to reproduce bulk water density (~0.033 molecules/Å³). The analysis produces 5–30 hydration sites, each with full thermodynamic decomposition.

### Stage 3: Ligand Preparation

| Tool | Role | Input | Output |
|------|------|-------|--------|
| **LigPrep** (Schrödinger) | Generate 3D conformations, tautomers, ionization states, stereoisomers | SMILES / SDF / 2D | 3D `.maegz` ligand library |
| **Epik** (embedded) | pKa prediction and ionization state enumeration | 2D structures | Ionization/protonation states at target pH |
| **Antechamber** (AmberTools) | Assign GAFF/GAFF2 atom types and AM1-BCC charges | MOL2 / PDB ligand | `ligand.prmtop`, `ligand.inpcrd` |

**Operational notes**: LigPrep is a critical quality step. For each input compound, it generates (at pH 7.4): up to 32 stereoisomers (if unspecified chirality), up to 8 tautomers, up to 4 ionization states (if ionizable groups present). This multiplies a 1M-compound library to potentially 10M+ ligand states. A typical production run uses `LigPrep -nt N` for parallel processing. Antechamber is used downstream for Amber-compatible topology files — the split workflow: Schrödinger for docking, Amber for rescoring.

### Stage 4: Docking with Explicit Water

| Tool | Role | Input | Output |
|------|------|-------|--------|
| **Glide WS** | Grid-based docking with flexible explicit waters | Grid, ligand library, WaterMap sites | Poses + WS scores (kcal/mol) |
| **Glide SP** (optional pre-screen) | Fast implicit-solvent docking to filter library | Grid, ligand library | Top N% candidates for WS refinement |
| **Glide XP** (optional refinement) | More rigorous scoring for WS candidates | WS poses | XP-refined poses + scores |

**Operational notes**: Glide WS is the computational bottleneck. The WS docking algorithm places up to 20 explicit water molecules per pose and treats them as flexible (translational + rotational degrees of freedom). Typical throughput: 10–100 compounds per CPU-hour depending on ligand flexibility. Production workflows use a funnel: SP screens 1M compounds → top 10% (100K) → XP refinement → top 1% (1K) → WS docking. Total runtime for a 1M-compound campaign: ~1–2 weeks on a 100-core cluster.

### Stage 5: Molecular Dynamics and Rescoring

| Tool | Role | Input | Output |
|------|------|-------|--------|
| **sander / pmemd** (Amber) | Explicit-solvent MD of protein-ligand complex | `complex.prmtop`, `complex.inpcrd` | MD trajectory (`.nc` / `.mdcrd`) |
| **pmemd.cuda** | GPU-accelerated MD | Same inputs | Same outputs (10–100× faster) |
| **cpptraj** (AmberTools) | Trajectory analysis: RMSD, RMSF, closest waters | Trajectory + topology | Selected frames, water indices |
| **MMPBSA.py** (AmberTools) | MM-GBSA binding free energy calculation | Trajectory snapshots | ΔG_bind per frame + statistics |

**Operational notes**: The MD+rescoring stage is the most computationally expensive. For the Nwat-MMGBSA protocol: 50 ns explicit-solvent MD per complex (GPU: ~24 hours per complex on V100), cpptraj selects N=30 closest waters per frame, MMPBSA.py computes ΔG_bind on 500–1000 equally spaced snapshots. The full protocol for 100 compounds requires ~100 GPU-days. This is why pre-filtering with Glide WS is essential — only the top-ranked candidates advance to MD rescoring.

### Stage 6: Analysis and Decision

| Tool | Role | Input | Output |
|------|------|-------|--------|
| **Maestro** | Visual inspection of top poses, water networks | `.maegz` poses + trajectory | Go/no-go decisions |
| **LiveDesign** | Collaborative review, SAR analysis, compound tracking | All docking results | Prioritized compound list |
| **Python/Jupyter** | Custom analysis, enrichment plots, ROC curves | `.csv` score tables | Figures, reports |

---

## 1. Schrödinger Platform

### 1.1 Maestro — The Graphical Environment

Maestro is Schrödinger's unified graphical interface for all computational chemistry workflows. It is the primary working environment for computational chemists in the pharmaceutical industry.

**Core capabilities for the water-energetic docking pipeline:**

| Module | Function |
|--------|----------|
| **Protein Preparation Wizard** | Automated structure preparation (H-bonds, protonation, minimization) |
| **Receptor Grid Generation** | Define docking grid from prepared structure |
| **LigPrep / LigFilter** | Ligand library preparation and property filtering |
| **Glide (SP/XP/WS)** | Docking job setup, submission, and results analysis |
| **WaterMap** | Water thermodynamics analysis and visualization |
| **Desmond** | MD simulation setup (via Maestro interface) |
| **Canvas** | Cheminformatics: fingerprint-based similarity, clustering, diversity selection |
| **Phase** | Pharmacophore modeling and 3D QSAR |

**Operational architecture**: Maestro runs as a desktop application (Linux, macOS, Windows) that connects to a Schrödinger license server. Jobs are submitted to a job server or cluster (via `$SCHRODINGER/utilities/jobcontrol`). The GUI is built on Qt and uses a project-table paradigm: compounds are rows, properties are columns, and all computational results populate columns automatically.

**Key file formats**:
- `.mae` / `.maegz` — Maestro structure format (proprietary, compressed)
- `.sdf` — MDL SDfile (interoperable)
- `.pdb` — Protein Data Bank format
- `.csv` — Tabular property export/import

### 1.2 Glide WS — Water Docking Engine

Glide WS is the core computational engine for water-aware docking. It is a proprietary, closed-source module within the Schrödinger suite.

**Algorithmic features** (from Friesner et al., 2004–2023):

1. **Grid-based scoring**: Pre-computes atom-type interaction grids (vdW, electrostatic, hydrophobic, hydrogen bond) on a cubic lattice with 0.25–0.50 Å spacing
2. **Hierarchical pose search**: Three-stage funnel — site-point search → refinement → minimization — progressively narrows the pose space
3. **Flexible explicit waters**: Up to 20 water molecules per pose, each with 3 translational + 3 rotational degrees of freedom, sampled during the docking search
4. **Water scoring function**: Modified empirical scoring function that includes water-protein, water-ligand, and water-water interaction terms
5. **Water displacement energy**: Derived from WaterMap thermodynamics — penalizes displacement of "happy" (low ΔG) waters, rewards displacement of "unhappy" (high ΔG) waters

**License requirements**: Glide WS requires both a Glide license and a WaterMap license (which includes Desmond). This is a commercial, per-core licensed product.

**Command-line interface** (typical invocation):
```bash
$SCHRODINGER/glide ws_job.in -HOST cluster:100 -WAIT
```

### 1.3 LiveDesign — Collaborative Drug Design Platform

LiveDesign is Schrödinger's web-based platform for collaborative drug discovery. It serves as the "data backbone" for multi-user discovery campaigns.

**Role in the pipeline**:
- **Compound registration**: Track compounds from idea to synthesis
- **Data aggregation**: All docking scores, MD results, and experimental data in one searchable database
- **SAR analysis**: Matched molecular pair analysis, R-group decomposition
- **Workflow integration**: LiveDesign's REST API enables automated data push from pipeline scripts
- **Visualization**: Web-based 3D viewer for protein-ligand complexes with pose overlay

**API integration point**: LiveDesign exposes a full REST API. Pipeline automation scripts can:
```python
import requests
# POST docking results to LiveDesign
requests.post(
    f"{LD_URL}/api/compounds/{compound_id}/assay-data",
    json={"assay_name": "Glide_WS_Score", "value": ws_score},
    headers={"Authorization": f"Bearer {ld_token}"}
)
```

### 1.4 Licensing and Infrastructure

Schrödinger products are commercially licensed. Typical pharmaceutical deployment:
- **Token-based licensing**: CPU-hours consumed across all products
- **Cluster deployment**: `$SCHRODINGER` installed on shared NFS filesystem
- **Job control**: `$SCHRODINGER/jobcontrol` manages cluster submission (SGE, LSF, SLURM)
- **Environment**: `$SCHRODINGER/run` wrapper script sets library paths

---

## 2. AmberTools Stack

AmberTools is the free, open-source companion to the Amber MD suite. It provides the ligand parameterization, trajectory analysis, and binding free energy tools that complete the water-energetic docking pipeline.

### 2.1 Antechamber — Ligand Parameterization

Antechamber is the AMBER tool for assigning atom types and charges to non-standard residues (ligands, cofactors, modified amino acids).

**Role in the pipeline**: Convert Schrödinger-docked ligands to Amber-compatible topology files for MD simulation.

**Workflow**:
```bash
# 1. Export docked pose from Maestro as MOL2
# 2. Run antechamber to assign GAFF2 atom types
antechamber -i ligand.mol2 -fi mol2 -o ligand.mol2 -fo mol2 \
    -c bcc -nc 0 -at gaff2 -pf yes

# 3. Generate Amber topology and coordinate files
parmchk2 -i ligand.mol2 -f mol2 -o ligand.frcmod
tleap -f leap.in  # Combines ligand + protein + solvent
```

**Key parameters**:
- `-c bcc`: AM1-BCC charge method — semi-empirical AM1 calculation followed by bond charge correction. This is the standard charge model for GAFF/GAFF2 ligands. Runtime: seconds to minutes per ligand.
- `-at gaff2`: GAFF2 (General Amber Force Field 2) — improved version with better torsion parameters vs. GAFF1.
- `-nc N`: Net charge of the ligand.
- `-pf yes`: Remove intermediate files after completion.

**Operational considerations**: AM1-BCC charge calculation requires `mopac.sh` or `sqm` (semi-empirical QM program, included in AmberTools). For large ligands (>100 heavy atoms), AM1 may fail — fall back to RESP charges from Gaussian or use the `-c rc` (RESP charge) option with precomputed charges.

### 2.2 tleap — System Building

`tleap` (and its graphical variant `xleap`) builds the complete simulation system: protein + ligand + water box + ions.

**Typical `leap.in` for Nwat-MMGBSA**:
```tcl
source leaprc.protein.ff19SB    # Protein force field
source leaprc.gaff2             # Ligand force field
source leaprc.water.tip3p       # Water model

# Load ligand parameters
loadamberparams ligand.frcmod
LIG = loadmol2 ligand.mol2

# Load protein
complex = loadpdb protein_ligand.pdb

# Solvate
solvatebox complex TIP3PBOX 12.0  # 12 Å buffer

# Add ions
addions complex Na+ 0
addions complex Cl- 0

# Save
saveamberparm complex complex.prmtop complex.inpcrd
quit
```

### 2.3 pmemd.cuda — GPU-Accelerated MD

`pmemd.cuda` is the GPU-accelerated MD engine in AMBER. It is the production simulation engine for the rescoring stage.

**Performance**: On an NVIDIA V100, explicit-solvent MD of a ~50K-atom system (protein + ligand + TIP3P water) achieves ~100–200 ns/day. A typical 50 ns production run completes in 6–12 hours.

**Nwat-MMGBSA production simulation**:
```bash
pmemd.cuda -O \
    -i md_production.in \
    -p complex.prmtop \
    -c equilibration.rst7 \
    -o md_output.out \
    -x trajectory.nc \
    -r final.rst7 \
    -inf md_info.inf
```

Key MD parameters (`md_production.in`):
```
&cntrl
  imin=0,              ! MD (not minimization)
  ntx=5, irest=1,      ! Restart from previous run
  nstlim=10000000,     ! 10M steps = 20 ns at 2 fs timestep
  dt=0.002,            ! 2 fs timestep
  ntt=3, temp0=300.0,  ! Langevin thermostat at 300K
  gamma_ln=2.0,        ! Collision frequency
  ntp=1, pres0=1.0,    ! Berendsen barostat at 1 atm
  ntc=2, ntf=2,        ! SHAKE on H-bonds
  ntpr=5000, ntwx=5000,! Output every 5000 steps
  cut=10.0,            ! 10 Å nonbonded cutoff
  iwrap=1,             ! Wrap coordinates into primary box
/
```

### 2.4 cpptraj — Trajectory Analysis

`cpptraj` is the AMBER trajectory analysis tool. In the Nwat-MMGBSA pipeline, it performs the critical "closest water" selection.

**Nwat-MMGBSA closest-water selection**:
```bash
cpptraj -p complex.prmtop << EOF
trajin trajectory.nc 1 last 100     # Every 100th frame
trajout stripped.nc                 # Output stripped trajectory

# Select N closest waters to ligand
closest 30 :WAT closestout closest_waters.dat name ClosestWaters

# Strip everything except protein, ligand, and closest waters
strip !(:LIG,ClosestWaters)
run
EOF
```

**The `closest` command**: For each frame, cpptraj computes distances from every water oxygen to every ligand atom. It selects the N waters with the minimum distance. This is the spatial nearest-neighbor problem — analogous to jump-cannon's Barnes-Hut octree traversal.

**Additional cpptraj analyses**:
```bash
# RMSD of ligand relative to docking pose
rmsd LIG first out rmsd_ligand.dat

# RMSF (flexibility) of binding site residues
atomicfluct out rmsf.dat :1-300 byres

# Hydrogen bond analysis
hbond HBOND out hbond.dat :LIG distance 3.5 angle 120.0
```

### 2.5 MMPBSA.py — Binding Free Energy

`MMPBSA.py` computes the binding free energy using the MM-GBSA or MM-PBSA continuum solvent models on snapshots from the MD trajectory.

**Nwat-MMGBSA protocol input**:
```bash
MPI="mpirun -np 16"  # Parallel across snapshots
$MPI MMPBSA.py -O \
    -i mmpbsa.in \
    -o results.dat \
    -sp complex.prmtop \
    -cp complex.prmtop \
    -rp receptor.prmtop \
    -lp ligand.prmtop \
    -y stripped.nc
```

**Key MM-GBSA parameters** (`mmpbsa.in`):
```
&general
  startframe=1, endframe=500,   # 500 snapshots
  interval=1,
  verbose=1,
/
&gb
  igb=8,                         # GB-Neck2 model (optimal for proteins)
  saltcon=0.150,                 # 150 mM salt
/
```

**Output**: For each snapshot, MMPBSA.py reports:
- `ΔG_VDWAALS` — van der Waals contribution
- `ΔG_EEL` — electrostatic contribution
- `ΔG_EGB` — polar solvation (Generalized Born)
- `ΔG_ESURF` — nonpolar solvation (SASA-based)
- `ΔG_GAS` — gas-phase energy (VDWAALS + EEL)
- `ΔG_SOLV` — solvation energy (EGB + ESURF)
- `ΔG_TOTAL` — total binding free energy

**Parallelization**: MMPBSA.py parallelizes across snapshots using MPI. For 500 snapshots on 16 cores, runtime is ~2–4 hours (GB calculation dominates). GPU acceleration for GB is available via `igb=8` and CUDA-aware MPI.

**Statistical output**:
```
RESULTS:
                         Mean      Std. Dev.   Std. Err. of Mean
-----------------------------------------------------------------------
VDWAALS:               -45.23       3.12           0.14
EEL:                   -28.67       8.45           0.38
EGB:                    35.12       6.78           0.30
ESURF:                  -5.89       0.34           0.02
DELTA G gas:           -73.90       9.67           0.43
DELTA G solv:           29.23       6.56           0.29
DELTA G total:         -44.67       5.89           0.26
```

---

## 3. Alternative Docking Tools

While the Schrödinger+Amber workflow is the gold standard, several alternative docking tools provide complementary capabilities or open-source alternatives for specific stages.

### 3.1 PLANTS — Protein-Ligand ANT System

**Developer**: University of Konstanz (Korb et al., 2006–2014)
**License**: Free for academic use; commercial license available

**Method**: PLANTS uses Ant Colony Optimization (ACO) for pose search. Virtual ants deposit pheromone on favorable ligand conformations, and the colony converges on the global minimum.

**Relevance to water-energetic docking**:
- **Speed**: Extremely fast — can dock 1M compounds per day on a single workstation (CPU). This makes it a viable pre-screen before more expensive water-aware methods.
- **Scoring**: ChemPLP (piecewise linear potential) — an empirical scoring function. No explicit water treatment, but the speed allows ensemble docking (docking to multiple receptor conformations).
- **Ensemble docking strategy**: Dock to 5–10 receptor conformations (from MD or NMR), then score by consensus. This indirectly captures some water effects because different receptor conformations have different water networks.

**Command-line example**:
```bash
PLANTS1.2_64bit \
    --mode screen \
    --protein protein.mol2 \
    --ligands ligands.mol2 \
    --chemplp \
    --speed SPEED1 \
    --res-dir results/
```

**Integration**: PLANTS can be used as a fast pre-screen. Output `.mol2` poses can be fed into Antechamber for Amber-compatible topology generation, then advanced to MMPBSA.py for water-aware rescoring.

### 3.2 SPORES — Structure-Based Protein-Ligand Virtual Screening

**Developer**: University of Hamburg (ten Brink et al., 2014)
**License**: Free for academic use

**Method**: SPORES focuses on **binding site characterization** rather than docking. It identifies:
- Subpockets and surface features
- Water-accessible regions
- Hydrogen bond donor/acceptor hotspots

**Relevance to water-energetic docking**:
- **Binding site decomposition**: Identifies subpockets, which can be mapped to WaterMap hydration sites
- **Water network analysis**: The surface analysis implicitly identifies regions where water molecules are likely to be structural
- **Pocket complementarity**: Quantifies how well a ligand fills each subpocket — a proxy for water displacement efficiency

**Use case**: SPORES analysis before docking can guide which waters are likely displaceable and which subpockets should be targeted — complementing WaterMap thermodynamics with geometric analysis.

### 3.3 UNICON — Universal Conformer Generator

**Developer**: Schrödinger (but available as a standalone tool from some academic collaborations)
**Role**: Conformer generation for ligand libraries

**Method**: UNICON generates a diverse set of low-energy 3D conformations for each ligand, sampling:
- Torsional degrees of freedom
- Ring conformations (pucker, chair/boat)
- Stereochemistry

**Relevance to water-energetic docking**:
- Conformer quality directly affects docking quality. UNICON generates conformers with RMSD < 0.5 Å to the crystal structure in >90% of cases (benchmarked on CSD).
- The Glide WS search starts from these pre-generated conformers, so conformer coverage determines whether the true binding mode can be found.

### 3.4 Comparison Matrix

| Feature | Glide WS (Schrödinger) | PLANTS | AutoDock Vina | GOLD |
|---------|----------------------|--------|---------------|------|
| **Explicit waters** | Yes (up to 20 flexible) | No | No | No |
| **Water scoring** | Yes (WaterMap-based) | No | No | No |
| **Speed** | Medium (10–100/CPU-hr) | Very fast (100K+/day) | Fast (1K+/CPU-hr) | Medium |
| **License** | Commercial | Academic free | Open source | Commercial |
| **Scoring function** | Empirical (modified for H₂O) | ChemPLP | Vina empirical | GoldScore/ChemScore |
| **Ensemble docking** | Supported | Supported (fast enough) | Limited | Supported |
| **Protein flexibility** | None (rigid receptor) | None | None | Side-chain libraries |
| **GPU acceleration** | No (CPU only) | No | No | No |

---

## 4. Pipeline Automation

See the [Pipeline Automation](pipeline_automation/pipeline_automation.md) document for a comprehensive treatment of workflow automation strategies, including:

- **VScreen** — Schrödinger's graphical virtual screening pipeline builder
- **autoMD** — Automated MD simulation and MM-GBSA rescoring workflow
- **Custom Python orchestration** — Luigi/Airflow-based pipeline for production campaigns
- **Containerization** — Docker/Singularity images for reproducibility
- **Scaling strategies** — From single-workstation to cluster/cloud deployment

---

## Cross-References

This document maps the software tools. For the scientific methodology they implement, see:
- **[01-docking-evolution](../01-docking-evolution/01-docking-evolution.md)** — Historical development of docking methods
- **[02-glide-ws](../02-glide-ws/)** — Glide WS deep dive (calibration, WaterMap integration, FEP)
- **[03-nwat-mmgb-sa](../03-nwat-mmgb-sa/)** — Nwat-MMGBSA protocol details (charge models, water selection, GPU)
- **[04-benchmarks](../04-benchmarks/04-benchmarks.md)** — Target-specific performance and jump-cannon algorithm mapping
- **[06-future-directions](../06-future-directions/)** — ML scoring functions, DiffDock, foundation models

---

*Document status: Draft — awaiting AI-Q research expansion on specific tool versions and cluster deployment patterns.*
