# Glide SP vs. XP vs. WS: Comprehensive Comparison Matrix

## Scoring Function Components

| Component | SP (2004) | XP (2006) | WS (2024) |
|---|---|---|---|
| Van der Waals | Softened LJ 6-12 | + hydrophobic enclosure | + water-modulated scaling |
| Electrostatics | Coulomb, distance-dep eps | + dipole correction | + water-screened eps |
| Hydrogen bonds | ChemScore-like geometric | XP H-bond (dist+angle+charge) | + water-mediated bridge term |
| Metal coordination | Simple distance-based | Geometric + angle penalty | Same |
| Desolvation | SASA-based | SASA + site-based | WaterMap dG_hyd (explicit) |
| Hydrophobic enclosure | None | Enclosure score | Same |
| pi-pi stacking | None | pi-pi term | Same |
| pi-cation | None | pi-cation term | Same |
| Halogen bonds | None | Halogen bond term | Same |
| Water displacement | None | Static grid waters | Dynamic WaterMap waters |
| Water thermodynamics | None | None | dG_hyd, dH, -TdS per water |
| Magic methyl detection | No | No | Yes (via WaterMap) |
| Rotatable bond penalty | 0.3 kcal/mol/bond | Same | Same + entropic correction |

## Performance Metrics

| Metric | SP | XP | WS |
|---|---|---|---|
| Pose prediction (RMSD < 2.0 A) | 70% | 75% | 85% |
| Self-docking accuracy | 88.7% | 91% | 98% |
| Affinity correlation (Pearson R) | 0.65 | 0.75 | 0.80 |
| Enrichment (EF 1%, DUD-E avg) | 8.5x | 12.3x | 18.7x |
| ROC AUC (DUD-E avg) | 0.62 | 0.68 | 0.78 |
| False positive rate (vs XP) | +25% | Baseline | -40% |

## Throughput and Hardware

| Metric | SP | XP | WS |
|---|---|---|---|
| Time per ligand | 2 sec | 10 sec | 60 sec |
| Throughput (ligands/hr) | 1,800 | 360 | 60 |
| GPU required | No | No | Recommended |
| Pre-computation | Receptor grid | Receptor grid | WaterMap (1-4 hr) + grid |

## Use Cases

| Application | SP | XP | WS |
|---|---|---|---|
| Ultra-large VS (>1M) | Best | Too slow | Too slow |
| Large VS (100K-1M) | Good | Better | Too slow |
| Focused VS (1K-100K) | Can use | Better | Best (water targets) |
| Hit triage (100-1K) | Skip | Can use | Best |
| Lead optimization (10-100) | Skip | Can use | Best |
| Magic methyl detection | No | No | Yes |
| Scaffold hopping | Fair | Good | Best |
| Macrocycles | Not supported | Not supported | Supported |
| Covalent docking | Supported | Supported | In development |

## Cost-Effectiveness

| Transition | AUC Gain | Cost Increase | Efficiency |
|---|---|---|---|
| SP -> XP | +0.06 | 5x | 0.012 AUC/$ |
| XP -> WS | +0.10 | 6x | 0.017 AUC/$ |
| SP -> WS | +0.16 | 30x | 0.005 AUC/$ |

The XP -> WS transition is the most cost-effective per-unit AUC improvement.

## References

- Friesner et al. (2004). J. Med. Chem. 47(7): 1739-1749.
- Friesner et al. (2006). J. Med. Chem. 49(21): 6177-6196.
- Schrodinger (2024). "20 Years of Glide." Technical White Paper.
