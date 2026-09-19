# The AmberTools Ecosystem for Nwat-MMGBSA

## Overview

AmberTools is the free, open-source software suite that powers the Nwat-MMGBSA protocol. It provides the molecular mechanics engine, trajectory analysis tools, and MM-GBSA energy evaluation that make explicit-water rescoring possible.

## Key Components

### Antechamber

Ligand parameterization tool. Generates GAFF atom types and AM1-BCC charges for arbitrary organic molecules.

Key capabilities:
- AM1-BCC charge calculation (1-5 seconds per ligand)
- GAFF/GAFF2 atom type assignment
- Bond order perception and formal charge assignment
- Output in multiple formats (mol2, prepi, frcmod)

### LEaP (tleap)

System assembly tool. Builds the complete solvated, ionized simulation system from component parts.

Key capabilities:
- Load force field libraries (ff14SB, GAFF, TIP3P)
- Solvate with truncated octahedron (most efficient shape for MD)
- Add counterions (neutralize + physiological salt concentration)
- Generate topology (.prmtop) and coordinate (.inpcrd) files

### pmemd.cuda

GPU-accelerated molecular dynamics engine. The computational workhorse of Nwat-MMGBSA.

Key capabilities:
- Explicit solvent MD with periodic boundary conditions
- Particle-mesh Ewald for long-range electrostatics
- Langevin thermostat for NVT ensemble
- SHAKE constraints on hydrogen bonds (enables 2 fs timestep)
- Near-linear scaling with GPU cores

### cpptraj

Trajectory analysis tool. The "closest" command is the defining operation of Nwat-MMGBSA.

Key capabilities:
- Per-frame water selection (closest N)
- RMSD and RMSF analysis for convergence checking
- Coordinate manipulation (centering, imaging, wrapping)
- Hydrogen bond analysis

### MMPBSA.py

Free energy evaluation tool. Computes MM-GBSA/MM-PBSA binding free energies from trajectory frames.

Key capabilities:
- Generalized Born models (igb=1,2,5,7,8)
- Poisson-Boltzmann (delphi, APBS)
- Normal mode entropy estimation
- Per-residue energy decomposition
- Parallel execution (MPI) for large trajectory files

## The Nwat-MMGBSA Pipeline with AmberTools

Complete pipeline in AmberTools commands:

```bash
# 1. Parameterize ligand
antechamber -i ligand.mol2 -fi mol2 -o ligand_gaff.mol2 -fo mol2 -c bcc -s 2 -nc 0
parmchk2 -i ligand_gaff.mol2 -f mol2 -o ligand.frcmod

# 2. Build system
tleap -f build_system.in

# 3. Run MD (NVT, 300K, 20ns)
pmemd.cuda -O -i prod.in -p complex.prmtop -c equil.rst -o prod.out -r prod.rst -x prod.nc

# 4. Select closest waters
cpptraj -p complex.prmtop << EOF
trajin prod.nc 1 last 10
closest 30 :LIG closestout closest.dat name NWAT
strip !(:LIG | :NWAT)
trajout stripped.nc
run
quit
EOF

# 5. MM-GBSA evaluation
MMPBSA.py -O -i mmpbsa.in -sp complex.prmtop -cp complex.prmtop \
  -rp receptor.prmtop -lp ligand.prmtop -y stripped.nc -o results.dat
```

## Version Compatibility

| Component | Required Version | Notes |
|---|---|---|
| AmberTools | 15+ | Earlier versions lack cpptraj improvements |
| AMBER | 14+ with pmemd.cuda | GPU MD required for throughput |
| cpptraj | 4.0+ | "closest" command added in v2.0 |

## Alternatives and Supplements

| Task | AmberTools Default | Alternative |
|---|---|---|
| Ligand charges | AM1-BCC | RESP (Gaussian/GAMESS), GAAMP |
| MD engine | pmemd.cuda | NAMD, GROMACS, OpenMM |
| Trajectory analysis | cpptraj | MDAnalysis, MDTraj, VMD |
| MM-GBSA | MMPBSA.py | gmx_MMPBSA, standalone scripts |

## References

- Case et al. (2023). "AmberTools." J. Chem. Inf. Model. 63(20): 6183-6191.
- Roe & Cheatham (2013). "PTRAJ and CPPTRAJ: Software for Processing and Analysis of Molecular Dynamics Trajectory Data." J. Chem. Theory Comput. 9(7): 3084-3095.
- Miller et al. (2012). "MMPBSA.py: An Efficient Program for End-State Free Energy Calculations." J. Chem. Theory Comput. 8(9): 3314-3321.
