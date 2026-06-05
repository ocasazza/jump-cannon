# Geometric constraint engine — implementation plan

Status of the feature and the concrete next steps, after the backend solver
landed (commit `7c41762`, branch `feat/geometric-constraint-engine`).

Companion to [`compute-architecture.md`](compute-architecture.md) (where engines
plug in) and [`layout-algorithms.md`](layout-algorithms.md) (the algorithm
survey). This doc is about *finishing the geometric engine end-to-end*.

---

## 0. What exists today (done)

A generic, composable **geometric constraint solver** in `graph-compute`
(`engines/geometric.rs`, id `"geometric"`), framed in pure geometric — not
molecular — language. A molecular force field is one *instantiation* of it.

- **Forces:** edge-length springs + neighbour-angle (coordination) constraints +
  per-class exclusion/affinity + mass-scaled gravity.
- **Composable lens:** each geometric role (`class`, `coordination`, `mass`,
  `edge_len`) resolves from a pluggable source — either **structural** (computed
  from CSR topology: degree, label-propagation community, PageRank) or
  **injected** (`GraphAttributes` shipped alongside the CSR).
- **Wire-adjacent plumbing:** `GraphAttributes` payload + `CsrShard.attributes`
  field + length validation.
- **Tests:** 13 unit tests (resolvers, injected path, qualitative
  crystallization behaviours); 51/51 `graph-compute` lib tests green. CPU only.

What's missing is everything that connects this engine to a *user*: the wire
fields to carry injected attributes, the **producer** that resolves a user's
mapping into those vectors, and the **UI** to choose the mapping. Plus the GPU
and distributed follow-ups.

---

## 1. Architecture decision: graph-api is the attribute producer

The renderer does **not** speak gRPC to `graph-compute`. The path is:

```
graph-renderer (WASM, egui)            has: the lens CHOICE (UI)
      │  WS /graph/layout/stream  (engine params incl. lens config)
      ▼
graph-api  compute_broker              has: VaultGraph metadata + graph-metrics
      │                                      (tags, type, frontmatter, weight,
      │                                       Louvain community, PageRank, …)
      │  gRPC Subscribe { layout_id, params, ATTRIBUTES }   ◄── resolves here
      ▼
graph-compute  geometric engine        has: CSR topology only
```

**graph-api is the right place to resolve attributes** because it is the only
hop that holds *both* the semantic metadata (tags/type/frontmatter/weight) and
the precomputed structural metrics (`NodeMetrics { community, pagerank, … }` from
`graph-metrics`). The topology-only `graph-compute` cannot derive semantic
attributes at all, and re-deriving structural ones there would duplicate
`graph-metrics`.

### The two source vocabularies + the translation layer

There are deliberately **two** source vocabularies, with graph-api translating
between them:

| Layer | Vocabulary | Example |
|---|---|---|
| **Frontend / UI** (rich) | by tag, by frontmatter field, by node type, by edge weight, by edge type, Louvain, PageRank, degree, uniform | `class = frontmatter["folder"]` |
| **Backend** (`geometric.rs`, already built) | `Injected` \| structural `{degree, community, pagerank}` | `class_source = Injected` |

graph-api's resolver maps frontend → backend:

- **Semantic sources** (tag/field/type/weight) → graph-api resolves them into
  numeric vectors from the `VaultGraph`, sets the backend source to `Injected`,
  and ships the vectors.
- **Structural sources** (Louvain/PageRank/degree) → graph-api *also* resolves
  them from `NodeMetrics` (single source of truth, matches what the rest of the
  UI colours by) and ships them as `Injected`.

The backend's own structural resolvers (`label_propagation`, `pagerank`,
`compute_degree`) are kept as the **standalone path**: when `graph-compute` runs
without graph-api (tests, a direct CSR file, the `--demo` graph) it can still
resolve structural sources itself. Production always injects.

### Node ordering contract

Injected vectors are **parallel to the backend graph's node order**, which is the
id-sorted order the `/graph/csr.bin` exporter already uses (positions use the
same order). graph-api resolves in that order; no new ordering contract needed.
`edge_len` is parallel to `CsrGraph.neighbors` (the canonical CSR entry order).

---

## 2. Phasing (DAG)

```
        ┌─────────────────────────────────────────────────────────┐
        │ DONE: backend geometric engine + GraphAttributes type    │
        └───────────────────────────┬─────────────────────────────┘
                                     │
        ┌────────────────────────────┴───────────────┐
        ▼ (A) wire                                    ▼ (B) UI scaffolding
  proto GraphAttributes msg                   egui mapping panel shell
  + SubscribeRequest field                    (source dropdowns, presets)
  + service.rs decode→host                    against a LensConfig struct
  + sim.rs thread into CsrShard               (no resolution yet)
        │                                             │
        ▼ (C) producer  ◄── depends on A             │
  graph-api attribute resolver                        │
  (VaultGraph + NodeMetrics → vectors)                │
  + compute_broker ships attributes                   │
        │                                             │
        └─────────────────┬───────────────────────────┘
                          ▼ (D) integrate
              renderer LensConfig → broker → resolver → engine
              end-to-end; verify in-browser
                          │
            ┌─────────────┴─────────────┐
            ▼ (E) GPU port              ▼ (F) distributed
      octree exclusion + WGSL     attributes shard with
      angle/edge kernels          partitions (ghost attrs)
```

A and B are independent and parallelizable. C depends on A. D joins them. E and F
are independent follow-ups after D.

---

## 3. Phase A — wire extension

**Goal:** carry injected `GraphAttributes` over gRPC, raw-LE per the wire rule.

### A.1 `proto/compute.proto`

```protobuf
// Injected per-node / per-edge geometric attributes (parallel to the backend
// graph's id-sorted node order; edge_len parallel to CSR neighbors). Bulk
// numeric ⇒ raw little-endian per the repo wire rule. Each field optional:
// empty bytes = absent (engine falls back to a structural source).
message GraphAttributes {
  bytes node_class        = 1;  // raw LE u32, len n_nodes
  bytes node_coordination = 2;  // raw LE u32, len n_nodes
  bytes node_mass         = 3;  // raw LE f32, len n_nodes
  bytes edge_len          = 4;  // raw LE f32, len neighbors.len()
}

message SubscribeRequest {
  string graph_id  = 1;
  string layout_id = 2;
  google.protobuf.Struct params = 3;
  GraphAttributes attributes = 4;  // NEW; absent ⇒ structural/standalone path
}
```

### A.2 `service.rs` — decode proto → host `GraphAttributes`

Mirror the existing `proto_to_host` / `decode_bytes` pattern used for
`HaloDelta`. Add:

```rust
fn proto_attrs_to_host(a: proto::GraphAttributes) -> Result<HostGraphAttributes, Status>
```

- `bytemuck`-cast each non-empty blob; **enforce alignment + length** (`u32`/`f32`
  multiples). Return `Status::invalid_argument` on a bad blob (the boundary where
  malformed requests are rejected — same posture as ADR-002 params).
- Empty blob → `None` for that field.
- In `subscribe`, when `want_select`, decode `req.attributes` and pass the host
  `GraphAttributes` to `init_engine`.

### A.3 `sim.rs` — thread attributes into `CsrShard`

`SimState::init_engine` currently builds `CsrShard::whole(&graph)`. Extend it to
accept `Option<GraphAttributes>` and build
`CsrShard::whole_with_attributes(&graph, &attrs)` when present. Store the host
attributes in `ActiveEngine` (or pass by value into the blocking init closure;
they're owned `Vec`s, so move them in). The fallback engine path must also pass
them through.

### A.4 Tests

- `proto_attrs_to_host` round-trips and rejects a misaligned / wrong-length blob.
- A `SimState::init_engine` test that injects a `node_class` vector and asserts
  the engine resolved it (run one step, no panic, correct length out).

**Verifiable here:** yes (`cargo test -p graph-compute`).

---

## 4. Phase B — UI scaffolding (egui)

**Goal:** the mapping panel as a `render_ui(ui, &mut Value)` for the geometric
engine — the centerpiece. The repo auto-generates **no** settings UI (descriptors
carry no param metadata), so this is hand-written, matching the existing
per-layout panels under `graph-renderer/src/ui/layout/algorithms/`.

### B.1 A frontend `LensConfig`

A serde struct the UI edits and that travels in the engine params. Distinct from
the backend `GeometricSettings` because it speaks the **rich** vocabulary:

```rust
struct LensConfig {
    class:        ClassLens,        // Uniform | DegreeBuckets | Louvain
                                    //   | Field(String) | Tag(String) | NodeType
    coordination: CoordinationLens, // Degree | Uniform(u32) | Field(String)
    mass:         MassLens,         // Uniform | Degree | PageRank | Field(String)
    edge_length:  EdgeLengthLens,   // Uniform | Weight | EdgeType
    // shared geometric knobs (pass straight through to GeometricSettings):
    edge_stiffness, angle_stiffness, exclusion_strength, affinity_strength,
    gravity, coordination_angles, class_radius, class_affinity, …
}
```

### B.2 The panel

- **Presets row** (buttons that fill the whole config), mirroring `gpu_force.rs`'s
  preset pattern:
  - *Crystallize motifs* — degree→coordination, angle on, uniform class.
  - *Separate communities* — Louvain→class, negative cross-affinity, low angle.
  - *Core–periphery* — PageRank→mass, higher gravity.
  - *Molecular* — preset coordination-angle + radius + affinity tables.
- **Per-role source dropdowns.** `Field`/`Tag`/`NodeType` options are populated
  from the loaded graph's schema (graph-api exposes a field schema;
  `vault-data::FieldSchema`). A field dropdown lists frontmatter keys; a tag
  dropdown lists known tags.
- **Advanced (collapsible):** the angle table, per-class radius, and the affinity
  matrix grid. Use the existing `row()`, `subgroup_label()`, `subgroup_separator()`
  helpers from `ui/widgets.rs`.
- Persisted automatically via `LayoutState::settings[id]` (serde) — no extra work
  once `LensConfig: Serialize + Deserialize + Default`.

### B.3 Tests

- `LensConfig` serde round-trip + default.
- (Browser visual verification deferred to Phase D.)

**Verifiable here:** serde tests yes; visual no (needs `just test browser`).

---

## 5. Phase C — producer (graph-api)

**Goal:** resolve a `LensConfig` into `GraphAttributes` from the `VaultGraph` +
`NodeMetrics`, and ship them on the gRPC `Subscribe`.

### C.1 Resolver (`graph-api`, new module e.g. `attribute_resolver.rs`)

Pure function over the in-memory graph snapshot:

```rust
fn resolve(lens: &LensConfig, snapshot: &GraphSnapshot)
    -> (GeometricSettings, GraphAttributes)
```

- Iterate nodes in the **id-sorted CSR order** (reuse whatever the csr.bin
  exporter uses — `build_csr_bin` ordering) so vectors line up with the backend
  graph.
- `class`: Louvain→`NodeMetrics.community`; Field/Tag/NodeType→categorical encode
  (string→dense u32 via a stable map); Degree buckets→from degree.
- `coordination`: Degree→degree (clamped); Field→numeric field.
- `mass`: PageRank→`NodeMetrics.pagerank`; Degree→degree; normalize to the range
  in the lens.
- `edge_len`: Weight→`Edge.weight` mapped to length (parallel to CSR neighbors —
  walk the same adjacency the exporter walks); EdgeType→categorical→length.
- Emit `GeometricSettings` with backend sources set to `Injected` for every role
  the resolver populated, and pass through the shared geometric knobs.
- Categorical encodings (tag/type/field) also need to size the class table:
  return sensible default `class_radius` length = #classes and a neutral (or
  preset) affinity matrix unless the lens overrides.

### C.2 `compute_broker.rs`

- Extend the broker's layout config to carry the `LensConfig` (today it reads
  `JUMP_CANNON_COMPUTE_LAYOUT_ID/PARAMS` from env; add the renderer-driven path).
- On subscribe: call `resolve`, build the gRPC `SubscribeRequest` with
  `layout_id = "geometric"`, `params = json_to_struct(settings)`, and
  `attributes = encode(graph_attributes)` (raw-LE blobs).
- Keep the env path working for headless/standalone deploys.

### C.3 Tests

- Resolver: a small fixture `VaultGraph` → assert vector contents + ordering for
  each lens kind (Louvain, tag, weight).
- Broker: the assembled `SubscribeRequest` carries non-empty attribute blobs of
  the right lengths.

**Verifiable here:** yes (unit tests, no live server needed).

---

## 6. Phase D — integrate + verify

- Wire the renderer's `LensConfig` into the `/graph/layout/stream` request body
  so it reaches the broker.
- End-to-end smoke: pick "Separate communities", confirm frames stream and the
  layout phase-separates; pick "Crystallize motifs", confirm motifs tighten.
- **Visual gate:** `just test browser` (mandatory before claiming the visual
  change works, per AGENTS.md). Not runnable in this sandbox — needs the browser
  harness.

---

## 7. Phase E — GPU port (follow-up, scale)

The CPU engine is `O(n²)` on exclusion. Port mirrors `fa2-bh`:

- Reuse `graph-layouts/.../octree.wgsl` for the exclusion/affinity far-field
  (θ-criterion), exactly as `fa2-bh` accelerates `fa2-brute`.
- WGSL kernels for edge springs (per unique edge), angle constraints (per node,
  capped neighbour pairs), and integration — attributes (class/coordination/mass)
  upload as storage buffers; the affinity matrix + angle/radius tables as a small
  uniform/storage buffer.
- Honour `EngineCtx.gpu`; keep the CPU path as the headless fallback.
- `reinit()` override to reuse buffers across multilevel levels (the hook already
  exists on the trait).

---

## 8. Phase F — distributed (follow-up, scale-out)

Attributes shard with the partition:

- Each worker receives its partition's slice of `node_class/coordination/mass`
  (owned + ghost) plus the global affinity/angle/radius tables (tiny, broadcast).
- `edge_len` follows the partition's local CSR neighbor order.
- Exclusion far-field across partitions reuses the Barnes-Hut COM exchange noted
  in `compute-architecture.md` §4 — attributes don't change that pattern, they
  ride the existing halo.
- SGD-style independence doesn't apply; geometric forces are local + far-field,
  so shardability ≈ Barnes-Hut FA2 (★★).

---

## 9. Risks / open questions

- **Categorical cardinality.** Tag/type/field classes can explode (hundreds of
  values). The class table + affinity matrix grow `O(k²)`. Mitigation: cap
  classes (top-k by frequency + an "other" bucket); document the cap; surface it
  in the UI.
- **Attribute payload size.** Injected vectors are `~16·n_nodes` bytes per
  subscribe. Fine for ≤1M nodes; if it becomes hot, cache per (graph, lens) on
  the broker and re-send only on lens change.
- **Lens ↔ knob coupling.** The affinity matrix dimension must match the resolved
  class count. The resolver owns this invariant (sizes the matrix to match);
  the UI's advanced editor must clamp to the current class count.
- **Angle term stability for hubs.** `max_angle_pairs` caps cost but a degree-1000
  hub still has frustrated geometry. Acceptable (it *should* look frustrated);
  watch for integrator blow-up — `max_step` guards it.

---

## 10. Sequencing recommendation

1. **A + B in parallel** (wire + UI shell) — both verifiable here via unit/serde
   tests.
2. **C** (producer) — verifiable here via resolver unit tests.
3. **D** (integrate + browser visual) — needs the browser harness; do on a
   machine that can run `just test browser`.
4. **E / F** as separate scale follow-ups, each its own PR.

Track the remaining phases (E / F) in this doc; tick them off as they land.

---

## 11. Validation / regression / performance harness

> Landed alongside the backend solver. Lives in
> `crates/graph-compute/tests/geometric_solver.rs`, built on one engine
> observable.

A geometric solver, like a molecular-dynamics / FEP engine, is only trustworthy
if you can answer three questions on every change: *is it still correct, did its
behaviour drift, and is it still fast?* The borrowed idea from FEP validation is
to anchor on **solved cases** — there, experimentally-measured binding free
energies; here, **analytically-known equilibrium geometries** — and a scalar
that says how relaxed a structure is.

### The observable: `GeometricEngine::observe()`

`step()` returns only positions, which can't tell you whether a layout has
*converged*. So the engine gained a non-destructive

```rust
pub fn observe(&self) -> Option<GeometricObservables>
//  → { potential energy (decomposed: edge / angle / exclusion / gravity),
//      kinetic energy, max & RMS residual force ‖∇E‖ }
```

The residual force is computed through the **same** `compute_forces` the
integrator uses (extracted so the two can't drift), and each energy term is the
integral of its force law — so `-∇(potential) == force` for the conservative
terms. At a solved (equilibrium) layout the residual → 0 and the potential sits
at a local minimum. (Class *affinity* — a constant-magnitude, cutoff force — is
not a clean potential and is omitted from the energy scalar; it is inactive at
the default `affinity_strength = 0` and still shows up in the residual.)

### Three layers, all on that observable

| Layer | What it asserts | Why it's robust |
|---|---|---|
| **Canary** (solved cases) | A **library of known problems**, each relaxing to a closed-form equilibrium: single spring → rest length; **spring + gravity → `d* = 2kL/(2k+gm)`** (the balance is verified quantitatively, centroid at origin); three equal springs → equilateral triangle; **4-cycle + 90° angle → square** (sides `L`, diagonals `L√2`); **K4 equal springs → regular tetrahedron** (all 6 distances `L`, 3D). Plus a damped run that sheds total energy monotonically. | The equilibria are *exact analytical* answers, so the tolerances are physics, not curve-fitting — and the set is chosen to pin down **every force term** (edge, gravity, angle) in 2D *and* 3D, so a regression in any one term fails a specific named case. All converge fast (16–23 steps). The angle term, untested by springs-only cases (a 4-cycle is a floppy rhombus without it), is what the square case exists to catch. |
| **Regression** (golden master) | A 5×5 grid under the full default force set, fixed seed, fixed 600 steps → robust scalars (potential, residual, radius of gyration) match a committed golden within a relative+absolute tolerance. | Captures the *whole* engine (springs + degree-angle + exclusion + gravity) as one fingerprint. Deterministic (no RNG; a SplitMix64 hash seeds positions). Regenerate intentionally with `UPDATE_GEOMETRIC_GOLDEN=1`; a first run with no golden writes one. |
| **Performance** | Throughput (steps/sec) on a 12×12 grid asserted above a *generous* floor (50 steps/sec vs. ~7k observed), plus a **steps-to-converge** budget on the triangle canary (< 4000 vs. 16 observed). | A wall-clock floor alone misses an algorithm that still converges but in 10× the iterations; the iteration budget catches that. The floor is loose enough not to be timing-flaky on CI yet trips on a complexity regression (e.g. the O(n²) exclusion pass blowing up). |

### CPU↔GPU equivalence gate

Because each solved case's `check` asserts closed-form *geometry* (distances),
which is invariant to the rigid rotation/translation a different backend settles
into, the *same* library is the CPU↔GPU equivalence gate:
`canary_gpu_solves_known_problems` runs each problem on the `geometric-gpu`
engine and asserts it relaxes to the same analytical answer. It skips cleanly
(loudly) when no wgpu adapter is present; on Metal (Apple M3 Pro) **all five**
problems pass, so the GPU edge-spring, gravity, octree-exclusion, **and angle
(coordination)** kernels are all validated against closed-form answers.

**Angle term on GPU.** `shaders/geometric_barnes_hut.wgsl` now ports the angle
constraint (previously CPU-only). With no CSR adjacency on the device, each
thread gathers its neighbours from the existing `edges` buffer and sums the net
angle force on its own node across the two roles it plays — *center* (reaction
`−(F_j+F_k)` for each neighbour pair) and *endpoint* (`F_j` for the triple
`(c; i, k)` at each neighbouring center `c`) — which is exactly the gradient of
the same `Σ ½·k·(θ−ideal)²` energy the CPU minimises. So the **square** is now a
GPU case and serves as the GPU canary for the angle gradient: if it regresses,
`square-90deg` fails on GPU while the spring/gravity cases still pass.

Cost is `O(deg²·E)` per node via the edge scans — fine for the validation graphs;
**uploading CSR adjacency** (offsets+neighbours) to replace the edge scan is the
perf follow-up (the device's 12-storage-buffer limit is the constraint to work
around). The GPU engine also still resolves `node_coordination` only from
injected attributes (defaulting to bucket 0); wiring structural coordination
(degree) on the GPU is a separate follow-up — it doesn't affect the square
(uniform coordination) but would for a degree-driven angle layout.

### Running it

```sh
just test geometric          # canary + GPU gate + regression + perf (prints numbers)
just test geometric-golden   # regenerate the regression golden (intentional baseline bump)
# GPU cases need a wgpu adapter; run on a GPU host (e.g. sandbox-off on macOS/Metal).
```

### Follow-ups this enables

- When the **GPU port** (Phase E) lands, point the *same* solved-case library +
  golden at the GPU engine: every known problem must still relax to its
  closed-form answer, and the residual-force tolerance is exactly the
  cross-backend equivalence check (CPU vs. GPU must agree to within solver
  tolerance), mirroring how `fa2-bh` was validated against `fa2-brute`.
- New engine variants / force terms slot in as new `SolvedCase` rows — a
  constrained motif with a known target geometry is the cheapest possible proof
  that a term does what it claims.
- A per-engine convergence read-out (`observe()`) is also what a UI "settled /
  still relaxing" indicator would consume — no new backend work needed.
