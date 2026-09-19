# GPU Acceleration of the Nwat-MMGBSA Protocol

## Hardware Evolution

The Nwat-MMGBSA protocol has tracked GPU hardware improvements closely. What was impractical on CPU clusters in 2015 became overnight workstation jobs by 2018 and lunch-break jobs by 2022.

### Performance Scaling

| GPU Generation | Year | Representative Card | ns/day (JAC benchmark) | Nwat-MMGBSA per compound |
|---|---|---|---|---|
| Kepler | 2013 | GTX TITAN Black | ~60 | 8 hours |
| Maxwell | 2015 | GTX 980 Ti | ~100 | 5 hours |
| Pascal | 2016 | GTX 1080 Ti | ~180 | 2.7 hours |
| Volta | 2017 | Tesla V100 | ~350 | 1.4 hours |
| Turing | 2018 | RTX 2080 Ti | ~200 | 2.5 hours |
| Ampere | 2020 | RTX 3090 / A100 | ~500 / ~800 | 1 hour / 40 min |
| Ada Lovelace | 2022 | RTX 4090 | ~500 | 1 hour |
| Hopper | 2023 | H100 | ~1,200 | 25 min |

### The pmemd.cuda Advantage

AMBER's GPU-accelerated MD engine (pmemd.cuda) achieves near-linear scaling with GPU compute units for systems up to ~100K atoms. The key optimizations:

1. **Nonbonded force calculation**: The O(N²) electrostatic and van der Waals computation runs on GPU via particle-mesh Ewald (PME). Direct-space nonbonded interactions are computed in a CUDA kernel; reciprocal-space Ewald summation runs on a separate stream, overlapping with the next step's direct-space calculation.

2. **Bonded forces**: Bonds, angles, dihedrals are computed on GPU. These are O(N) but latency-sensitive; pmemd.cuda uses warp-level parallelism to minimize latency.

3. **Langevin thermostat**: The random force generation and velocity scaling are fused into the integration kernel, avoiding a separate kernel launch.

### Multi-GPU Scaling

For large systems (>200K atoms), pmemd.cuda supports multi-GPU via domain decomposition:

| GPUs | Scaling Efficiency | Notes |
|---|---|---|
| 1 | 1.0× (baseline) | Single GPU |
| 2 | 1.7–1.9× | Near-linear for large systems |
| 4 | 2.8–3.5× | Communication overhead begins |
| 8 | 4.5–6.0× | Diminishing returns |

For Nwat-MMGBSA (typically 30–80K atoms, including explicit solvent), single-GPU is optimal. Multi-GPU adds communication overhead without proportional speedup for these system sizes.

## Jump-Cannon GPU Architecture Comparison

Jump-cannon's `graph-compute` engine achieves similar GPU scaling patterns:

| Jump-Cannon Engine | GPU Utilization | Scaling Bottleneck |
|---|---|---|
| `fa2-brute` | O(n²) kernel, GPU-bound | Memory bandwidth (reading all positions) |
| `fa2-bh` | O(n log n), octree build host-side | Host→GPU octree upload |
| `fa2-bh` (future) | Fully-GPU octree (`octree.wgsl`) | Shader occupancy |
| `multilevel.wgsl` | Fully-GPU cascade | Sort/scan atomics throughput |

The key bottleneck common to both domains: **host↔GPU data movement**. In Nwat-MMGBSA, trajectory frames must be written to disk and read by cpptraj. In jump-cannon's current `fa2-bh`, the octree is built on the host and uploaded each step. In both cases, the fix is the same: **move the operation onto the GPU**.

For Nwat-MMGBSA, this would mean a CUDA kernel that selects closest waters directly from GPU memory without disk I/O. For jump-cannon, this is the `octree.wgsl` fully-GPU octree builder — already implemented, eliminating the host build bottleneck.

## Throughput Analysis: CPU vs. GPU

| Stage | CPU (16-core) | GPU (RTX 4090) | Speedup |
|---|---|---|---|
| MD production (20 ns) | 40 hours (2 ns/day) | 1 hour (500 ns/day) | 40× |
| Water selection (cpptraj) | 2 minutes | 0.1 seconds (script) | 1,200× |
| MM-GBSA energy | 30 minutes | 5 minutes (MMPBSA.py MPI) | 6× |
| **Total per compound** | **~41 hours** | **~1.1 hours** | **~37×** |

The MD production step dominates the total cost. GPU acceleration of MD is the single most impactful optimization.

## Cost Analysis

| Platform | Hardware Cost | Cost per Compound | Cost per 500-Compound Screen |
|---|---|---|---|
| 16-core CPU cluster (on-prem) | $20K (amortized) | ~$4.50 | ~$2,250 |
| Cloud CPU (c5.18xlarge, spot) | ~$1.50/hr | ~$62.00 | ~$31,000 |
| Single RTX 4090 workstation | $4K (amortized) | ~$0.12 | ~$60 |
| Cloud GPU (p3.2xlarge, spot) | ~$3.00/hr | ~$3.30 | ~$1,650 |
| Cloud GPU (g5.xlarge, spot) | ~$1.50/hr | ~$1.65 | ~$825 |

A single RTX 4090 workstation can rescore 500 compounds for ~$60 in electricity — the same job on CPU cloud would cost $31,000. GPU acceleration makes Nwat-MMGBSA practical for medium-throughput screening.
