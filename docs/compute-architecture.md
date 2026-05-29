# graph-compute architecture & layout-engine refactor

How the standalone layout solver (`crates/graph-compute`) is wired today, and the
refactor that turns its single hardcoded ForceAtlas2 sim into a **registry of
selectable layout engines** that can eventually **shard across workers**.

Companion to [`layout-algorithms.md`](layout-algorithms.md), which surveys *which*
algorithms to add and why. This doc is about *where they plug in*.

---

## 1. Current state

```
                          graph-compute  (gRPC :50051, one process, one GPU)
 ┌──────────────────────────────────────────────────────────────────────────┐
 │  main.rs ── load CsrGraph (file or path-graph) ── SimState::new           │
 │                                                                            │
 │  SimState                                                                  │
 │    ├ graph: CsrGraph              (offsets[], neighbors[])                 │
 │    ├ positions: RwLock<Vec<f32>>  (interleaved x,y,z)                      │
 │    ├ tx: broadcast<PositionDelta> (32-frame ring)                         │
 │    └ wgpu_sim: Option<WgpuSim>    (lazy; None ⇒ cpu_step fallback)        │
 │                                                                            │
 │  run_sim_loop ── tick @ N Hz ──►  WgpuSim::step()  OR  cpu_step()         │
 │                                         │                                  │
 │                                   ┌─────┴─────┐                            │
 │                                   │ force_atlas2.wgsl                      │
 │                                   │  • O(n²) repulsion  ◄── brute force    │
 │                                   │  • O(m) attraction (linear edge scan)  │
 │                                   │  • gravity + Euler                     │
 │                                   └───────────┘                            │
 │                                                                            │
 │  gRPC service.rs                                                           │
 │    • Subscribe(graph_id) → stream PositionDelta   (sim loop is producer)   │
 │    • Health()                                                              │
 │    • TopoFisheye(stream FocusRequest) → stream HybridFrame  ◄── the one    │
 │         (multilevel coarsen + hybrid + radial distort)        advanced path│
 └──────────────────────────────────────────────────────────────────────────┘
                                   ▲   │ PositionDelta (raw LE f32)
                       Subscribe   │   ▼
 ┌──────────────────────────────────────────────────────────────────────────┐
 │ graph-api  compute_broker.rs ── redialing forwarder ── broadcast ── WS ──► │
 │ renderer (wgpu/egui, WASM or native)                                       │
 └──────────────────────────────────────────────────────────────────────────┘
```

### What's wrong with it (the "feels like a prototype" symptoms)

1. **One hardcoded algorithm.** `WgpuSim` *is* ForceAtlas2. There's no way to
   pick a different solver, and no abstraction to hang one on. Meanwhile
   `crates/graph-layouts` already has a clean `StaticLayout`/`PhysicsLayout` +
   object-safe `DynStaticLayout`/`DynPhysicsLayout` trait surface with a
   descriptor/registry — graph-compute just doesn't use it.
2. **O(n²) repulsion.** Caps useful graph size at ~10–50k nodes. The octree that
   would fix it already exists in `graph-layouts/.../octree.wgsl`.
3. **No algorithm/params on the wire.** `compute.proto`'s `SubscribeRequest` is
   just `{ graph_id }`. The client can't choose an algorithm or tune it.
4. **"Horizontal scaling" is aspirational.** `lib.rs` advertises "Phase 2: NCCL
   halo exchange, multi-GPU partitioning" — none of it exists. One process, one
   GPU, the whole graph in one buffer.

---

## 2. Target: a layout-engine registry

Lift the algorithm behind a trait so `graph-compute` holds **a map of engines**
and the sim loop drives whichever one a `Subscribe` selected. Reuse — do not
fork — the trait vocabulary from `graph-layouts::layout::layout_trait`.

```
        ┌──────────────── scale-out-first server engine trait ────────────────┐
        │  trait LayoutEngine: Send + Sync {                                   │
        │     fn descriptor(&self) -> &LayoutDescriptor;  // shared vocabulary │
        │     fn set_params(&mut self, &Value) -> Result<(), String>;          │
        │                                                                      │
        │     // shard = None  ⇒ single worker owns the whole graph            │
        │     // shard = Some  ⇒ owns a CSR partition + a ghost-node table     │
        │     fn init(&mut self, ctx: &mut EngineCtx,                          │
        │             graph: &CsrShard, positions: &[f32]) -> Result<…>;       │
        │                                                                      │
        │     // advance one tick; returns OWNED nodes' positions for          │
        │     // broadcast (and, when sharded, the boundary slice to ship)     │
        │     fn step(&mut self, ctx: &mut EngineCtx) -> StepOutput;           │
        │                                                                      │
        │     // distributed hook — no-op single-worker; apply peer ghosts     │
        │     fn apply_halo(&mut self, _halo: &HaloUpdate) {}                  │
        │     fn is_halted(&self) -> bool { false }                            │
        │  }                                                                   │
        │  // EngineCtx carries the optional wgpu Device/Queue so CPU and GPU  │
        │  // engines share one trait. CsrShard = CsrGraph + partition meta.   │
        └─────────────────────────────────────────────────────────────────────┘
                      ▲              ▲              ▲              ▲
        ┌─────────────┘   ┌──────────┘   ┌─────────┘    ┌─────────┘
   ┌────┴─────┐    ┌──────┴──────┐  ┌─────┴──────┐  ┌────┴───────────┐
   │ Fa2Brute │    │ Fa2BarnesHut│  │ SgdStress  │  │ MaxentStress / │
   │ (current)│    │ (octree)    │  │ (s_gd2)    │  │ Multilevel<E>  │  ...
   └──────────┘    └─────────────┘  └────────────┘  └────────────────┘

   registry: HashMap<LayoutId, Box<dyn LayoutEngine>>   (built once at startup)
```

`Multilevel<E>` is a **wrapper engine** (decorator): it coarsens, runs an inner
`E` at each level, and prolongs — so multilevel composes with *any* inner solver
instead of being copy-pasted per algorithm. This is the DRY payoff: N solvers ×
{flat, multilevel} without N×2 implementations. See
[`layout-algorithms.md` §2](layout-algorithms.md#2--multilevel--multiscale-a-wrapper-not-a-standalone).

### Decision: dedicated server-side trait, shared vocabulary (resolved)

> **ADR-001.** A `graph-compute`-owned `LayoutEngine` trait, designed scale-out
> first, that **reuses the shared vocabulary** (`LayoutDescriptor`, `LayoutId`,
> JSON-params convention) from `graph-layouts::layout::layout_trait`. Chosen for
> long-term horizontal-scaling fit. Supersedes the earlier open A/B fork.

The rejected alternative was extending `graph-layouts`' browser `PhysicsLayout`
trait directly. The two have **different execution models**, and that difference
is exactly where scaling-out lives:

| | browser `PhysicsLayout` | server `LayoutEngine` |
|---|---|---|
| Process model | in-process, single | standalone worker, fan-out to many clients |
| Graph extent | whole graph in one buffer | a **shard** (CSR partition + ghost nodes) |
| Step output | mutates a shared `wgpu::Buffer` the renderer reads | **host-readable** positions for broadcast/halo |
| Peers | none | exchanges **boundary positions** each superstep |

Extending the browser trait (the rejected option) would bolt partition-awareness,
halo exchange, and host-readback onto the renderer's crate permanently. Instead:

- **Execution stays separate.** The server trait is designed for the distributed
  model from day one — `shard = None` is the single-worker case; the distributed
  case adds no new trait, just a non-empty shard + the `apply_halo` hook.
- **Vocabulary is shared.** `LayoutDescriptor` / `LayoutId` / the serde-settings
  convention (`settings_json() -> serde_json::Value`) come from `graph-layouts`,
  which `graph-compute` **already depends on** (the topo-fisheye hierarchy is
  re-exported from it) — so this is *zero new coupling*, and the renderer's layout
  picker + `AppState`/`LayoutState::active` persistence stay
  engine-location-agnostic across in-process and remote engines. On the wire that
  same `Value` rides as `google.protobuf.Struct` — see §3 / ADR-002.

If the shared types ever need to be consumed without pulling the full
`graph-layouts` crate (wgpu + every browser algorithm), extract just the
vocabulary into a small `layout-core` crate. Not now — YAGNI; the dependency
already exists.

---

## 3. Wire-format evolution

Algorithm selection + tuning has to reach the worker. Minimal, backward-friendly
proto changes (additive — old clients keep working because proto3 fields default):

```protobuf
import "google/protobuf/struct.proto";

message SubscribeRequest {
  string graph_id              = 1;
  string layout_id             = 2;  // NEW: registry key, e.g. "fa2-bh",
                                     //      "sgd-stress". empty ⇒ server default.
  google.protobuf.Struct params = 3; // NEW: dynamic engine settings. empty ⇒
                                     //      engine defaults.
}
```

### Why `Struct`, not JSON-in-`bytes` and not a typed `oneof` (ADR-002)

> **ADR-002.** Engine params travel as `google.protobuf.Struct` — protobuf's
> native dynamic type. The contract is the Rust settings struct; the wire just
> transports it. Supersedes an earlier "JSON-encoded `bytes`" sketch.

The earlier sketch embedded a JSON *string* in a `bytes` field — a second
serialization format smuggled through an opaque blob. It loses protobuf's
introspection and reads as "a blob you have to know is JSON." Three options were
weighed:

| Option | Contract in | DRY | Wire introspectable | Per-engine `.proto` edit |
|---|---|---|---|---|
| JSON in `bytes` (rejected) | Rust struct | ✅ one serde struct | ❌ opaque | none |
| **`google.protobuf.Struct`** (chosen) | Rust struct | ✅ one serde struct | ✅ real protobuf | none |
| Typed `oneof` per engine | `.proto` schema | ❌ struct *and* proto msg + mapping | ✅ | one per engine |

Decisive context: the repo is **Rust on both ends** (hard rule — client renderer
and server worker are both Rust), and the renderer already represents settings as
`serde_json::Value` via `DynPhysicsLayout::settings_json()`.

- A typed `oneof` would re-declare, in the `.proto`, types that already exist as
  serde settings structs in `graph-layouts` — the duplication the DRY rule pushes
  against, and it would couple the browser crate's settings to the backend wire
  schema (backwards). Its main payoff — a schema-enforced contract — chiefly
  benefits *cross-language* clients, which the Rust-only rule forbids.
- `google.protobuf.Struct` is "JSON-shaped data as real protobuf fields," not
  JSON-text-in-a-blob. `serde_json::Value ↔ prost_types::Struct` is a direct
  mapping, so the renderer's existing `settings_json()` flows straight through,
  one Rust settings struct stays the single source of truth, the registry keeps
  its zero-wire-change extensibility, and the wire stays introspectable (dump a
  request, read the params). Validation happens on the server when the `Struct`
  is deserialized into the engine's typed settings — a malformed request is
  rejected at the boundary.

(If a non-Rust consumer of these params ever appears — which the repo rule
currently forbids — revisit and switch to a typed `oneof`.)

- **Bulk numeric stays raw.** Positions/edges/metrics remain raw LE `f32`/`u32`
  per the repo wire-format rule — only the *control* message gains structure.
- `PositionDelta` is unchanged. A future `HaloDelta` (below) is the only new
  streaming message the distributed model needs.

---

## 4. Distributed / horizontal-scaling model

> Forward-looking design. See the **least-verified** caveat in
> [`layout-algorithms.md` §5](layout-algorithms.md#5--distributed--horizontal-scaling).
> Land the registry (§2) and Barnes-Hut first; this builds on them.

The standard pattern: **partition CSR → ghost boundary nodes → BSP supersteps
exchanging only boundary positions.**

```
                         ┌──────────── coordinator (graph-api or a lead worker) ───────────┐
                         │  partition CSR into P blocks (edge-cut, METIS-style)             │
                         │  assign block p → worker p; compute ghost lists                  │
                         └───────────────┬───────────────┬───────────────┬─────────────────┘
                                         │ block 0       │ block 1       │ block 2
                                         ▼               ▼               ▼
                                ┌────────────────┐ ┌────────────────┐ ┌────────────────┐
                                │ worker 0       │ │ worker 1       │ │ worker 2       │
                                │ owns V0 + ghost│ │ owns V1 + ghost│ │ owns V2 + ghost│
                                │ copies of V1,V2│ │ copies of V0,V2│ │ copies of V0,V1│
                                └───────┬────────┘ └───────┬────────┘ └───────┬────────┘
                                        │                  │                  │
   superstep loop (BSP):                ▼                  ▼                  ▼
     a. local forces (own + ghost contributions)                                   │
     b. integrate own positions                                                    │
     c. ────────── exchange boundary positions (HaloDelta) ──────────►◄────────────┘
     d. barrier; repeat
```

**Key facts that make this tractable here:**

- **Communication = boundary positions only.** `PositionDelta`'s shape (frame +
  raw LE f32 + node count) is already exactly a halo message; a distributed build
  needs a `HaloDelta { frame, owner_id, node_ids[], positions[] }` — a small
  generalization, not a new transport.
- **Far-field forces need coarse remote COMs.** Barnes-Hut/FMM workers also hold
  a low-resolution copy of other partitions' centers-of-mass (multi-GPU FMM does
  this). Cheap relative to full position exchange.
- **Algorithm shardability** (from the survey): **SGD stress ≥ Barnes-Hut FA2 >
  FMM > DR-embedding.** SGD's independent pairs shard best; eigendecomposition
  layouts (PivotMDS/MDS) resist partitioning and should stay single-worker (use
  them as *seeds*, not distributed solvers).
- **CSR partitions** drop out of the existing `/graph/csr.bin` exporter +
  `CsrGraph::load_bin` — partition the offsets/neighbors arrays per block and
  append the ghost-node id table.

### Phasing

| Phase | Deliverable | Depends on |
|---|---|---|
| 0 (now) | This refactor plan + docs | — |
| 1 | Engine registry + trait (§2); port current FA2 into it | — |
| 2 | Barnes-Hut FA2 engine (reuse octree) | Phase 1 |
| 3 | `layout_id` + `params` on the wire (§3); renderer engine picker | Phase 1 |
| 4 | SGD-stress engine (pivot/sparse) | Phase 1 |
| 5 | `Multilevel<E>` wrapper (reuse `coarsen.rs`) | Phases 2/4 |
| 6 | Partition + `HaloDelta` + BSP supersteps (§4) | Phases 2/4 |

---

## 5. DRY checklist (carry into implementation)

- One **coarsening** implementation (`coarsen.rs` / topo-fisheye hierarchy) — used
  by topo-fisheye *and* the multilevel wrapper. Don't fork it.
- One **octree** (`octree.wgsl`) — used by Barnes-Hut FA2 *and* maxent repulsion.
- One **descriptor/params** convention (`LayoutDescriptor` + JSON settings) across
  in-process (`graph-layouts`) and remote (`graph-compute`) engines, so the
  renderer's layout UI and `AppState` persistence are engine-location-agnostic.
- One **wire transport** (`PositionDelta` → generalized `HaloDelta`); bulk numeric
  stays raw LE, control messages stay protobuf.
- Multilevel as a **wrapper**, not per-algorithm copies: N solvers, not N×2.
