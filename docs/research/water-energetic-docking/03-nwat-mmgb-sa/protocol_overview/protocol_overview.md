# Nwat-MMGBSA Protocol: Step-by-Step Implementation

## Complete Protocol

### Input Requirements

- Protein-ligand complex structure (PDB format)
- Docked pose or co-crystal structure
- Access to AmberTools15+ and AMBER (pmemd.cuda for GPU)

### Step 1: Structure Preparation (MOE/SPORES)

Before parameterization, the structure must be prepared:

1. **Protonation state assignment**: MOE (Molecular Operating Environment) Protonate3D or SPORES for correct tautomer/protonation states at pH 7.4
2. **Stereoisomer check**: Chiral centers must have correct stereochemistry
3. **Missing atoms**: Terminal residues, flexible loops — model with MOE or SPORES
4. **Histidine protonation**: Critical for active-site histidines; use MOE to assign HID/HIE/HIP based on local environment

### Step 2: Ligand Parameterization (Antechamber)

```bash
# Generate AM1-BCC charges and GAFF parameters
antechamber -i ligand.mol2 -fi mol2 -o ligand_gaff.mol2 -fo mol2 \
  -c bcc -s 2 -nc <net_charge>

# Generate force field modification file
parmchk2 -i ligand_gaff.mol2 -f mol2 -o ligand.frcmod
```

The AM1-BCC charge method:
1. Runs AM1 semi-empirical QM calculation to get Mulliken charges
2. Applies bond charge correction (BCC) to map Mulliken → HF/6-31G* quality
3. Total time: ~1–5 seconds per ligand (vs. hours for RESP at HF/6-31G*)

### Step 3: System Assembly (LEaP)

```tcl
# tleap input
source leaprc.protein.ff14SB
source leaprc.gaff
source leaprc.water.tip3p

# Load protein and ligand
complex = loadPDB protein_ligand.pdb
loadAmberParams ligand.frcmod
loadMol2 ligand_gaff.mol2

# Solvate
solvateBox complex TIP3PBOX 10.0

# Add ions (neutralize + 150 mM NaCl)
addIons complex Na+ 0
addIons complex Cl- 0

# Save topology and coordinates
saveAmberParm complex complex.prmtop complex.inpcrd
quit
```

### Step 4: Minimization and Equilibration

```bash
# Minimization (two stages: solvent only, then all atoms)
pmemd.cuda -O -i min1.in -p complex.prmtop -c complex.inpcrd \
  -ref complex.inpcrd -o min1.out -r min1.rst

# Heating (NVT, 0 → 300 K over 50 ps)
pmemd.cuda -O -i heat.in -p complex.prmtop -c min1.rst \
  -o heat.out -r heat.rst -x heat.nc

# Equilibration (NVT, 300 K, 1–5 ns)
pmemd.cuda -O -i equil.in -p complex.prmtop -c heat.rst \
  -o equil.out -r equil.rst -x equil.nc
```

### Step 5: Production MD

```bash
# Production (NVT, 300 K, 5–20 ns, 2 fs timestep)
pmemd.cuda -O -i prod.in -p complex.prmtop -c equil.rst \
  -o prod.out -r prod.rst -x prod.nc
```

Input file (`prod.in`):
```
Production MD, NVT, 300 K
 &cntrl
   imin=0, ntx=5, irest=1,
   nstlim=10000000, dt=0.002,    ! 20 ns, 2 fs timestep
   ntt=3, temp0=300.0, gamma_ln=2.0,  ! Langevin thermostat
   ntc=2, ntf=2,                       ! SHAKE on H-bonds
   ntpr=5000, ntwx=5000,               ! Output every 10 ps
   ntwr=50000,                         ! Restart every 100 ps
   cut=10.0,                           ! Nonbonded cutoff
   iwrap=1,                            ! Wrap coordinates
 /
```

### Step 6: Water Selection (cpptraj)

```bash
# Use the "closest" command to select N waters per frame
cpptraj -p complex.prmtop << EOF
trajin prod.nc 1 last 10          # Use every 10th frame
closest 30 :LIG closestout closest.dat name NWAT
strip !(:LIG | :NWAT)
trajout stripped.nc
run
quit
EOF
```

Key parameters:
- `30`: N — number of waters to select
- `:LIG`: Mask for the ligand residue
- `closestout`: Records which waters were selected each frame
- `name NWAT`: Labels the selected waters for the strip command

### Step 7: MM-GBSA Energy Evaluation

```bash
# MMPBSA.py input file
cat > mmpbsa.in << EOF
&general
  startframe=1, endframe=1000,
  interval=1,
  verbose=2,
  keep_files=0,
/
&gb
  igb=5,                    ! GB model (OBC2)
  saltcon=0.150,            ! 150 mM salt
  surften=0.0072,           ! Surface tension (kcal/mol/Å²)
  surfoff=0.0,              ! Surface offset
  molsurf=0,                ! LCPO surface area
/
EOF

MMPBSA.py -O -i mmpbsa.in \
  -sp complex.prmtop \
  -cp complex.prmtop -rp receptor.prmtop -lp ligand.prmtop \
  -y stripped.nc \
  -o mmpbsa_results.dat
```

### Step 8: Analysis

```python
import numpy as np
import pandas as pd

# Parse MMPBSA.py output
results = pd.read_csv('mmpbsa_results.dat', sep=r'\s+')

# Compute ensemble average and statistics
delta_g = results['DELTA_TOTAL'].values  # kcal/mol
mean_dg = np.mean(delta_g)
std_dg = np.std(delta_g)
sem_dg = std_dg / np.sqrt(len(delta_g))

print(f"ΔG_bind = {mean_dg:.2f} ± {sem_dg:.2f} kcal/mol")
print(f"Range: [{np.min(delta_g):.2f}, {np.max(delta_g):.2f}]")
```

### Step 9: Ranking

Compounds are ranked by mean ΔG_bind. The standard error of the mean (SEM) provides a confidence interval. Compounds with overlapping confidence intervals are considered statistically indistinguishable.

## Protocol Variants

### Fast Protocol (screening, lower accuracy)
- MD: 5 ns production (NVT)
- Nwat: 20
- Frames: Every 20 ps → 250 frames
- Throughput: ~2 hours per compound on RTX 4090

### Standard Protocol (lead optimization)
- MD: 20 ns production (NVT)
- Nwat: 30
- Frames: Every 10 ps → 2,000 frames
- Throughput: ~8 hours per compound on RTX 4090

### PPI Protocol (protein-protein interfaces)
- MD: 20 ns production (NVT)
- Nwat: 80
- Frames: Every 10 ps → 2,000 frames
- Throughput: ~8 hours per compound on RTX 4090

### Conservative Protocol (maximum accuracy)
- MD: 50 ns production (NVT)
- Nwat: 30–40 (enzyme) or 80–100 (PPI)
- Frames: Every 5 ps → 10,000 frames
- Entropy: Normal mode analysis on 50 frames
- Throughput: ~24 hours per compound on RTX 4090

## Quality Control

### Convergence Check

Plot ΔG_bind as a function of simulation time. The cumulative average should plateau:

```python
def check_convergence(delta_g):
    cumulative = np.cumsum(delta_g) / np.arange(1, len(delta_g) + 1)
    first_half = np.mean(cumulative[:len(cumulative)//2])
    second_half = np.mean(cumulative[len(cumulative)//2:])
    drift = abs(second_half - first_half)
    return drift < 0.5  # kcal/mol — less than 0.5 kcal/mol drift
```

### Water Occupancy Analysis

Waters selected in >80% of frames are "conserved" and should be examined for structural role. Waters selected in <20% of frames are "transient" and contribute mostly noise.

### Correlation with Known Binders

If known binders with experimental affinities are available, compute the Pearson correlation between Nwat-MMGBSA ΔG_bind and experimental ΔG_exp. If R < 0.6, the protocol may need adjustment (longer MD, different N, or explicit entropy calculation).

## Common Pitfalls

1. **Ligand drifts out of binding site**: Too much kinetic energy or insufficient equilibration. Fix: increase equilibration time or use a ligand restraint during heating.
2. **Water count too low**: Key interactions missed. Fix: increase N by 10 and re-run.
3. **Water count too high**: Bulk water noise. Fix: decrease N until r² peaks.
4. **Slow water exchange**: Some waters take >10 ns to exchange. Fix: extend production MD.
5. **Protein conformational change**: Induced fit invalidates single-trajectory assumption. Fix: run separate trajectories for different protein conformations.
