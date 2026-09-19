# Foundation Models and Their Impact on Molecular Docking

## Executive Summary

The release of AlphaFold 3 (Abramson et al., 2024) and RoseTTAFold All-Atom (Krishna et al., 2024) marks the arrival of foundation models capable of predicting protein–ligand complex structures directly from sequence. These models do not "dock" in the traditional sense—they predict the bound complex in a single forward pass using a diffusion architecture trained on the PDB. Their impact on molecular docking workflows is profound but indirect: they provide high-quality protein structures and enable new "differentiable docking" experiments, but they do not replace traditional docking for virtual screening.

*Full analysis of this sub-topic is in development. Key findings are summarized below.*

## AlphaFold 3 (2024)

### What AF3 Does

AlphaFold 3 is a diffusion-based model that predicts the 3D structure of biomolecular complexes given input sequences and optional ligand specifications. Unlike AF2, which only predicted protein structures, AF3 handles:

- Proteins
- Nucleic acids (DNA, RNA)
- Small molecule ligands (drug-like compounds)
- Ions and cofactors
- Post-translational modifications

The architecture uses a diffusion module operating on atomic coordinates, with the diffusion process conditioned on pairwise representations from a modified Pairformer (a simpler replacement for AF2's Evoformer).

### AF3 as a Docking Tool

AF3 can be used for docking by providing a protein sequence and ligand SMILES as input. The model predicts the bound complex structure. Key characteristics:

| Property | AF3 | Traditional Docking |
|----------|-----|---------------------|
| Speed | Minutes per complex | Seconds per complex |
| Poses per run | 5–25 (sampled) | 100–10,000 |
| Explicit water | No | Varies (some methods: yes) |
| Protein flexibility | Full (predicted) | Typically rigid (some flexible) |
| Training data | All PDB (experimental) + predicted | Physics-based (no training) |

### Limitations for Docking

1. **Computational cost**: AF3 inference requires significant GPU resources and is ~100–1000× slower per complex than traditional docking.
2. **No explicit water**: Like all current ML methods, AF3 does not model explicit solvent.
3. **Stoichiometry requires prior knowledge**: The user must specify which ligands, cofactors, and metals are present. AF3 does not screen for unknown binders.
4. **Limited to training distribution**: Performance degrades on ligand chemotypes and protein families underrepresented in PDB.
5. **Deterministic (or low-diversity)**: With a fixed random seed, AF3 produces limited pose diversity. Sampling multiple seeds is expensive.

### State-Aware Improvements

Zhu et al. (2025) showed that AF3's protein–ligand prediction accuracy improves when using "purified" MSA inputs—removing sequences that may contain structural information about the bound state. This "state-aware" protocol reduces contamination from holo-structure information in MSAs.

## RoseTTAFold All-Atom (2024)

RoseTTAFold-AA extends the RoseTTAFold architecture to handle non-protein components including ligands, nucleic acids, and covalent modifications. Unlike AF3 (which is proprietary at the structure-prediction core), RF-AA is published with code and weights available for academic use.

Key differences from AF3:
- **Architecture**: Three-track (1D sequence, 2D distance, 3D coordinates) rather than Pairformer + diffusion
- **Accessibility**: Open weights and code for academic use
- **Ligand coverage**: Broader chemical space due to different training strategy

## Impact on Docking Workflows

### Near-Term (2024–2026)

1. **Structure source, not docking replacement**: AF3 and RF-AA are used to generate high-quality protein structures for traditional docking campaigns, especially for targets with no experimental structure.
2. **Pose validation**: AF3 predictions serve as independent validation of docking poses. Agreement between AF3 and traditional docking increases confidence.
3. **Binding site identification**: AF3's predicted binding sites guide focused docking, reducing search space.

### Medium-Term (2026–2028)

1. **Differentiable docking**: AF3's differentiable architecture enables gradient-based optimization of ligand structure for binding. Abbaszadeh & Shahlaee (2025) describe AF3 as "a foundational framework for integrating deep learning with physics-based molecular simulations."
2. **Ensemble docking**: Combining AF3 predictions with traditional docking and physics-based rescoring for consensus scoring.
3. **Active learning loops**: AF3 generates initial poses → traditional docking refines → physics rescoring selects → AF3 validates top hits.

### Long-Term (2028+)

Open question: will foundation models eventually make traditional docking obsolete? Current assessment: **no, but they will reshape it**. Traditional docking's strengths (speed, explicit solvent modeling, physics-based interpretability) remain relevant. AF3's strengths (whole-complex prediction, protein flexibility, generalization) address different needs.

## The Water Gap (Again)

Neither AF3 nor RF-AA model explicit water molecules. For water-sensitive targets (aspartic proteases, kinases with conserved water networks), foundation model predictions must be complemented by explicit-water scoring. This is the same gap identified for all ML methods in this section.

## References

- Abramson J et al. (2024). "Accurate structure prediction of biomolecular interactions with AlphaFold 3." *Nature* **630**:493–500.
- Krishna R et al. (2024). "Generalized biomolecular modeling and design with RoseTTAFold All-Atom." *Science* **384**:eadl2528.
- Abbaszadeh A, Shahlaee A (2025). "From Prediction to Simulation: AlphaFold 3 as a Differentiable Framework for Structural Biology." arXiv:2508.18446.
- Zhu X et al. (2025). "State-aware protein-ligand complex prediction using AlphaFold3 with purified sequences." arXiv:2506.00147.
