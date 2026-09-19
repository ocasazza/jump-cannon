# ML Scoring Functions for Molecular Docking

## Executive Summary

Machine learning scoring functions represent a fundamental rethinking of how to evaluate protein–ligand binding. Rather than parameterizing physical equations (force fields, electrostatics, solvation models), ML methods learn the relationship between structural features and binding affinity directly from data. This section surveys the evolution from early feature-based models (RF-Score, NNScore) through graph neural networks (DeepAtom) to the latest topological deep learning approaches (KEPLA, PSLL).

*Full analysis of this sub-topic is in development. Key findings from the research phase are summarized below.*

## Key Papers and Methods

### Classical ML Scoring (2010–2017)

- **RF-Score** (Ballester & Mitchell, 2010): Random Forest on protein–ligand interaction features. Established that ML could match classical scoring with simple features.
- **NNScore** (Durrant & McCammon, 2010): Neural network scoring using structural features. Early demonstration of neural approaches.
- **Atomic CNN** (Gomes et al., 2017): 3D spatial convolution on atomic coordinates. Showed DL could surpass AutoDock Vina scoring.

### Deep Learning Scoring (2018–2022)

- **DeepAtom** (Li et al., 2019): 3D CNN on voxelized protein–ligand complexes. Unified representation of binding site and ligand in 3D grid.
- **Pafnucy** (Stepniewska-Dziubinska et al., 2018): 3D CNN with rotational augmentation for scoring.
- **OnionNet** (Zheng et al., 2019): Shell-based CNN using distance-binned interaction features. Rotationally invariant.

### Modern ML Scoring (2023–2026)

- **KEPLA** (Liu et al., 2025): Knowledge-enhanced DL integrating Gene Ontology annotations and ligand properties. Multimodal sequence+graph input.
- **PSLL** (Zia et al., 2026): Persistent Sheaf Laplacian Learning—topological deep learning incorporating atomic charges. Latest advance in topology-aware scoring.
- **Binding Affinity Survey** (Schapin et al., 2024): Comprehensive benchmark showing simpler models (GBDT, CNN) can match or exceed complex GNNs.

## Key Finding: Simpler Models Often Win

The most important finding from 2024–2026 is that **model complexity does not correlate with accuracy** for binding affinity prediction. Tree-based models on ligand-only 2D features (RDKit fingerprints + XGBoost) achieve competitive performance with 3D CNNs and GNNs trained on bound complexes. This suggests that (a) binding affinity is largely encoded in ligand structure, and (b) current deep learning architectures have not found a way to leverage 3D structural information that substantially improves over simple baselines.

## Relevance to Water-Energetic Docking

ML scoring functions in their current form are **complementary to, not competitive with**, water-energetic scoring. The NNScore, RF-Score, and DeepAtom families all operate on dry complexes. A hybrid approach that adds water-derived features (WaterMap energies, explicit water positions) to ML scoring inputs is an open research direction.

## References

- Ballester PJ, Mitchell JBO (2010). "A machine learning approach to predicting protein–ligand binding affinity with applications to molecular docking." *Bioinformatics* **26**:1169–1175.
- Durrant JD, McCammon JA (2010). "NNScore: A Neural-Network-Based Scoring Function for the Characterization of Protein–Ligand Complexes." *J. Chem. Inf. Model.* **50**:1865–1871.
- Gomes J, Ramsundar B, Feinberg EN, Pande VS (2017). "Atomic Convolutional Networks for Predicting Protein-Ligand Binding Affinity." arXiv:1703.10603.
- Li Y, Rezaei MA, Li C, Li X (2019). "DeepAtom: A Framework for Protein-Ligand Binding Affinity Prediction." arXiv:1912.00318.
- Stepniewska-Dziubinska MM, Zielenkiewicz P, Siedlecki P (2018). "Development and evaluation of a deep learning model for protein–ligand binding affinity prediction." *Bioinformatics* **34**:3666–3674.
- Zheng L, Fan J, Mu Y (2019). "OnionNet: a multiple-layer inter-molecular convolutional neural network for protein-ligand binding affinity prediction." *J. Chem. Inf. Model.* **59**:4318–4329.
- Schapin N et al. (2024). "On Machine Learning Approaches for Protein-Ligand Binding Affinity Prediction." arXiv:2407.19073.
- Liu H et al. (2025). "KEPLA: A Knowledge-Enhanced Deep Learning Framework for Accurate Protein-Ligand Binding Affinity Prediction." arXiv:2506.13196.
- Zia M, Jones B, Wei G-W (2026). "PSLL: Persistent Sheaf Laplacian Learning for Protein-Ligand Binding Affinity Prediction." arXiv:2609.05475.
