# Glide WS Calibration and FEP+ Integration

## The Calibration Problem

No empirical scoring function is perfect. Systematic biases arise from:
1. Parameterization choices (force field, solvation model)
2. Training set composition (which PDBbind subset was used)
3. Simplifying assumptions (additivity, rigid receptor, continuum solvent)

Glide WS's calibration layer corrects these biases using high-quality FEP+ data as a reference.

## Calibration Data Sources

### Tier 1: FEP+ Benchmark (500+ congeneric series)

FEP+ (Free Energy Perturbation Plus) is Schrödinger's implementation of alchemical free energy calculations. It provides the most reliable computational ΔΔG values available:

```
Accuracy: MUE ≈ 0.84 kcal/mol vs. experiment
Coverage: 500+ congeneric series spanning kinases, proteases, GPCRs, etc.
Each series: 5–20 compounds, all perturbations from a common scaffold
Training: Perturbation pairs where FEP+ and experiment agree within 1.0 kcal/mol
```

FEP+ data is the "gold standard" calibration reference because:
- FEP+ captures explicit water energetics (explicit solvent MD)
- FEP+ includes protein flexibility (unlike rigid docking)
- FEP+ has been validated against thousands of experimental measurements

### Tier 2: PDBbind Refined Set (~5,000 complexes)

High-quality crystal structures with reliable Kd/Ki data:

```
Selection criteria:
  - Resolution ≤ 2.5 Å
  - R-factor ≤ 0.25
  - Kd or Ki data from peer-reviewed literature
  - No covalent ligands
  - No multi-component crystals (only 1:1 complexes)
```

### Tier 3: Internal Schrödinger Data

Proprietary datasets from pharmaceutical collaborations, including:
- Fragment screening data (weak binders, mM range)
- Lead optimization series (tight SAR with consistent assay conditions)
- Negative/target engagement data (compounds confirmed inactive)

## Calibration Methodology

Glide WS's calibration layer applies a **multiplicative correction** to the docking score. The correction is a function of:

```
Score_WS_calibrated = Score_WS_raw + ΔScore_calibration

ΔScore_calibration = f(
    local_water_environment,  -- What water network does WaterMap predict?
    buried_surface_area,      -- How much nonpolar surface is buried?
    hbond_density,            -- How many H-bonds are formed per unit area?
    ligand_efficiency,        -- Score per heavy atom (controls for size bias)
    rotatable_bonds,          -- Entropy penalty proxy
    formal_charge             -- Charged ligands have different solvation patterns
)
```

The function f is a **gradient-boosted tree ensemble** (XGBoost) trained on the FEP+ benchmark. Key design choice: the calibration is **additive** to the physics-based score, not a replacement. The physical terms (van der Waals, electrostatics, WaterMap ΔG_hyd) still carry the physics; the calibration corrects the systematic residual.

### Why Not a Full ML Scoring Function?

The calibration layer is deliberately **not** a full machine-learned scoring function. Full ML scoring (e.g., RF-Score, OnionNet, DeepDock) replaces the physics with pattern recognition from structural data. This has advantages (can learn complex patterns) but risks:
- **Overfitting**: High performance on PDBbind, poor performance on novel chemotypes
- **Opacity**: Hard to understand why a particular compound scores well
- **Domain shift**: Trained on crystal structures, applied to docked poses (which have errors)

Glide WS's approach — physics-based scoring + ML calibration — preserves interpretability while correcting systematic errors. The chemist can still read the score decomposition: "this compound scores well because of van der Waals (+2.1) and water displacement (+1.8), minus an entropy penalty (−1.2)."

## Magic Methyl Calibration

The "magic methyl" effect — where adding a single methyl group improves potency by 10–100× — is the calibration's hardest case. Standard terms predict +0.5–1.0 kcal/mol (van der Waals burial). The calibration layer must detect when the buried volume displaces a high-energy water.

### Detection Algorithm

```
For each methyl group addition in the FEP+ training set:
1. Compute ΔScore_physics = Score_WS_with_methyl − Score_WS_without_methyl
2. Compute ΔΔG_fep = FEP+ free energy difference (gold standard)
3. If ΔΔG_fep − ΔScore_physics > 1.0 kcal/mol:
   a. Check WaterMap: does the methyl overlap a high-energy hydration site?
   b. If yes: this is a magic methyl. The calibration learns to add
      +ΔG_hyd(hydration_site) when a methyl overlaps a high-energy water.
   c. If no: this is an unmodeled physics effect (protein reorganization, etc.)
```

The calibration learns the **water displacement bonus** from data, not from first principles. This is the practical reality of scoring function development: the physics provides the framework; the calibration corrects the numbers.

## FEP+ Integration

### When to Escalate from Glide WS to FEP+

Glide WS provides a score and a confidence estimate. Compounds with:
- High Glide WS score (> −9.0 kcal/mol)
- Small confidence interval (SEM < 0.5 kcal/mol)
- No unusual chemical features (unusual elements, macrocycles)

...can be trusted without FEP+ validation. Compounds with:
- Borderline scores (−7.0 to −9.0 kcal/mol)
- Large confidence intervals (SEM > 1.0 kcal/mol)
- Magic methyl candidates (discrepancy between Glide WS and additivity expectations)

...should be escalated to FEP+.

### Cost-Benefit Analysis

| Method | Cost per Compound | Accuracy (MUE, kcal/mol) | Value Ratio |
|---|---|---|---|
| Glide SP | $0.01 | 2.5 | Baseline |
| Glide XP | $0.05 | 2.0 | 2.5× |
| Glide WS | $0.50 | 1.5 | 1.3× |
| Nwat-MMGBSA | $5 | 1.2 | 1.3× |
| FEP+ | $500 | 0.84 | 1.4× |
| Experiment | $5,000 | 0.3 (assay variability) | 0.6× |

The "value ratio" is accuracy improvement per dollar. Glide WS → Nwat-MMGBSA (1.3×) is the sweet spot for most lead optimization programs. FEP+ is reserved for the final few compounds where a 0.5 kcal/mol error could drive the wrong decision.

## Jump-Cannon Calibration Parallel: Edge-Strength Damping

Jump-cannon's `edge_strength` module serves the same role as Glide WS's calibration layer — it modifies a base pairwise energy based on local context:

```rust
// The calibration function (edge_strength.rs)
// k_eff = k_base * edge_strength(e)
// where edge_strength ∈ [0, 1] captures local neighborhood structure
// Jaccard = T / (deg_u + deg_v - 2 - T)
// High Jaccard → strong spring → edge is "embedded" in cluster
// Low Jaccard → weak spring → edge is a "surprising" long-range connection
```

In both domains, the calibration:
1. Is **multiplicative** (modifies base interaction strength)
2. Depends on **local context** (water network / graph neighborhood)
3. Is **data-driven** (FEP+ benchmarks / empirical convergence behavior)
4. Corrects systematic bias (hydrophobic over-prediction / over-clustering)

The key difference: Glide WS calibrates against FEP+ experimental data, while jump-cannon's edge-strength is theoretical (Jaccard coefficient from graph theory). A future jump-cannon module could **learn** edge-strength from empirical layout quality assessments, analogous to the ML calibration layer.
