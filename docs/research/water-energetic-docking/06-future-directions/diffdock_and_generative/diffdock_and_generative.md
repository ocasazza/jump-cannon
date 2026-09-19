# DiffDock and Generative Models for Molecular Docking

## Executive Summary

DiffDock (Corso et al., 2022) represents the most significant methodological innovation in molecular docking since the advent of empirical scoring. By reframing docking as a generative modeling problem on the SE(3) manifold of ligand poses, it achieved a 38% top-1 success rate (RMSD < 2Å) on PDBBind, compared to 23% for the prior state of the art. This triggered a wave of follow-up work extending the diffusion framework to flexible docking, protein–protein docking, and hybrid scoring.

*Full analysis of this sub-topic is in development. Key findings are summarized below.*

## The DiffDock Framework

### Core Innovation

Traditional docking: **search** over pose space with a physics-based scoring function.
DiffDock: **generate** poses from a learned distribution by reversing a diffusion process.

The ligand pose space is decomposed into three sub-manifolds:
1. **Translation** (ℝ³): Center of mass position relative to pocket
2. **Rotation** (SO(3)): Orientation of rigid ligand body
3. **Torsion** (SO(2)ᵐ for m rotatable bonds): Internal ligand conformation

A separate diffusion process is defined on each sub-manifold, trained to denoise random poses into correct binding poses. At inference, random noise is progressively denoised to produce a predicted binding pose.

### Key Results

| Metric | DiffDock | Prior Best | Improvement |
|--------|----------|------------|-------------|
| Top-1 RMSD < 2Å (PDBBind) | 38% | 23% (EquiBind) | +65% |
| Top-5 RMSD < 2Å | 54% | 40% | +35% |
| Inference time | ~10s per complex | ~1s (traditional) | Slower |

### Limitations

1. **Rigid protein assumption**: Original DiffDock treats the protein as rigid. Real binding involves pocket sidechain rearrangement.
2. **No explicit water**: Like all current ML docking methods, operates on dry complexes.
3. **Single-ligand**: Trained for one ligand at a time; can't handle cofactors, metals, or multi-ligand systems.
4. **Training data dependency**: Performance degrades on protein families underrepresented in PDBBind.
5. **Computational cost**: ~10 seconds per complex vs. ~1 second for traditional docking. This matters at virtual screening scale.

## The Diffusion Wave (2023–2026)

### DiffDock Variants and Extensions

- **DiffDock-PP** (Ketata et al., 2023): Extends to rigid protein–protein docking. Same SE(3) diffusion framework on larger interface.
- **Re-Dock** (Huang et al., 2024): Introduces *flexible docking*—simultaneously predicts ligand and pocket sidechain poses using a diffusion bridge. Addresses the rigid-protein limitation.
- **Harmonic Torsional Diffusion** (2026): Specialized diffusion on torsional space for flexible ligand docking. Latest refinement of the torsional component.
- **DiffBindFR** (Zhu et al., 2023): SE(3) equivariant network for flexible docking. Alternative architecture to diffusion for the same problem.
- **Hierarchical Adaptive Diffusion** (Yin & Shen, 2025): Separates global rigid-body motion from local flexibility with distinct noise schedules. Mimics induced-fit.

### Alternative Generative Approaches

- **PLANTAIN** (Brocidiacono et al., 2023): Diffusion-*inspired* but not a generative model. Trains a neural scoring function using diffusion-style denoising, then uses L-BFGS optimization for fast pose prediction. Compromise between generative flexibility and optimization speed.
- **Docking Game** (Zhang et al., 2025): Game-theoretic self-play between ligand and protein docking modules. Novel framework beyond pure diffusion.

## Benchmark Reality Check (2026)

The LIT-PCBA benchmark (Abo-Dahab et al., 2026) provides the most rigorous evaluation of DiffDock for virtual screening:

> DiffDock + NMDN rescoring underperformed AutoDock-GPU + GNINA by a significant margin (median EF1% ~1.2 vs 2.14). Raw DiffDock poses, even with rescoring, lack the precision needed for accurate virtual screening enrichment.

This suggests that while DiffDock's generative approach produces physically plausible poses, the *ranking* quality (distinguishing actives from inactives) is not yet competitive with traditional docking + ML rescoring.

## Relevance to Water-Energetic Docking

DiffDock's most natural integration with water-energetic methods is as a **pose diversity generator**:

- Use DiffDock to generate diverse binding poses (including cryptic/alternative binding modes)
- Apply explicit-water rescoring (WScore, Nwat-MMGBSA, WaterMap) to the generated poses
- Select poses where water thermodynamics favor binding

This pipeline has not been benchmarked and represents an open research opportunity.

## References

- Corso G, Stärk H, Jing B, Barzilay R, Jaakkola T (2022). "DiffDock: Diffusion Steps, Twists, and Turns for Molecular Docking." arXiv:2210.01776.
- Ketata MA et al. (2023). "DiffDock-PP: Rigid Protein-Protein Docking with Diffusion Models." arXiv:2304.03889.
- Huang Y et al. (2024). "Re-Dock: Towards Flexible and Realistic Molecular Docking with Diffusion Bridge." arXiv:2402.11459.
- Zhu J, Gu Z, Pei J, Lai L (2023). "DiffBindFR: An SE(3) Equivariant Network for Flexible Protein-Ligand Docking." arXiv:2311.15201.
- Brocidiacono M et al. (2023). "PLANTAIN: Diffusion-inspired Pose Score Minimization for Fast and Accurate Molecular Docking." arXiv:2307.12090.
- Yin R, Shen Y (2025). "A Hierarchical Adaptive Diffusion Model for Flexible Protein-Protein Docking." arXiv:2509.20542.
- Zhang Y et al. (2025). "The Docking Game: Loop Self-Play for Fast, Dynamic, and Accurate Prediction of Flexible Protein-Ligand Binding." arXiv:2508.05006.
- Abo-Dahab Y et al. (2026). "Benchmarking Single-Pose Docking, Consensus Rescoring, and Supervised ML on the LIT-PCBA Library." arXiv:2605.01681.
