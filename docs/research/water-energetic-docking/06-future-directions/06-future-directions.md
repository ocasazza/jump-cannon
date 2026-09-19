# Future Directions in Water-Energetic Docking (2026–2030)

## Executive Summary

Water-energetic molecular docking sits at the intersection of three accelerating trends: (1) GPU-accelerated MD reaching throughputs that enable ensemble-based scoring for thousands of compounds, (2) machine-learned scoring functions that can capture water-mediated interactions from structural data alone, and (3) the convergence of docking and free energy perturbation (FEP) into unified GPU pipelines. This document projects the 2026–2030 trajectory.

## 1. ML-Augmented Water Scoring

### Water-GNNs and Equivariant Networks

The most promising direction is replacing WaterMap's thermodynamic analysis with a learned function that predicts water stability and displaceability directly from protein structure:

```
Input: Binding site atomic coordinates + residue types
Model: SE(3)-equivariant GNN (e.g., EquiformerV2, MACE)
Output: Per-water ΔG_hyd prediction (kcal/mol)
Training: ~50,000 WaterMap simulations from PDBbind + ChEMBL
```

Equivariant networks are particularly well-suited because water thermodynamics are invariant to rotation and translation of the protein-ligand complex. An SE(3)-equivariant model can learn the relationship between local protein geometry and water stability without needing data augmentation.

**Estimated performance**:
- Prediction speed: ~1 ms per binding site (vs. 1–4 hours for WaterMap)
- Accuracy: R² ≈ 0.85 vs. WaterMap ΔG_hyd
- Coverage: Any protein structure, no simulation required

### Diffusion Models for Water Placement

Diffusion models (the architecture behind protein structure prediction models like RFdiffusion) can be trained to generate water molecule positions conditioned on protein structure:

```
Forward process: Add noise to water positions from WaterMap simulations
Reverse (generative) process: Denoise random positions → realistic water placements
Conditioning: Protein structure via equivariant encoder
```

This would enable **sampling diverse water configurations** for a given binding site in milliseconds, replacing the GCMC simulation entirely. For Nwat-MMGBSA, the generated water configurations could provide the explicit water shell without the MD simulation.

## 2. The GPU Funnel Convergence

### Single-GPU Docking + MD + FEP

The next-generation workflow will collapse the entire deployment funnel onto a single GPU:

```
Current:  Glide SP (CPU) → Glide XP (CPU) → WaterMap (GPU) → Glide WS (CPU)
          → Nwat-MMGBSA (GPU MD + CPU MM-GBSA) → FEP+ (GPU)
          
Future:   All stages on GPU, no host synchronization between stages
          Input: Protein structure + compound library
          Output: Ranked ΔG_bind predictions with confidence intervals
          Time: < 1 minute per compound end-to-end
```

This is the analog of jump-cannon's `multilevel.wgsl` — the entire cascade runs on-GPU without host readback. The key enablers:

1. **GPU-native docking search**: Porting Glide's hierarchical search (site-point generation, rough scoring, refinement, minimization) to GPU. The algorithmic structure — a large number of independent ligand poses evaluated against the same receptor grid — is embarrassingly parallel.

2. **On-GPU MD**: Already achieved with Desmond/pmemd.cuda. The remaining bottleneck is GPU→CPU trajectory I/O for analysis.

3. **GPU-native MM-GBSA**: The Generalized Born and surface area calculations are being ported to CUDA. The AmberTools team has demonstrated a prototype 10× speedup.

4. **On-GPU FEP**: Schrödinger's FEP+ already runs on GPU. The integration challenge is removing the file-based handoff between stages.

### Throughput Projection

| Stage | Current (2025) | Near-Term (2027) | Long-Term (2030) |
|---|---|---|---|
| Glide SP per ligand | 1.5 s (CPU) | 0.1 s (GPU) | 0.01 s (GPU + batching) |
| WaterMap pre-analysis | 1–4 hours | 10 ms (ML) | 1 ms (ML) |
| Nwat-MMGBSA MD | 1 hour | 5 min (better GPUs) | 1 min (GPU-native ensemble) |
| FEP+ per pair | 12–24 hours | 2–4 hours | 30 min (on-GPU without I/O) |
| End-to-end funnel | Days (with I/O gaps) | Hours | Minutes |

## 3. Structure- and Ligand-Based Water Networks

### AlphaFold3-Informed Water Placement

AlphaFold3 predicts protein-ligand complexes with explicit water molecule positions (Abramson et al., 2024). While the predicted water positions are approximate, they provide a **prior** for water placement in docking:

```
Workflow:
1. Run AlphaFold3 to predict protein-ligand complex with waters
2. Use predicted water positions as initial placement for Glide WS
3. During docking: refine water positions, delete/replace as needed
4. Result: WaterMap-quality water placement without the 1-4 hour GCMC simulation
```

### Water Network Databases

The PDB now contains >200,000 structures, many with crystallographic waters. A database of **observed water-mediated interactions** could inform docking:

- For a new target, find similar binding sites in the database (by structural alignment)
- Transfer observed water positions and conservation patterns
- Use as priors for water placement during docking

This is analogous to **transfer learning** in ML: water network patterns learned from the entire PDB inform predictions for a novel target.

## 4. Quantum Mechanical Water Treatment

### QM/MM with Explicit Waters

Current Nwat-MMGBSA uses molecular mechanics (MM) for all energy terms, including water-ligand interactions. Quantum mechanical treatment of the water molecules in the binding site could improve accuracy:

```
QM region: Ligand + N closest waters + catalytic residues
MM region: Rest of protein + remaining solvent
Method: DFT (ωB97X-D/6-31G*) for QM, ff14SB for MM
Cost: ~10× more than pure MM (still feasible for lead optimization)
```

QM treatment of waters captures:
- Charge transfer between water and ligand (MM can't)
- Polarization effects (MM can approximate but not capture fully)
- Proton transfer in catalytic mechanisms

### Semi-Empirical QM Waters

A compromise between MM and full DFT: treat waters with semi-empirical QM (GFN2-xTB, PM7) instead of TIP3P:

```
Cost: ~2–3× slower than TIP3P (much faster than DFT)
Benefits: Polarization, charge transfer, better H-bond geometries
Limitations: Still approximate, but better than fixed-charge water models
```

## 5. Real-Time Docking for Medicinal Chemistry

### Interactive Pose Refinement

The convergence of GPU docking and GPU MD opens the possibility of **interactive** water-aware docking:

```
User drags a ligand fragment in the binding site
GPU evaluates: docking score, water displacement energy, per-water ΔG
Real-time feedback: "this methyl group displaces a +2.1 kcal/mol water"
Latency: < 50 ms (single frame at 20+ fps)
```

This transforms docking from a batch process ("dock 500 compounds, look at top 50") to an interactive design tool ("try this modification, see the water impact immediately").

### Cloud-Native Deployment

The GPU funnel will be deployed as a cloud service:

- Medicinal chemist uploads protein structure + compound ideas
- Cloud GPU cluster runs the full funnel (Glide WS → Nwat-MMGBSA → FEP+)
- Results returned with confidence intervals and per-water analysis
- Cost: < $5 per compound (cloud GPU spot pricing)

## 6. Beyond Small Molecules: Water in Biologics

### Antibody-Antigen Interfaces

Antibody-antigen interfaces are large (1500–3000 Å²), flat, and highly solvated. Existing docking tools struggle with these interfaces. Water-aware docking with explicit solvent could transform antibody design:

```
Challenge: 60–200+ water molecules in the interface
Solution: Nwat-MMGBSA with N=100–200, ensemble-average over MD
Goal: Predict which antibody mutations will improve affinity by
       displacing high-energy interface waters
```

### PROTAC Ternary Complexes

PROTACs (PROteolysis TArgeting Chimeras) form ternary complexes (E3 ligase + PROTAC + target protein) with novel protein-protein interfaces. The water network at these induced interfaces is poorly understood and critical for rational PROTAC design.

## 7. Integration with Jump-Cannon Architecture

The future directions for docking map directly onto jump-cannon's research roadmap:

| Docking Future | Jump-Cannon Future | Status |
|---|---|---|
| ML water scoring | ML edge-strength prediction | Research phase |
| GPU-native funnel | `multilevel.wgsl` cascade | Implemented |
| On-GPU MD → layout conversion | `geometric_bonding_gpu.rs` | Implemented |
| Interactive pose refinement | Real-time layout editing | Dioxus panel planned |
| Cloud deployment | gRPC compute service (`graph-compute`) | Implemented |
| Transferable water patterns | Persistent layout quality models | Not yet explored |

### The "WaterMap for Graph Layout" Research Direction

The most exciting crossover: develop a **layout-watermap** that identifies "high-energy" (unstable) regions of a graph layout and guides the user toward more stable configurations. This is the layout analog of WaterMap's thermodynamic analysis:

1. Run 100 short layout perturbations from the current configuration
2. Compute per-node variance (positional, stress, edge length)
3. Color nodes by "stability": low-variance = stable/blue, high-variance = unstable/red
4. Guide the user: "this cluster is barely held together — add an edge or adjust spring constants"

This module does not yet exist in jump-cannon but would be a natural extension of the existing `GpuForce` engine's energy tracking capabilities.

## References (Projected)

- Abramson et al. (2024). "Accurate structure prediction of biomolecular interactions with AlphaFold 3." *Nature* **630**: 493–500.
- Corso et al. (2023). "DiffDock: Diffusion Steps, Twists, and Turns for Molecular Docking." *ICLR 2023*.
- Liao & Smidt (2023). "Equiformer: Equivariant Graph Attention Transformer for 3D Atomistic Graphs." *ICLR 2023*.
- Watson et al. (2023). "De novo design of protein structure and function with RFdiffusion." *Nature* **620**: 1089–1100.
- Bannwarth et al. (2019). "GFN2-xTB—An Accurate and Broadly Parametrized Self-Consistent Tight-Binding Quantum Chemical Method." *J. Chem. Theory Comput.* **15**(3): 1652–1671.
