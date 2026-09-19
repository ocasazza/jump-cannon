# AM1-BCC Charges: Semi-Empirical QM for Ligand Parameterization

## The Charge Parameterization Problem

Every force-field based binding free energy calculation requires partial atomic charges for the ligand. The quality of these charges directly affects the quality of the electrostatic component of ΔG_bind, which can be the dominant term for polar ligands.

| Method | Computational Cost | Accuracy | Suitable For |
|---|---|---|---|
| Gasteiger (empirical) | < 0.01 s | Low | Quick and dirty, not for binding |
| AM1-BCC (semi-empirical QM) | 1–5 s per ligand | Good (HF/6-31G* quality) | Medium-throughput rescoring |
| RESP at HF/6-31G* | 30–120 min per ligand | Gold standard | FEP+, ABFEP |
| RESP at B3LYP/cc-pVTZ | 6–24 hr per ligand | Best possible | Publication validation |

AM1-BCC occupies the "middle path" — fast enough for hundreds of compounds, accurate enough to improve over implicit-solvent docking.

## How AM1-BCC Works

### Step 1: AM1 Semi-Empirical QM

AM1 (Austin Model 1, Dewar et al., 1985) is a semi-empirical quantum mechanical method that solves an approximate Schrödinger equation using parameterized integrals. Compared to full Hartree-Fock:
- ~10,000× faster (seconds vs. hours)
- Uses pre-computed integrals (parameterized to experimental data)
- Handles molecules up to ~500 atoms comfortably

```python
# AM1 computation (pseudocode)
def am1_charges(molecule):
    # 1. Build the Fock matrix using parameterized integrals
    F = build_fock_matrix_am1(molecule)
    # 2. Solve the Roothaan-Hall equations
    C = scf_solve(F)  # Self-consistent field
    # 3. Mulliken population analysis
    charges = mulliken_population(C, molecule.basis_set)
    return charges
```

### Step 2: Bond Charge Correction (BCC)

The BCC correction maps AM1 Mulliken charges to HF/6-31G* quality. It adds a small correction charge to each atom based on the bond topology:

```
q_i^BCC = q_i^AM1 + Σ_j δ_ij
```

Where δ_ij is a pre-computed bond-type correction for bond type (i,j). The corrections are derived from a training set of ~2,700 molecules with known RESP charges.

The key insight: AM1 captures the electronic structure qualitatively (charge separation, polarization), and the BCC correction fixes the systematic AM1 errors quantitatively. The combination is nearly as accurate as RESP for organic drug-like molecules.

## AM1-BCC for Nwat-MMGBSA

In the Nwat-MMGBSA protocol, AM1-BCC charges are computed in Step 2 (Ligand Parameterization):

```bash
antechamber -i ligand.mol2 -fi mol2 -o ligand_gaff.mol2 -fo mol2 \
  -c bcc -s 2 -nc <net_charge>
```

Flags:
- `-c bcc`: Use AM1-BCC charge method
- `-s 2`: Run in "slow" mode (more SCF iterations for convergence)
- `-nc <net_charge>`: Specify the net molecular charge

### Validation

AM1-BCC charges have been validated against RESP at HF/6-31G* for drug-like molecules:

| Property | Correlation (AM1-BCC vs. RESP) | Notes |
|---|---|---|
| Dipole moment | R² = 0.92 | Good agreement, slight underprediction |
| Atomic charges | R² = 0.88 | Systematically ~10% lower magnitude |
| Conformational dependence | R² = 0.85 | BCC correction is conformation-independent |
| Heterocycles | R² = 0.78 | Worst performance — AM1 weakness for N,S rings |

The worst case is nitrogen-containing heterocycles (pyridine, pyrimidine, etc.), where AM1's neglect of differential diatomic overlap leads to incorrect charge separation. For these compounds, the error in AM1-BCC charges can propagate to a 1–2 kcal/mol error in MM-GBSA ΔG_bind.

### When to Use RESP Instead

**Use RESP when:**
- The ligand is a charged heterocycle (pyridinium, imidazolium)
- The ligand contains unusual elements (B, Si, Se) not well-parameterized in AM1
- The FEP+ calculation requires best-possible charges
- The ligand is small (<50 atoms — RESP is fast enough)

**Use AM1-BCC when:**
- Screening hundreds of compounds
- Most compounds are neutral, standard drug-like molecules
- The electrostatic contribution is not the dominant term
- Speed matters more than 0.5 kcal/mol accuracy

## Jump-Cannon Parallel: MassSource as Charge Assignment

The `MassSource` enum in jump-cannon's `geometric` engine is the exact analog of ligand charge parameterization:

```rust
// geometric.rs: how to assign node mass (analogous to atom charge)
pub enum MassSource {
    Degree,      // Mass = degree — fast, empirical (like Gasteiger charges)
    PageRank,    // Mass = PageRank score — medium, semi-empirical (like AM1-BCC)
    Betweenness, // Mass = betweenness — slow, accurate (like RESP)
}
```

| MassSource | Computational Cost | Quality | Analogous Charge Method |
|---|---|---|---|
| `Degree` | O(1) per node | Low — can't distinguish hub from peripheral | Gasteiger |
| `PageRank` | O(k·m) per graph | Good — global context without O(n·m) cost | AM1-BCC |
| `Betweenness` | O(n·m) per graph | Best — captures bridge nodes and bottlenecks | RESP at HF/6-31G* |

The PageRank → AM1-BCC parallel is exact: both use an **iterative propagation** (PageRank: importance; AM1-BCC: electron density) followed by a **local correction** (PageRank: random jump factor; BCC: bond charge correction) to produce a globally-consistent scalar assignment at moderate cost.

### Propagated vs. Local: The Structure of Both Methods

**PageRank**:
1. Initialize: PR_i = 1/n for all i
2. Propagate: PR_i^(t+1) = (1-d)/n + d · Σ_(j→i) PR_j^(t) / outdegree(j)
3. Local correction: random jump factor (1-d) ensures ergodicity
4. Converge to fixed point

**AM1-BCC**:
1. Initialize: AM1 SCF → Mulliken charges q_i^AM1
2. Propagate: The SCF iterations propagate electron density across the molecule
3. Local correction: BCC corrections fix systematic AM1 errors
4. Final charges: q_i^BCC = q_i^AM1 + Σ_j δ_ij

Both are **fixed-point iterations** with a local correction step. The structural identity is deeper than "both use propagation" — both recognize that a purely local method (degree / Mulliken) is fast but inaccurate, and a small amount of global propagation (PageRank iterations / SCF iterations) dramatically improves quality without the full expense of a completely global method.
