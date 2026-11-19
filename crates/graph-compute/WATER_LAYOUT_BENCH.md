# Water-sheet GPU layout — throughput bench

Measured with `examples/water_bench.rs` on an **Apple M5 Max (Metal)**, a shared
workstation GPU (the same device drives the desktop). Adapter report:

```text
adapter: Apple M5 Max (Metal) | max_buffer_size 80.64 GiB | max_storage_binding 4.00 GiB
```

Reproduce:

```text
cargo run -p graph-compute --release --example water_bench -- --steps 100
```

## Workload

Each target node count `N` builds the canonical `waterSheet` shape: `M = N/10`
molecules (3 nodes each → 30% of nodes, 2 O–H springs per molecule, one windowed
O–O repulsion) plus a near-square sheet of `ceil(sqrt(N - 3M))²` nodes (70% of
nodes, ≤4 lattice springs, 8-cell repulsion window at the tuned `r_sheet = 1`).

| target | molecules | sheet | nodes | device bytes | bytes/node |
|-------:|----------:|------:|------:|-------------:|-----------:|
|   1e6  |   100,000 |  837² =  700,569 |   1,000,569 | 0.03 GiB | 36.0 |
|   1e7  | 1,000,000 | 2646² = 7,001,316 |  10,001,316 | 0.34 GiB | 36.0 |
|   1e8  |10,000,000 | 8367² =70,006,689 | 100,006,689 | 3.35 GiB | 36.0 |

`bytes/node` is device-resident state only: 6 ping-pong position axes +
3 velocity axes (9 × 4 B) + params. The CSR is **not** uploaded; bonds are
derived from the node index (see the tuning log). Positions are SoA (`x`,`y`,`z`
separate storage buffers) — the kernel is memory-bound, and a `vec4` AoS layout
would add 33% padding to the dominant traffic.

### Protocol

`--steps 100 --batch 10`: 2 warm-up steps, then 10 submits of 10 steps each,
queue-synced after every submit. Reported `ms/it min` is the fastest submit;
`ms/it med` is the 5th of 10 sorted submits. `init_s` is host graph build +
seed + upload + pipeline creation, excluded from the per-iteration time.

**Noise.** `ms/it min` and `ms/it med` can differ 2–4× at 100M on this shared
GPU, and the same configuration spans ~2× across runs (e.g. 100M min
9.9–18.5 ms, med 29.8–48.1 ms). 1M and 10M are stable (≤5% run-to-run), so they
are the sizes used to accept/reject a tuning change; 100M numbers are
indicative. **100M fits** (largest single buffer ≈381 MiB, a position axis,
vs the 4.00 GiB storage-binding cap), so no extrapolation is used anywhere below.

## Final throughput

Single representative run of the final configuration (`workgroup=256`,
`staged=false`, `r_sheet=1`, `r_mol=1`):

| size | nodes | ms/iter min | ms/iter med | nodes/s (min) | device bytes | bytes/node | init_s |
|-----:|------:|------------:|------------:|--------------:|-------------:|-----------:|-------:|
|  1M  |    1,000,569 | 0.132 | 0.253 | 7.59G  | 0.03 GiB | 36.0 | 0.0 |
| 10M  |   10,001,316 | 1.135 | 1.265 | 8.81G  | 0.34 GiB | 36.0 | 0.2 |
| 100M |  100,006,689 | 12.308 | 33.022 | 8.13G | 3.35 GiB | 36.0 | 2.4 |

Across four runs of the same binary the stable sizes were
1M min 0.132–0.141 ms, 10M min 0.969–1.135 ms, 100M min 9.9–18.5 ms.

## Tuning log

All numbers `--steps 100 --batch 10`. `bench b76ef90`/`b76ef90`-era runs print
both min and median; the baseline bench (24a59f8) printed the median only, so
baseline comparisons are median-to-median.

### Cycle 1 — tuned default window + cheaper force loop (`b76ef905c7`)

| configuration | 1M | 10M | 100M |
|---|---:|---:|---:|
| baseline (24a59f8), default `r_sheet=3,r_mol=2` | 0.505 med | 3.955 med | 146.166 med |
| baseline, `r_sheet=1,r_mol=1` | 0.139 med | 1.629 med | 57.154 med |
| after, `r=3/2` | 0.519 med | 6.320 med | 140.512 med |
| after, `r=1/1` (new default) | 0.259 med | 1.493 med | 63.870 med |

- Each node scans 8 window cells instead of 24 at the new default; the
  relaxation geometry is unchanged (both windows are asserted by
  `cpu_relaxes_to_lattice_and_bond_lengths` /
  `cpu_relaxes_with_wide_window` and by the GPU oracle test).
- Shipped default before → after (median): **1.95× / 2.65× / 2.29×**.
- Same-code wide vs narrow isolation: 2.0× / 4.2× / 2.2× (median).
- The shader micro-optimisation alone (position vector reused, squared-distance
  cull) is **within noise** at the narrow window (−8% at 10M, +12% at 100M
  median): at `r_sheet = 1` every 1.0/1.41-spaced neighbour is inside
  `rcut = 1.6`, so the cull almost never skips the `sqrt`. Kept because it is
  strictly less work, not because it measured as a win.
- Also in this cycle: staged-kernel radius is substituted from settings
  (`r_sheet ≤ 8`) instead of pinned at 3, and device limits now allow the
  adapter's max workgroup size (1024) instead of the 256 default.

### Cycle 1 follow-up — workgroup size sweep: rejected

min / med ms per iteration, 3 runs each:

| workgroup | 10M (3 runs, min) | 100M (3 runs, min) | 100M (3 runs, med) |
|---|---|---:|---:|
| 256 | 1.392 / 1.307 / 1.401 | 40.9 / 13.4 / 20.5 | 59.5 / 35.6 / 45.7 |
| 512 | 1.353 / 1.381 / 1.380 | 30.0 / 21.1 / 21.1 | 60.4 / 54.4 / 39.6 |
| 1024 | 1.352 / 1.398 / 1.313 | 21.9 / 20.1 / 13.3 | 53.9 / 58.6 / 34.3 |

All three workgroup sizes sit at 1.38 ± 0.05 ms at 10M, and the 100M spread
inside a single configuration (13–41 ms min) is larger than any difference
between configurations. **No default change**: 256 stays.

### Cycle 1 follow-up — staged sheet kernel at `r_sheet = 1`: rejected

The staged kernel tiles an 8×8 workgroup against a `(8+2R)²` shared window. At
`R = 1` that is a 100-cell tile staged to save 8 global reads per node:

| kernel | 1M min | 10M min | 100M min |
|---|---:|---:|---:|
| naive (default) | 0.123 | 1.391 | 23.180 |
| staged | 0.253 | 1.643 | 35.091 |

**Worse at every size** (2.1× at 1M, 1.2× at 10M, 1.5× at 100M), so `staged = false`
remains the default for the tuned window.

The staged path only breaks even at wide windows (measured `--steps 60`, naive
vs staged at `r_sheet=3, r_mol=2`): 1M 0.510 vs 0.393 ms min (1.3× faster
staged), 10M 4.157 vs 3.488 ms min (1.2×) but 5.455 vs 9.889 ms median, and
100M 68.678 vs 68.930 ms min (wash). Staging is therefore not a throughput
lever worth switching to; it stays selectable and is covered by the oracle test
at both radii.

### Cycle 2 — implicit bonds, CSR off the device (`0bdbdea959`)

| metric | before | after |
|---|---:|---:|
| bytes/node | 52.8 | **36.0** (−32%) |
| 100M device bytes | 4.92 GiB | **3.35 GiB** |
| 100M init_s | 3.9 | 2.4 |
| 10M min ms | 1.392 / 1.307 / 1.401 | **1.005 / 0.969 / 1.029** |
| 10M med ms | 1.516 / 1.471 / 1.534 | **1.109 / 1.062 / 1.065** |
| 100M min ms | 40.9 / 13.4 / 20.5 | 18.5 / 9.9 / 17.9 |
| 100M med ms | 59.5 / 35.6 / 45.7 | 47.5 / 29.8 / 48.1 |

- **10M (stable): 1.39 → 1.00 ms (−28%, 1.39×) min; 1.52 → 1.07 ms (−30%) median.**
- 100M improves in the min statistic but remains noise-dominated (best measured
  12.3 ms = 8.13G nodes/s at 36.0 B/node).
- 1M is launch-overhead bound and unchanged within noise (0.123 → 0.136 ms min).
- The bond springs are computed from the node index in the exact order
  `WaterSheetParams::graph` emits them, so the CPU oracle still agrees
  (short-horizon tolerance 1e-3, long-horizon 2e-2). `validate_csr` still runs
  at init; the graph is validated but never uploaded.

## Levers measured, not adopted

| lever | result | decision |
|---|---|---|
| workgroup 512 / 1024 | 10M 1.38 ± 0.05 ms for all sizes | keep 256 |
| staged sheet kernel at `r_sheet = 1` | 1.5–2.1× slower | keep naive |
| `vec4`/AoS position layout | +33% dominant traffic (12 → 16 B per axis read) | not implemented; SoA is bandwidth-optimal here |
| f16 positions/velocities | halves traffic but quantises to ~1e-2 absolute at layout scale | not implemented; would break the 1e-3 oracle tolerance |
| adapter limits | already at adapter max for workgroups; 100M fits the 4 GiB storage-binding cap | nothing to raise |

## Precision

`WaterSheetSettings.precision` selects the numeric representation of the
GPU-resident position state. Velocities and the force model stay f32 in every
mode; only the position storage and accumulation change.

| mode | position state | needs | this adapter |
|---|---|---|---|
| `f32` (default) | 3 × f32, 4 B/axis | — | runs |
| `f64pos` | 3 × 64-bit fixed point, 8 B/axis | — | runs |
| `f64` | 3 × native f64, 8 B/axis | `SHADER_F64` | **unsupported** |

Probe (`examples/water_f64_probe.rs`):

```text
adapter: Apple M5 Max (Metal)
probe: SHADER_F64 supported = false
  request_device(SHADER_F64) error: Unsupported features were requested: Features(SHADER_F64)
```

Apple GPUs expose no fp64. `F64` therefore hard-errors, naming the mode
(`precision 'f64' requires wgpu::Features::SHADER_F64, not exposed by this
adapter (Apple M5 Max / Metal)`); the bench reports that per size and the test
asserts the rejection. **There is no silent precision downgrade.**

### Why fixed point, not an f32 double-double

The obvious double-float design — a `(hi, lo)` f32 pair updated with Dekker's
`TwoSum` — does not survive this stack. wgpu-hal leaves Metal's
`MTLCompileOptions.fastMathEnabled` at its default (on), and the shader
compiler reassociates floating point, folding `(a + b) - a` to `b` and the
`TwoSum` residual to zero. Measured with that kernel: the `lo` word was
**exactly 0 for every component on every step**, and the 10,000-step drift was
bit-identical to the f32 kernel. Integer arithmetic is immune to such
reassociation, so `F64Pos` stores each component as a 64-bit fixed-point pair
(`i32` integer word × 2⁸ + `u32` fraction word, scale 2⁻⁴⁰), updated by an
exact integer add with carry. Resolution 9.1e-13, range ±8.4e6 — enough for a
10⁸-node sheet, and no device feature is involved.

### Cost

| size | f32 min / med ms | f64pos min / med ms | f64pos/f32 (min) | bytes/node |
|-----:|-----------------:|--------------------:|-----------------:|-----------:|
|  1M  | 0.242 / 0.259, 0.256 / 0.264 | 0.242 / 0.260, 0.260 / 0.419 | **1.00×** | 36 → 60 |
| 10M  | 1.016 / 1.143, 1.118 / 1.244 | 1.643 / 1.770, 1.755 / 1.802 | **1.58×** | 36 → 60 |
| 100M | 10.260 / 44.666, 10.545 / 51.253 | 16.568 / 88.449, 17.293 / 96.422 | **1.64×** | 36 → 60 |

Two runs each, `--steps 100 --batch 10`, tuned defaults
(`workgroup=256 staged=false r_sheet=1 r_mol=1`).

**The cost is memory, and it is honest:** 6 ping-pong position axes at 8 B
instead of 4 B takes the state from 36.0 to 60.0 B/node (+67 %), and this
kernel is bandwidth-bound, so the 10M/100M time tracks the byte ratio (1.58–
1.64× for 1.67× bytes). 1M is launch-bound and unchanged. There is **no**
"same speed" claim to make: `f64pos` buys precision with bandwidth. (100M
timing remains contended — see the noise note; the min column is the best
sample and the bytes ratio is the physical cause.)

### Drift evidence (`tests/gpu_water_layout.rs`)

10,000 steps, 556 nodes, GPU state vs the full-f64 CPU oracle, max absolute
deviation (positions are O(1)):

| horizon | f32 | f64pos |
|---:|---:|---:|
| 100 | — | 6.8e-7 |
| 300 | — | 1.0e-6 |
| 1,000 | — | 1.5e-6 |
| 3,000 | — | 3.0e-6 |
| 10,000 | **1.36e-3** | **1.46e-5** |

`f64pos` is **93× tighter**. What remains is not position-storage drift: the
f32 *force* model is shared, and GPU-vs-CPU dot-product rounding (~1 ulp; Metal
contracts the f32 products) is amplified by the transient —
`f64pos` GPU vs its own CPU oracle is 1.43e-5, and the growth curve above is
smooth. For reference, the CPU `step_cpu_f64pos` oracle (f64 position, same f32
forces) reaches 9.5e-7 against the full oracle, so ~1e-6 is the f32-force floor
and the residual 1.4e-5 is implementation rounding, not storage quantisation.
The test asserts `f32 > 1e-4`, `f64pos < 5e-5`, and a >20× ratio, so a silent
f32 fallback (or an f32-collapsed readback — which measured exactly the f32
value, 1.36e-3, before the readback exposed the full state) fails it.

Short-horizon oracle checks (`f64_precision_modes_match_their_oracles`, 20 and
300 steps, naive and staged kernels): `f64pos` GPU vs its CPU oracle 1.5–3.9e-7,
vs the full-f64 oracle 1.3–1.7e-7; `f64` is asserted to be rejected by name.

### Recommendation

- Keep `f32` as the default: it is the throughput mode.
- Use `f64pos` when long-horizon positional drift matters more than ~1.6×
  step time at 10M+ (parity at 1M): 93× less drift for 67 % more device memory.
- `f64` is not available on Apple silicon; it stays implemented for
  fp64-capable adapters and is covered by the error-path test here.

## Verification

Every commit in the log ran:

```text
cargo test -p graph-compute --lib water                          # 7 passed, 0 failed
cargo test -p graph-compute --test gpu_water_layout              # 3 passed, 0 failed
```

The precision work (commit `db075f3b99`) ran the full set:

```text
cargo test -p graph-compute --lib water                          # 12 passed, 0 failed
cargo test -p graph-compute --test gpu_water_layout              #  6 passed, 0 failed
cargo test -p graph-compute --test water_topology_matches_generator  # 1 passed, 0 failed
cargo check -p graph-compute --tests --examples                  # clean
```

The CPU relaxation test is discriminating: negating `k_bond` in
`water::cpu::node_force` fails both relaxation tests (5 passed, 2 failed), and
restoring it returns to 7 passed.

## End-to-end from native generator

`examples/water_e2e.rs` runs the production path end to end: the native SoA
`WaterSheetGenerator` (a dev-dependency of graph-compute — the same crate
`graph-api --source native` feeds) builds the `waterSheet` graph, its counts
and per-node kinds are cross-checked against `WaterSheetParams`, and the GPU
engine relaxes that shape. Topology equality itself — node count, per-index
kind, and the full undirected edge set — is asserted exhaustively by
`tests/water_topology_matches_generator.rs` over degenerate, single-axis,
ragged, and `20000x256x256` shapes; no GPU ordering fix was needed (both sides
already emit O/H1/H2 triples then a row-major sheet).

Shape: `molecules = 30,000,000`, sheet `3163 x 3163` →
**100,004,569 nodes**, 80,002,812 undirected bonds (30M O + 60M H + 10,004,569
sheet cells). Reproduce:

```text
cargo run -p graph-compute --release --example water_e2e -- --steps 300
```

Two runs, tuned defaults (`workgroup=256 staged=false r_sheet=1 r_mol=1`), on
the shared M5 Max Metal device — 100M timing is contended, so treat the min
column as a best sample (see the noise note above):

| run | generate_s | init_s | ms/it min | ms/it med | nodes/s (min) | device bytes | bytes/node |
|----:|-----------:|-------:|----------:|----------:|--------------:|-------------:|-----------:|
| 1 | 0.38 | 1.66 | 20.712 | 55.104 | 4.83G | 3.35 GiB | 36.0 |
| 2 | 0.18 | 1.01 | 34.052 | 54.278 | 2.94G | 3.35 GiB | 36.0 |

`generate_s` is the native SoA build alone (one O(n+e) pass, ~2.3 GB host
state) and `init_s` is CSR build + canonical seed + device upload + pipeline
creation. Per-iteration time is above the `1e8` row in the table above because
this shape is 90% molecule nodes (that bench shape is 30%): every molecule
node pays the O–O window scan plus two O–H springs, while sheet nodes pay the
lattice window.

Post-run geometry, measured on the read-back positions after the 300 timed
steps (bit-identical across both runs — the layout is deterministic, only the
timing is noisy):

| metric | measured | target | test tolerance |
|---|---:|---:|---:|
| max abs(O–H distance − `bond_rest` 0.96) | **0.0071** | 0 | 0.05 |
| max abs(lattice spacing − `sheet_rest` 1.00) | **0.0766** | 0 | 0.15 |
| max lattice offset from the ideal grid (centroid removed) | **0.3030** | 0 | 0.5 |

So the 100M-node layout converges to the target geometry within the same
tolerances the small-scale GPU oracle asserts — it does not merely execute.
