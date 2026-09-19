# Conformation Generation in Glide WS: The RDKit/ConfGen Hybrid

## The Conformation Problem

Docking requires generating low-energy 3D conformations of the ligand. The bioactive conformation is often NOT the global energy minimum in vacuum.

## Glide SP/XP: Standard Torsion Sampling

Historical Glide uses systematic torsion sampling:
1. Identify rotatable bonds
2. Assign torsion libraries from CSD
3. Combinatorial enumeration: N_confs = product of n_i across bonds
4. Clustering and pruning (RMSD < 0.5 A, cap at 1000)

Limitations: ring sampling from small pre-computed set, macrocycles unsupported, combinatorial explosion for flexible chains.

## Glide WS: RDKit/ConfGen Hybrid

### Non-Aromatic Rings: ConfGen Ring Templating

ConfGen pre-computes low-energy ring conformations from CSD and QM:
- Piperidine: 4-8 low-energy puckers (B3LYP/6-31G* verified)
- Cycloheptane: 12-20 pseudo-rotation conformations
- Macrocycles: distance geometry embedding

### Acyclic Flexible Chains: ConfGen Distance Geometry

Distance geometry generates conformations by satisfying constraints:
1. Build bond graph with bond-length and bond-angle constraints
2. Compute upper/lower bounds for all atom pairs (triangle inequality smoothing)
3. Random embedding within bounds -> convert to 3D
4. Refinement to valid geometry

Advantages: cost is O(n3) not O(product of n_i), diverse sampling, handles macrocycles.

### Rotatable Bonds: Standard Torsion Sampling

For standard rotatable bonds, WS uses the same torsion libraries as SP/XP.

## Performance Comparison

| Ligand Type | SP Confs | WS Confs | Improvement |
|---|---|---|---|
| Drug-like (<=5 rot bonds) | 100-300 | 100-300 | No change |
| Flexible (8-12) | 500-1000 capped | 200-400 | Better coverage, fewer |
| Piperidine | 4-8 ring | 8-12 ring | Rare puckers captured |
| Macrocycle (18) | FAIL | 50-100 | Newly supported |
| Cycloheptane | 6 ring | 14-20 ring | Better pseudo-rotation |

## Jump-Cannon Analog: Seed Position Generation

The seed_positions function solves the analogous problem for graph layout:

- Random torsion sampling ~ SeedMode::RandomBall - fast, no guarantees
- Distance geometry ~ SeedMode::TopoFisheye - global constraints, diverse
- Ring templating ~ SeedMode::Geometric - domain-specific knowledge
