# Hybrid Physics–Machine Learning Approaches for Molecular Docking

## Executive Summary

The most productive research direction in molecular docking from 2022–2026 has not been pure deep learning or pure physics—it has been the **hybrid combination of the two**. Physics-based methods provide physically interpretable pose generation and the ability to model explicit solvent, while machine learning provides fast, data-driven rescoring and the ability to learn complex nonlinear relationships that classical force fields miss. The strongest virtual screening results in recent benchmarks come from **physics-based pose generation + ML rescoring**, not from end-to-end DL methods.

## The Taxonomy of Hybrid Approaches

Hybrid methods can be classified by which component is ML and which is physics-based:

### Category 1: Physics Generation + ML Scoring (The Pragmatic Standard)

The traditional docking pipeline is: **pose generation → scoring → ranking.** ML enters at the scoring stage.

| Method | Physics Component | ML Component | Key Result |
|--------|-------------------|--------------|------------|
| **GNINA** | Vina/smina pose search + Vina intermolecular terms | CNN scoring function trained on cross-docked poses | Best single method in LIT-PCBA benchmark; EF1% = 2.14 median |
| **AutoDock-GPU + NMDN** | AutoDock-GPU pose generation | Neural network rescoring | Competitive with GNINA; 2026 benchmark |
| **Glide SP + RF-score-vs** | Glide SP pose generation | Random Forest on protein–ligand interaction features | Strong enrichment on DUD-E targets |
| **Surflex-Dock + NNScore** | Surflex pose sampling | Neural network scoring on structural features | Older pipeline; established the pattern |

**How it works**: A physics-based docking engine (AutoDock, Glide, Vina) generates a set of candidate poses. The top-ranked poses by the built-in scoring function are then rescored by an ML model trained on structural features (distance distributions, interaction fingerprints, electrostatic maps). The ML rescoring corrects systematic errors in the physics-based score without inheriting the speed penalty of explicit-solvent rescoring.

**Why it works**: Physics-based pose generation samples physically reasonable conformations (the search problem), while ML scoring captures nonlinear relationships that the simplified force field misses (the scoring problem). Each does what it's good at.

### Category 2: ML Generation + Physics Scoring (Emerging)

The reverse pattern—ML generates poses, physics validates them.

| Method | ML Component | Physics Component | Status |
|--------|-------------|-------------------|--------|
| **DiffDock + GNINA** | Diffusion pose generation | CNN rescoring | Evaluated in 2026 benchmark; underperformed AutoDock + GNINA |
| **PLANTAIN** | Neural scoring function trained diffusion-style | L-BFGS optimization (physics-inspired) | Fast; 2023 |
| **EquiBind + MM-GBSA** | SE(3)-equivariant pose prediction | MM-GBSA rescoring | Not yet systematically benchmarked |

This category is **not yet competitive** for virtual screening. The 2026 LIT-PCBA benchmark found that DiffDock-generated poses, even when rescored with GNINA or NMDN, produced lower enrichment than AutoDock-GPU-generated poses with the same rescoring. The likely explanation: DiffDock produces diverse poses but struggles with the precision needed for consistent active/inactive discrimination.

### Category 3: Physics–ML Co-Design (Research Frontier)

Methods where the physics and ML are intertwined in a single model architecture.

| Approach | Mechanism | Example |
|----------|-----------|---------|
| **Delta-learning** | ML predicts correction Δ to physics-based score. Corrects MM → QM-level accuracy. | Δ-ML for DFT-quality binding energies applied to docking poses |
| **Physics-informed neural networks (PINNs)** | Physical constraints embedded in network architecture or loss function | Equivariant networks (SE(3) symmetry enforced by design) |
| **Interaction fingerprints + GBDT** | Docking pose → IFP feature vector → gradient-boosted tree regressor | PLEC fingerprints + XGBoost; strong on CASF benchmarks |
| **Active learning** | Bayesian optimization over docking scores + physics priors | BAL (Cao & Shen, 2019): UQ for docking quality |

### Category 4: Acceleration Via ML (Operational)

ML used to speed up physics-based workflows without changing the underlying method.

| Method | ML Role | Speedup |
|--------|---------|---------|
| **MLDDM** | Predict which compounds to skip before full docking | 2–5× reduction in docking compute |
| **Multi-task VS** | Share learned features across target families | Improved enrichment with same docking budget |
| **Active learning loops** | Select most informative compounds for experimental testing | Reduced experimental burden |

## The GNINA Archetype

GNINA (McNutt et al., 2021) is the best-studied hybrid docking method and represents the current state of the art for virtual screening. Understanding its design reveals why hybrid methods outperform pure DL methods.

### Architecture

```
Input: Protein structure (PDB) + ligand SMILES
    ↓
1. smina (Vina fork): Generate poses via Monte Carlo search
   - Scoring: Vina intermolecular terms (steric, hydrophobic, H-bond)
   - Output: N ranked poses with Vina scores
    ↓
2. CNN scoring function: Rescore each pose
   - Input: 3D voxelized protein–ligand complex (48×48×48 Å grid)
   - Architecture: 5 convolutional layers → dense layers
   - Training: Cross-docked poses from PDBbind general set
   - Output: CNN affinity prediction per pose
    ↓
3. Ensemble: Average Vina score + CNN score
   - Final ranking by combined score
```

### Why GNINA Works

1. **Physics handles the search**: Vina's Monte Carlo sampling efficiently explores conformational space with physically reasonable moves. ML methods (DiffDock, EquiBind) must learn this from scratch and are limited by training data coverage.

2. **CNN learns what Vina misses**: Vina's scoring function is a linear combination of physically interpretable terms. The CNN captures nonlinear interactions (π-stacking, halogen bonding, desolvation effects) that don't fit the linear model.

3. **Ensembling reduces variance**: Averaging physics + ML scores is more robust than either alone. Systematic errors in one component are partially corrected by the other.

### Limitations

- **No explicit water**: GNINA's CNN operates on dry complexes. Water-mediated interactions are opaque to it.
- **Static scoring**: The CNN is a snapshot scorer; it doesn't capture conformational dynamics or entropy.
- **Training data dependency**: Cross-docked PDBbind poses may not represent realistic virtual screening scenarios.

## Delta-Learning: The QM Accuracy Bridge

A specific class of hybrid approach with direct relevance to water-energetic docking is **delta-learning**.

### Concept

Classical force fields (MMFF94, GAFF, OPLS) are fast but inaccurate for non-covalent interactions. Quantum mechanical methods (DFT, CCSD(T)) are accurate but too slow for docking. Delta-learning trains an ML model to predict the *difference* between QM and MM energies:

```
Δ = E_QM(complex) − E_MM(complex)
ML model: features → Δ̂
Corrected score: E_MM + Δ̂
```

This gives near-QM accuracy at near-MM cost.

### Application to Docking

For docking scoring functions, the most relevant target is **interaction energy**:

- Train ML model on QM-level interaction energies for protein–ligand fragment pairs
- Apply Δ correction to Vina/Glide scores during rescoring
- Corrects systematic errors in electrostatic and dispersion terms

### Relevance to Water-Energetic Methods

The same delta-learning framework can be applied to **explicit-water scoring**:

- MM-GBSA with Nwat waters gives MM-level water energetics
- Δ-ML trained on QM-level water–ligand interaction energies could correct systematic errors in the MM water model
- This is a natural extension of Nwat-MMGBSA workflows (Section [03](../../03-nwat-mmgb-sa/))

No published study has yet applied delta-learning to water-corrected docking scores. This is an open research opportunity.

## Interaction Fingerprints: The Pragmatic Baseline

An underappreciated finding from 2024–2026 is that **simple feature engineering + GBDT often matches or beats deep learning**.

### IFP Pipeline

```
Docking pose → extract IFP features → train XGBoost/LightGBM → predict affinity
```

An **interaction fingerprint (IFP)** encodes which protein residues interact with which ligand atoms through what types of interactions:

- Hydrophobic contacts (distance < 4.0 Å between nonpolar atoms)
- Hydrogen bonds (donor–acceptor distance < 3.5 Å, angle > 120°)
- π-stacking (ring centroid distance < 5.0 Å)
- Salt bridges (oppositely charged groups within 4.0 Å)
- Water bridges (water-mediated protein–ligand H-bonds)

The PLEC (Protein–Ligand Extended Connectivity) fingerprint encodes these as a fixed-length bit vector. With gradient-boosted trees, PLEC achieves competitive enrichment on CASF benchmarks with training times of seconds, not GPU-days.

### Why It Works (When It Works)

- **IFPs are physically interpretable**: The features directly encode the physics of binding. A tree model learning on these features is essentially learning a nonlinear combination of physically meaningful interactions.
- **Low data requirements**: IFP + GBDT trains on hundreds of complexes, not thousands. This matters for target-specific models where data is limited.
- **No deep learning overhead**: Training and inference run on CPU in seconds, making it practical for iterative screening workflows.

### Limitations

- **Depends on pose quality**: IFPs are extracted from docked poses. If the pose is wrong, the fingerprints are meaningless.
- **No 3D spatial context**: IFPs are pairwise interaction counts; they lose the geometric arrangement of interactions.
- **Transferability is limited**: Models trained on one target family may not generalize to structurally different families.

## Active Learning for Docking

Active learning closes the loop between computation and experiment, using ML to decide which compounds to test next.

### BAL: Bayesian Active Learning for Protein Docking

Cao & Shen (2019) introduced Bayesian active learning for protein–protein docking, but the framework generalizes to small-molecule docking:

1. **Prior**: Bayesian model over docking scores and their uncertainty
2. **Acquisition**: Select poses/compounds that maximize information gain
3. **Oracle**: Evaluate selected items (experimentally or with expensive computation)
4. **Update**: Refine model with new data
5. **Iterate**: Repeat until convergence

For water-energetic workflows, the oracle could be expensive explicit-water rescoring (WScore, WaterMap, MM-GBSA), applied only to the most informative subset of poses selected by the active learner.

## Benchmark Reality Check

The 2024–2026 period has produced essential calibration for hybrid methods:

### Schapin et al. (2024): ML for Binding Affinity

Key findings from a comprehensive benchmark of ML methods for binding affinity prediction:

1. **2D ligand-only models (RDKit fingerprints + GBDT) can match 3D structure-based neural networks** on several benchmark datasets. The additional complexity of 3D CNNs and GNNs does not always translate to better predictions.
2. **Large language model embeddings (ESM, ProtBERT) as protein representations added value** in some scenarios but not consistently.
3. **Active learning improved sample efficiency**—selecting which compounds to train on mattered more than model architecture.
4. **Classification and ranking tasks showed different rankings of methods** than regression; no single method dominated all tasks.

The practical takeaway: start with simple models, add complexity only when validated.

### Abo-Dahab et al. (2026): LIT-PCBA Virtual Screening

The most important benchmark for hybrid methods to date, on a realistic, experimentally-derived dataset:

| Method | Median EF1% | Notes |
|--------|-------------|-------|
| **AutoDock-GPU + GNINA** | **2.14** | Best single method |
| AutoDock-GPU + NMDN | ~1.8 | Competitive with GNINA |
| DiffDock + NMDN | ~1.2 | DL pose gen underperformed physics pose gen |
| DiffDock + GNINA | ~1.0 | Same rescoring, worse poses → worse enrichment |
| Consensus (rank-based) | ~2.0+ | Combining multiple methods improved results |
| Supervised ML on docking features | variable | Depended on target; not consistently better |

Key interpretation: **pose quality is the limiting factor**. Even the best ML rescoring cannot compensate for poor pose generation. Traditional physics-based pose generators (AutoDock-GPU, Glide) currently produce more screening-competent poses than DL generators (DiffDock).

## The Water Gap in Hybrid Methods

No hybrid method as of 2026 incorporates explicit water molecules in its scoring. This is the single largest opportunity for improvement:

1. **GNINA + explicit water**: Adding water-occupied voxels to GNINA's CNN input would be a natural extension. The CNN architecture already handles 3D grids; adding water channels is architecturally trivial. Training data is the bottleneck (few co-crystallized waters in PDBbind).

2. **Nwat-MMGBSA rescoring of DL poses**: The most direct integration of water-energetic methods with DL docking. Generate poses with DiffDock/EquiBind, then apply Nwat-MMGBSA rescoring with the closest-N-waters protocol. This has not been benchmarked.

3. **WaterMap-informed ML scoring**: Use WaterMap thermodynamic predictions (ΔG, ΔH, ΔS per water site) as input features to ML scoring functions. The ML model learns when water displacement energy overrides the ligand–protein interaction score.

4. **Delta-learning for water interactions**: Train Δ-ML models on QM-quality water–protein and water–ligand interaction energies, then apply the correction to MM-GBSA water terms.

## Practical Recommendation

For drug discovery teams deciding between pure physics, pure ML, and hybrid approaches:

| Scenario | Recommended Approach | Rationale |
|----------|---------------------|-----------|
| Large-scale VS (>1M compounds) | Glide SP → IFP + GBDT rescoring | Speed + interpretability |
| Medium-scale VS (10K–100K) | AutoDock-GPU + GNINA | Best enrichment per compute |
| Lead optimization (10–100) | Glide WS → WScore → FEP+ | Highest accuracy, explicit water |
| Novel target (no known ligands) | EquiBind/DiffDock for pose diversity → physics rescoring | Explore binding modes |
| Water-sensitive target | Physics-only: Glide WS + WaterMap | ML methods can't handle water yet |

## References

- McNutt AT, Francoeur PG, Aggarwal R et al. (2021). "GNINA 1.0: molecular docking with deep learning." *J. Cheminform.* **13**:43.
- Schapin N, Navarro C, Bou A, De Fabritiis G (2024). "On Machine Learning Approaches for Protein-Ligand Binding Affinity Prediction." arXiv:2407.19073.
- Abo-Dahab Y, Xiang X, Chun J, Zhao L (2026). "Benchmarking Single-Pose Docking, Consensus Rescoring, and Supervised ML on the LIT-PCBA Library." arXiv:2605.01681.
- Cao Y, Shen Y (2019). "Bayesian active learning for optimization and uncertainty quantification in protein docking." arXiv:1902.00067.
- Morrone JA, Weber JK, Huynh T, Luo H, Cornell WD (2019). "Combining docking pose rank and structure with deep learning improves protein-ligand binding mode prediction." arXiv:1910.02845.
- Liu Z, Ye X, Fang X, Wang F, Wu H, Wang H (2021). "Docking-based Virtual Screening with Multi-Task Learning." arXiv:2111.09502.
- Ma W, Qin X, Zhang J et al. (2021). "Deep Learning Model of Dock by Dock Process Significantly Accelerate the Process of Docking-based Virtual Screening." arXiv:2110.10918.
- Corso G, Stärk H, Jing B, Barzilay R, Jaakkola T (2022). "DiffDock: Diffusion Steps, Twists, and Turns for Molecular Docking." arXiv:2210.01776.
- Morehead A, Giri N, Liu J, Neupane P, Cheng J (2024). "Assessing the potential of deep learning for protein-ligand docking." arXiv:2405.14108.
- Gomes J, Ramsundar B, Feinberg EN, Pande VS (2017). "Atomic Convolutional Networks for Predicting Protein-Ligand Binding Affinity." arXiv:1703.10603.
