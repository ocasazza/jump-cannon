# tvix graph generation + pipeline plugin architecture — design

Two linked deliverables, presented as **research + design**. No features are
implemented yet; this is the plan the orchestrator reviews before any code lands.

Companion to [`compute-architecture.md`](compute-architecture.md) (where engines
plug in), [`layout-algorithms.md`](layout-algorithms.md) (the algorithm survey),
and the existing plan docs ([`geometric-engine-plan.md`](geometric-engine-plan.md),
[`self-assembly-plan.md`](self-assembly-plan.md)).

The two deliverables:

1. **A UI panel** that lets the user *generate* graph datasets / initial test
   graphs **in the browser** by writing [tvix](https://tvix.dev) (Nix)
   expressions that evaluate to graph data — for verifying/seeding specific
   datasets (e.g. authoring the self-assembly morphology canaries as Nix exprs).
2. **A forward-looking plugin / extension architecture** so users can inject
   behaviour at points in the compute/render/layout pipeline — in the browser via
   tvix now, and (future) server-side as sandboxed Rust/WASM executed as
   callback-hook extensions at pipeline stages.

---

## 0. Executive summary

Both deliverables are feasible and mostly **greenfield wiring on top of proven
seams**.

- The tvix panel is provable today. The reference implementation
  `/tmp/bird-nix/playground` (olivecasazza/bird-nix) shows `tvix-eval` compiling
  to `wasm32` under the *same* stack jump-cannon uses (trunk + wasm-bindgen,
  `default-features = false`, a custom in-memory `EvalIO` VFS, `.enable_import()`).
- jump-cannon already owns `crates/tvix-wasm`, but it is the **wrong shape**:
  `tvix-eval` is gated native-only (`crates/tvix-wasm/Cargo.toml:22-23`) and the
  `wasm32` path is a stub returning `Err("tvix-eval: native only")`
  (`crates/tvix-wasm/src/lib.rs:25-28`) behind a stale, false comment claiming
  tvix "cannot compile" on wasm. The plan **rewires that crate** to the bird-nix
  pattern rather than re-porting tvix from scratch.
- The plugin layer is a **stage-keyed hook registry**, not an ECS. The user's
  "ECS interface built into the graph layout, like a game engine" intuition is
  *half* right: they want the **systems** half (ordered hooks at labeled stages,
  the Bevy `Plugin` model), not the entity-component-storage half. The data is
  one homogeneous node type already stored struct-of-arrays.
- The smallest first increment is **browser-only**: rewire `tvix-wasm`, evaluate
  an expression to a live graph, render it. Everything plugin-related is
  forward-looking and explicitly deferred.

---

## 1. What exists today

### 1.1 `crates/tvix-wasm` — present but mis-shaped

The crate is the right *home* but the wrong *implementation*:

- `Cargo.toml:22-23` — `tvix-eval` is under
  `[target.'cfg(not(target_arch = "wasm32"))'.dependencies]`, i.e. native-only,
  pinned at rev `1dbdd85`.
- `src/lib.rs:25-28` — the wasm32 path is a stub returning
  `Err("tvix-eval: native only")`, behind a comment asserting tvix cannot compile
  on wasm. **This belief is unverified and the reference proves it false.**

It is also **not wired** into the frontend or `graph-api` — grep finds no
usage. So the panel is greenfield wiring on top of an existing bridge + a proven
reference pattern.

### 1.2 The graph data domain

- `crates/vault-data` — `VaultNode { id, meta { title, tags, frontmatter }, metrics, x, y }`,
  `VaultEdge { source, target, … }`, `VaultGraph`.
- `crates/graph-api` — holds an `ArcSwap<GraphSnapshot>`, serves `/graph/*` (raw
  little-endian `f32`/`u32`) + protobuf init/meta; `attribute_resolver::resolve`
  is a pure per-node `Lens` dispatch table.
- `crates/graph-compute` / `crates/graph-layouts` — the `LayoutEngine` trait +
  `EngineRegistry` (`engines/mod.rs`).
- `app/ui` — the Dioxus/wgpu WASM frontend; graph bootstrap + panel chrome.
  (This plan was originally written against the egui renderer's
  `data::Bootstrap` / `Section` enum / `loaded_into_gpu` latch — egui-era;
  superseded by app/ui — see git history.)

### 1.3 The plugin seams that already exist (verified)

- `LayoutEngine` trait: `set_params(&serde_json::Value)`,
  `step(&mut EngineCtx) -> StepOutput`, `init`/`reinit`/`apply_halo`/`is_halted`
  (`graph-compute/src/engines/mod.rs:350/361/370`).
- `GeometricEngine::observe()` / `observe_assembly()`
  (`engines/geometric.rs:799/864`) — already an observer-style hook, just concrete.
- The sim step call site (`graph-compute/src/sim.rs:198`).
- `EngineConstructor = fn() -> Box<dyn LayoutEngine>` — a **bare fn pointer that
  cannot capture** (`engines/mod.rs:415`; code comments confirm this).
- `MultilevelEngine` (`engines/multilevel.rs`) — proves the decorator pattern
  (wraps an inner engine via reinit/step without touching the registry).
- `attribute_resolver::resolve(lens, snapshot) -> (GeometricSettings, GraphAttributes)`
  — a pure dispatch over `Lens` variants, validated by `GraphAttributes::validate`
  (strict length invariants), shipped raw-LE, run once per graph-change off the
  hot path.

---

## 2. The bird-nix reference pattern

`/tmp/bird-nix/playground` (`src/lib.rs`, ~657 lines) is the whole glue. It uses
the same stack as jump-cannon and demonstrates the proven path:

- Links `tvix-eval` directly (`git tvlfyi/tvix`, `default-features = false` to drop
  `impure`/`arbitrary`/`nix_tests` and the native-fs builtins).
- Implements a custom `EvalIO` — `struct BirdNixIO` = an **in-memory virtual
  filesystem** (`path_exists`/`open`/`file_type`/`read_dir`/`import_path`). The
  trait signature matches `tvix-eval`'s `io.rs:71` exactly.
- `include_str!`-embeds a library of `.nix` files at compile time
  (`src/graph.nix`, `src/graph-combinators.nix`) plus many demo generators
  (`playground/demos/*.nix`: star/cycle/grid/complete/path graphs,
  protein-interaction-network, chemical-reaction-graph, neural-network-layers,
  hpc-cluster-topology, social-network, toroidal-mesh, subdivide-edge, hub-rule,
  reverse-edges, …).
- Injects user code as `/playground/input.nix`, evaluates with
  `Evaluation::builder(Rc::new(io) as Rc<dyn EvalIO>).enable_import().build()`.
- `wasm-bindgen = =0.2.114`, `opt-level = 's'` + `lto` to absorb the embedded
  files.

**Verified tvix facts at jump-cannon's pinned rev `1dbdd85`:**

- `builtins.toJSON` **exists and is pure** (`eval/src/builtins/mod.rs:462`, in
  `mod.rs` not `impure.rs`) — available under `default-features = false`.
- `builtins.deepSeq` is pure (`mod.rs:328`).
- `default = ["impure", "arbitrary", "nix_tests"]` (`Cargo.toml:49`); impure
  builtins (`getEnv`/`exec`/…) are correctly `#[cfg(feature = "impure")]` gated —
  so `default-features = false` makes eval **pure by construction**.
- `mimalloc` is a **dev-dependency only** (benches), not a runtime/wasm blocker.
- The one residual wasm32 risk: `dirs.workspace = true` is an *unconditional*
  (non-impure-gated) dep of `tvix-eval` (`Cargo.toml:15`) — the stub's comment
  blaming `dirs` is literally true that it is always pulled in. But bird-nix
  builds for wasm32 at rev `1becae0` with the same crate, proving `dirs` compiles
  / dead-strips on wasm. **Net: rev `1dbdd85` looks more likely to build for
  wasm32 than the stub implies — keep the Phase-0 check but lean optimistic.**

**Two correction notes about the reference (folded in):**

1. **Serialization.** bird-nix does **not** use `toJSON` — it serializes via Nix
   `Display` (`format!("{}", value)`, `lib.rs:226`) and its demos return plain
   Nix attrsets. `toJSON` does exist and is pure at `1dbdd85`, so the `toJSON`
   plan is viable, but it is a **deliberate improvement over the reference** that
   needs its own test (Nix→JSON type coercions: key ordering, null, functions),
   *not* the proven default. The first increment may follow either path; if in
   doubt, follow bird-nix's `Display` path verbatim and treat `toJSON` as an opt-in.
2. **Schema.** The star demo does **not** emit `toGraphJSON`'s shape.
   `/tmp/bird-nix/playground/demos/star-graph.nix` returns
   `{ nodes = getNodeIds; edges = getEdgeIds; hub_degree = …; }` — an ad-hoc shape.
   `toGraphJSON` (`/tmp/bird-nix/src/graph.nix:143`) emits
   `{ nodes = [{id,type,…}]; links = [{source,target,id,directed,…}]; }`. These
   are **two different schemas.** The first increment must fix on **one** accepted
   schema (recommended: `toGraphJSON`'s `{nodes, links}`) and author the demo to
   match it, rather than reusing the star demo verbatim.
3. **tvix closure-across-3+-imports bug.** Real and documented in bird-nix's own
   test comments (`playground/src/lib.rs` `test_graph_import` /
   `test_graph_combinators_import`). `graph-combinators.nix` takes `{ graph }` as
   an explicit param to dodge it; avoid point-free builtin composition. **Keep
   that workaround verbatim — do not "clean up" the library.**

---

## 3. Deliverable 1 — the Generate (tvix) panel

A new Generate panel (label "Generate (tvix)") in the frontend — today that is
`app/ui/src/panels/generate.rs`. (The file paths and dispatch sites below are
egui-era — `Section::Generate` + `sections/generate.rs` in graph-renderer;
superseded by app/ui — see git history.)

### 3.1 UI (Rust-native widgets only — AGENTS.md forbids JS editors, so no Monaco)

New file `crates/graph-renderer/src/ui/sections/generate.rs` (egui-era; the
app/ui equivalent is `panels/generate.rs`) exposing `pub fn show(ui, state)`:

- `egui::TextEdit::multiline` for the Nix expression (monospace, code font from
  `FontFamilyChoice`).
- An "Evaluate" button.
- A demo `egui::ComboBox` populated from the embedded demo catalog.
- A node/edge count readout.
- An error/diagnostics area rendering tvix errors as red egui labels (drop the
  bird-nix Monaco `{line,col,endCol}` format — flatten to text).

Register the section by adding:

- `Generate` to the `Section` enum (`ui/state.rs:12`)
- `Generate` to `Section::ALL` (`state.rs:23`)
- a title arm (`state.rs:33`)
- `Section::Generate => generate::show(ui, state)` to the `sections/mod.rs:35`
  match, plus `pub mod generate;`
- an icon arm in `sidebar::draw_icon`

The tray launcher (`status_footer.rs` `for &section in Section::ALL`),
`FloatingPanel` chrome, tiled `PaneKind::Section`, focus, and the
`BTreeMap<Section, bool>` persistence all come **for free** — zero new panel
plumbing. Persistence is unknown-key-tolerant, so the new map entry is
forward-compatible. **Do not add new persisted `AppState` fields without bumping
the persist version key** (stale-state deserialize would break).

### 3.2 Evaluation path — rewire `crates/tvix-wasm`

- `Cargo.toml`: replace the `cfg(not(target_arch = "wasm32"))` `tvix-eval` block
  with an **unconditional** dep
  `tvix-eval = { git = tvlfyi/tvix, rev = <known-good>, default-features = false }`.
  `default-features = false` drops impure/arbitrary/nix_tests. Pin `wasm-bindgen`
  to a tvix-compatible version (bird-nix uses `=0.2.114`).
- `src/lib.rs`: delete the wasm32 stub (`25-28`) and the stale comment. Port the
  bird-nix `VfsIO` `EvalIO` impl + an `embed_nix_files!`-style macro +
  `.enable_import()`.
- **Typed entry point** (do not return a `String`): add
  `pub fn eval_graph(expr: &str) -> Result<GeneratedGraph, String>`. Wrap user
  code so lazy thunks are forced and errors surface at eval time:
  `let __r = <USER>; in builtins.deepSeq __r (builtins.toJSON __r)`, then
  `serde_json::from_str` into `GeneratedGraph { nodes: Vec<GenNode>, edges: Vec<GenEdge> }`.
  (`toJSON` is verified pure at `1dbdd85`; the `Display`-path fallback from the
  reference is available if the `toJSON` coercions misbehave — see §2 note 1.)

### 3.3 Embedded `.nix` library (the canary-authoring substrate)

`include_str!`-embed `src/graph.nix` + `src/graph-combinators.nix` from bird-nix
(graphs are id-keyed attrsets with `addNode`/`addEdge`/`fromEdgeList`/`toGraphJSON`).
Keep the `{graph}`-as-param workaround verbatim. Embed the **graph** demos as the
catalog (star/cycle/grid/complete/path, protein-interaction-network,
chemical-reaction-graph, neural-network-layers, hpc-cluster-topology,
social-network, toroidal-mesh, subdivide-edge, hub-rule, reverse-edges). These
**are** the self-assembly morphology canaries authored as Nix. Watch wasm binary
size — embed only the graph demos, not bird-nix's combinator-theory demos; rely on
`opt-level = 's'` + `lto`.

### 3.4 Expr → {nodes, links} → graph wiring (client-side, bypasses graph-api)

`toGraphJSON` emits `{ nodes = [{id,type,…}], links = [{source,target,id,directed,…}] }`.

- **Domain mismatch** (real): bird-nix node is `{id, type, metadata}`; `VaultNode`
  is `{id, meta{title,tags,frontmatter}, metrics, x, y}`. An **explicit mapping
  layer** (in `vault-data` or the bridge) is required — you cannot deserialize
  `toGraphJSON` straight into `VaultGraph`. Map `GenNode{id,type}` → `VaultNode`
  (`id`→`id`; `type`→`meta.tags`/doctype); `GenEdge{source,target}` →
  `VaultEdge{source,target}`.
- Build a `data::Bootstrap`: assign dense indices to node ids (reuse the
  `id→idx` logic in `GraphSnapshot::build` / `app.rs:919`), `edges = Vec<u32>`
  index pairs, `positions = data::spawn_on_unit_sphere(n, 800.0)` (the renderer
  re-seeds positions on a Fibonacci sphere anyway — a generated graph only needs
  node **count** + edge index pairs; the GPU sim takes over), empty/derived
  metrics, synthesize a `proto::Init { n_nodes, n_edges, palette }`.
- On "Evaluate" success, write `LoadState::Ready(bootstrap)` into the
  `SharedLoad` mutex **and** flip `loaded_into_gpu = false` so the existing
  `app.rs:895` path **replaces** the live graph. tvix eval is synchronous and fast
  for small canaries; run inline on the egui thread initially (no async).

### 3.5 Error surfacing

tvix `result.errors` formatted with `{:#}`, joined, shown as red egui labels.
`eval.compile_only` can drive a non-running "Check" button for parse/scope errors.

### 3.6 Security / sandboxing

`default-features = false` makes tvix-eval **pure by construction** — no
fs/network/IFD; `import` resolves only inside the in-memory VFS. The residual risk
is **DoS**: tvix-eval has **no built-in fuel/step limit**, so `deepSeq` on an
infinite-recursion / huge-attrset expr can hang the UI thread.

- **Near-term:** cap output graph size (reject > N nodes/edges before pushing to
  the renderer). Inline-on-egui-thread is acceptable with this cap.
- **Forward-looking:** run eval in a Web Worker with a kill-timeout.
- **Never** enable tvix `impure` / `builtins.exec`.

---

## 4. Deliverable 2 — plugin / extension architecture

**Model:** a **stage-keyed hook registry** (the Bevy `Plugin` / render-graph /
middleware model — ordered systems at labeled stages), built as a sibling to the
existing registries, **not** an ECS `World`.

> **Phases 6-8 (everything in this section beyond the ECS verdict and the
> stage taxonomy framing) are NON-BINDING forward-looking exploration, not a
> committed roadmap. Do not block the panel on any of it.**

### 4.1 ECS verdict (honest assessment)

**ECS is the wrong framing. Recommend a stage-hook registry — the systems half of
the game-engine model, not the entity-component half.**

The user's intuition is *half* right and the right half matters: they want user
code to **run at defined points** in the pipeline. That is exactly the Bevy
`Plugin` / system-scheduler model — `app.add_system(Stage, system)` with
`.before()`/`.after()` ordering. It is **not** the entity-component-storage half
(archetypes, sparse-set storage, archetype queries).

Why ECS specifically is wrong here:

- ECS solves **heterogeneous entities** with varied component sets queried by
  archetype each frame. This pipeline has **one homogeneous entity type**: the
  node. No archetype variety, no runtime component add/remove, no
  entity-relational query need.
- The one genuinely ECS-like thing — struct-of-arrays storage — **already exists
  and is better than ECS would give**: `GraphAttributes` is parallel `f32`/`u32`
  vectors (`node_class`/`node_coordination`/`node_mass`/`edge_len`/`node_director`)
  over a CSR graph (offsets/neighbours), with interleaved `x,y,z` buffers. Bolting
  `bevy_ecs` on adds a `World` + scheduler + `Query` machinery that buys nothing
  the existing `EngineRegistry` + stage traits do not already provide.
- It would **fight the scale-out design**: `CsrShard`/`ShardMeta`/`apply_halo`
  assume contiguous owned-node ranges + BSP supersteps; a global ECS `World`
  contradicts shard-local ownership + halo exchange.
- **Precedent:** even Bevy keeps its **render graph separate from ECS** because
  GPU command-encoding parallelism does not map to plain systems — directly
  analogous to jump-cannon's GPU layout engines.

Confidence: **medium-high**. The repo's existing traits already fit this shape;
the only real cost is the `EngineConstructor` fn-pointer → boxed-closure change.

### 4.2 Pipeline stages (each backed by a real existing seam)

1. **Ingest** — produces a `vault_data::Graph` → `GraphSnapshot` in graph-api's
   `ArcSwap`, on the same path as a vault reload (`watcher.rs` 400ms debounce).
   Hook shape: `GraphSource`. **The tvix generator panel attaches here —
   deliverable 1 is the first Ingest hook.**
2. **Attribute-resolve** — `attribute_resolver::resolve` is already a pure
   per-node `Lens` dispatch table (Uniform/Degree/Louvain/Tag/NodeType/PageRank/…).
   A tvix expression is a new `Lens::Tvix(expr)` variant (plus Mass/Coordination/
   EdgeLength variants), output = existing `GraphAttributes` vectors, validated by
   `GraphAttributes::validate`, shipped raw-LE. Cleanest plugin point: pure
   dispatch table, already produces the wire payload, runs once per graph-change.
   **Caveat:** `attribute_resolver` lives in **graph-api (native/server-side)** and
   reads `node.metrics` computed natively — it is **not** in-browser unless eval is
   duplicated client-side. (The earlier "strongest in-browser compute hook"
   framing overstated the browser story; corrected here.)
3. **Engine-select** — `EngineRegistry { constructors: HashMap<LayoutId, EngineConstructor> }`
   (`engines/mod.rs:483`), mirrored renderer-side by `LayoutRegistry`/`LayoutFactory`.
   **Structural blocker:** `EngineConstructor` is a bare `fn() -> Box<dyn LayoutEngine>`
   (`mod.rs:415`, no captures) so a tvix/WASM engine that must capture an expr
   cannot be a plain fn. Needs a small trait change:
   `enum EngineCtor { Fn(fn() -> Box<dyn LayoutEngine>), Boxed(Arc<dyn Fn() -> Box<dyn LayoutEngine>>) }`.
   This is the **one genuine (small) abstraction change.**
4. **Pre-step / post-step (force injection)** — `LayoutEngine::step(&mut EngineCtx)`
   with `set_params(serde_json::Value)` as the typed-config seam. `MultilevelEngine`
   proves the decorator pattern. A per-step force plugin wraps a `LayoutEngine`:
   `step()` calls `inner.step()` then applies an extra force field from
   `set_params`. **Per-frame interpreted tvix here is the main perf risk — keep
   Rust-decorator-only near-term.**
5. **Observe (post-step, non-destructive)** — `GeometricEngine::observe()` /
   `observe_assembly()` is already an observer hook, just concrete. Generalize to
   an `Observer` trait `fn observe(&EngineState, frame) -> serde_json::Value`
   invoked by `sim.rs` after `step()`. The self-assembly canaries + the renderer
   Metrics HUD (`app.rs` `last_observed_max_ke`) are the first observers.
6. **Frame (render)** — `egui_wgpu` `CallbackTrait` prepare/paint in
   `graph_callback.rs` + `apply_layout_to_gpu` in `app.rs`; `GraphPipelines`
   exposes `update_colors`/`update_sizes`/`update_shape_ids` setters. **Weakest
   candidate for untrusted extension** (raw wgpu). Keep Rust-only/internal; if
   exposed, only safe setter-style hooks.

### 4.3 Trait(s) / registry

- A `trait PipelineHook` family (one per stage, typed context + serde-roundtrippable
  params), registered in a `HookRegistry` mapping `Stage → ordered Vec<Box<dyn Hook>>`
  with `.before()`/`.after()` labels. Split client-side (in the frontend,
  app/ui, beside `LayoutRegistry`: Ingest/AttributeResolve/Frame) and server-side (in
  `graph-compute`, beside `EngineRegistry`: EngineSelect/PreStep/PostStep/Observe),
  sharing a descriptor/id vocabulary the way ADR-001 made engines
  location-agnostic.
- `enum HookSource { Tvix(expr), NativeRust(fn), Wasm(component) }` so the
  in-browser tvix path and the future sandboxed path register into the **same**
  stage-keyed map.

### 4.4 How tvix registers behaviour now / how server-side fits later

- **Now:** tvix is a `HookSource::Tvix` at the **Ingest** stage (the panel) and the
  **AttributeResolve** stage (`Lens::Tvix`) — both pure, both off the hot path,
  both producing existing payloads, both running through `tvix-wasm`.
- **Later:** `HookSource::Wasm` = a wasmtime host running untrusted
  user-Rust-compiled-to-WASM **components** against WIT-defined hook interfaces
  with capability-based WASI (deny-by-default; grant only the graph buffers) and
  per-call fuel/memory/epoch limits. It plugs in as just another
  `Box<dyn LayoutEngine>`/`Box<dyn Hook>` behind the registry. ("Sandboxed Rust"
  realistically means compile-to-WASM + wasmtime, not native dylibs.) This is a
  **multi-month subsystem and should be a separate future RFC, not a phase here.**

### 4.5 Scale-out respect

`CsrShard`/`ShardMeta`/`apply_halo` assume contiguous owned-node ranges + BSP
supersteps. Compute-stage hook context **must be shard-scoped** (owned nodes only,
respect halo exchange). A global-state ECS `World` would break this — another
reason to reject ECS. The wire stays raw-LE/protobuf throughout because hooks
resolve to existing `GraphAttributes` / position buffers; nothing new crosses the
wire.

---

## 5. Phased plan

Dependency-ordered; each phase independently verifiable. **Phases 1-4 are
near-term (the panel, browser-only). Phases 5+ are forward-looking (the plugin
system) and explicitly non-binding.**

### Phase 0 — de-risk (BLOCKING, browser-only)

Empirically verify `tvix-eval` builds for wasm32 in this workspace:
`cargo build -p tvix-wasm --target wasm32-unknown-unknown` with
`default-features = false` at rev `1dbdd85`. If it fails, pin bird-nix's rev
`1becae0` + `wasm-bindgen = =0.2.114`.
**Verify:** builds clean for wasm32. This gates all of deliverable 1. (Prior leans
optimistic per §2.)

### Phase 1 — rewire `tvix-wasm` (browser-only)

Unconditional `default-features = false` dep; delete the wasm32 stub + stale
comment (`src/lib.rs:25-28`); port bird-nix `VfsIO` `EvalIO` + embed macro +
`.enable_import()`. Add `eval_graph(expr) -> Result<GeneratedGraph, String>` via
`deepSeq` + `toJSON` (or the `Display` fallback). Embed `graph.nix` +
`graph-combinators.nix` + the graph demos.
**Verify:** a unit test `eval_graph("…starGen…")` returns the right node/edge
counts, native **and** wasm.

### Phase 2 — Nix → Bootstrap adapter (browser-only)

`GenNode`/`GenEdge` → `VaultGraph` → `data::Bootstrap` (dense indices, edge
`Vec<u32>`, `spawn_on_unit_sphere` positions, synth `proto::Init`).
**Verify:** golden test — a known demo expr yields a `Bootstrap` with expected
`n_nodes`/`n_edges`/edge indices.

### Phase 3 — the panel (browser-only)

Add the Generate panel (TextEdit + Evaluate + demo picker + error labels) and
the `tvix-wasm { features = ["wasm"] }` + `vault-data` deps to the frontend.
(Written against egui-era files — `Section::Generate`, `sections/generate.rs`,
`graph-renderer/Cargo.toml`; superseded by app/ui's `panels/generate.rs` — see
git history.)
**Verify:** panel opens from the tray; evaluating a demo prints node/edge counts;
a deliberate syntax error surfaces as a red label.

### Phase 4 — live replace (browser-only; the single load-bearing renderer edit)

Convert `app.rs` `loaded_into_gpu` from a permanent one-shot into a re-loadable
trigger (consume a `Bootstrap` whenever `LoadState::Ready`, reset the latch on each
new `Ready`). **Correction:** `GraphPipelines::load` is **already effectively
re-loadable** — it allocates all buffers fresh via `device.create_buffer_init` and
reassigns into `&mut self` on every call (`graph_pipelines.rs:395-510`), so
`load()` itself needs little/no change. The real work is (a) reset the latch on
each new `Ready` and (b) audit the gate sites. **Correction:** the
`!self.loaded_into_gpu` gate-site count is **~17**, not ~9 (app.rs
895/1030/1641/1874/2092/2247/2270/2318/2376/3330/3438/3455/3469/3500 + the
field/init/struct-copy at 56/549/1018/1218) — the audit is ~2× the original scope,
but the easier `load()` makes net effort smaller.
**Verify:** evaluating an expr **replaces** the live graph; regenerating works
repeatedly. **This completes deliverable 1.**

---

*Forward-looking below; do not block the panel on these. Phases 6-8 are
non-binding exploration.*

### Phase 5 — AttributeResolve tvix hook (server-or-native)

Add `Lens::Tvix(expr)` variants to `attribute_resolver.rs`, per-node eval via
`tvix-wasm` (native dep), output validated by `GraphAttributes::validate`. First
real "plugin" beyond ingest. Browser-capable only if eval runs client-side;
otherwise graph-api (native).

### Phase 6 — HookRegistry + EngineConstructor change

`trait PipelineHook` + `HookRegistry` (stage → ordered Vec, `.before()`/`.after()`)
beside `EngineRegistry`/`LayoutRegistry`; change `EngineConstructor` to allow
boxed-closure ctors; lift `GeometricEngine::observe` into an `Observer` trait
called by `sim.rs`; refactor Phases 1-4 and Phase 5 to register as
`HookSource::Tvix`. Needs graph-compute + graph-api.

### Phase 7 — auto-registration (optional)

Adopt `inventory`/`linkme` distributed-slice so engines/hooks self-register
without editing `builtin()`.

### Phase 8 — sandboxed server-side WASM hooks (separate future RFC)

wasmtime host + WIT hook interfaces + `wit-bindgen` + capability WASI +
fuel/memory/epoch limits; `HookSource::Wasm`. Server-only; high cost; explicitly
last.

---

## 6. First increment (recommended)

**A single PR: "Generate (tvix) panel — evaluate a Nix expr to a live graph in the
browser."** Phases 0-4 collapsed to the thinnest end-to-end slice (skip polish;
one hardcoded demo is fine):

1. (Phase 0) Confirm `tvix-eval` builds for wasm32 at the workspace rev; pin
   bird-nix's rev if not.
2. (Phase 1, minimal) In `crates/tvix-wasm`: make `tvix-eval` an unconditional
   `default-features = false` dep, delete the wasm32 stub (`src/lib.rs:25-28`),
   port bird-nix's `VfsIO` `EvalIO` + `.enable_import()`, embed only `src/graph.nix`
   + `src/graph-combinators.nix`. Add `eval_graph(expr) -> Result<GeneratedGraph, String>`
   via `deepSeq` + `toJSON` (or `Display` fallback). Author **one** demo string
   inline that emits the **`toGraphJSON` `{nodes, links}` schema** — do **not**
   reuse `star-graph.nix` verbatim (it emits a different ad-hoc shape).
3. (Phase 2, minimal) `GeneratedGraph` → `data::Bootstrap` adapter.
4. (Phase 3) the Generate panel with a TextEdit prefilled with the demo, an
   Evaluate button, an error label. Add `tvix-wasm` + `vault-data` deps to the
   frontend (egui-era wording; app/ui's `panels/generate.rs` is the live
   implementation — see git history).
5. (Phase 4) Reset `loaded_into_gpu` on a fresh `LoadState::Ready` so the
   generated graph renders (the one load-bearing edit; `load()` is already
   re-loadable).

**End state:** open the panel from the tray, click Evaluate on the prefilled
Nix expr, watch the graph appear and run in the GPU force sim. No graph-api, no new
wire format, no plugin/registry/ECS work — deferred to Phases 5-8. This proves the
entire deliverable-1 path (tvix-in-wasm → attrset → VaultGraph → Bootstrap → live
render) in one verifiable increment.

---

## 7. Risks

- **tvix-eval wasm32 build may fail at the pinned rev** — the whole panel is
  blocked until Phase 0. tvix is explicitly **not** production-stable; expect
  breakage on bumps. *Mitigation:* pin a git rev; prefer bird-nix's proven
  `1becae0`. (Prior leans optimistic — see §2.)
- **`loaded_into_gpu` one-shot latch** has ~17 gate sites protecting
  picking/camera/sim; reworking into a re-loadable trigger risks early-return
  invariants. *Mitigation:* keep the rework minimal (reset on each new `Ready`),
  audit every `!loaded_into_gpu` site. (`load()` itself is already re-loadable, so
  this is smaller than it looks.)
- **Domain-type mismatch** (`{id,type,metadata}` vs `VaultNode {id, meta{…},
  metrics, x, y}`) requires an explicit mapping layer; naive deserialize fails.
  *Mitigation:* a dedicated `GeneratedGraph` → `VaultGraph` adapter.
- **toJSON vs Display** — `toJSON` is verified pure at `1dbdd85` but is *not* the
  proven bird-nix path (it uses `Display`). *Mitigation:* test the `toJSON`
  coercions explicitly, or follow `Display` for the first increment.
- **Schema conflation** — fix on `toGraphJSON`'s `{nodes, links}` and write the
  demo to match; do not reuse the star demo's ad-hoc shape.
- **tvix closure-across-3+-imports bug** — keep the `{graph}`-as-param workaround
  verbatim; do not "clean up" the library.
- **DoS** — no fuel limit means a malicious/buggy expr + `deepSeq` can hang the UI
  thread. *Mitigation:* output-size cap now; Web Worker + kill-timeout later.
- **wasm binary size** — embedding many demos inflates the bundle. *Mitigation:*
  graph demos only; `opt-level = 's'` + `lto`.
- **`EngineConstructor` is a bare fn pointer** (no captures); a tvix/WASM engine
  needs a boxed-closure variant — a small Phase-6 trait change rippling to
  `EngineRegistry::builtin()`. *Mitigation:* add a new ctor enum variant rather
  than change the existing fn type.
- **No server-computed metrics for client-side graphs** — colors/sizes-by-metric
  (pagerank/community/kcore) silently fall back to defaults, which may confuse
  users. *Mitigation:* document; or compute basic metrics client-side; or add a
  future POST path.
- **AppState persistence is versioned** — adding `Section::Generate` to the
  `section_open` `BTreeMap` is forward-compatible, but any **new** persisted field
  requires bumping the persist key. *Mitigation:* avoid new persisted fields in the
  panel PR, or bump the version key.
- **Sandboxed server-side WASM (Phase 8)** carries real operational cost and must
  not block the panel. *Mitigation:* sequence it last; keep browser-tvix and
  server-wasm as separate mechanisms.

---

## 8. Open questions

1. **Feasibility gate:** does `tvix-eval` at `1dbdd85` actually build for wasm32
   with `default-features = false`, or must it pin `1becae0`? Phase 0 must answer
   this empirically — single biggest risk. (Prior: optimistic.)
2. **Client-side only vs round-trip:** should generated graphs stay purely
   client-side (fast canaries, no server-computed metrics) or POST to graph-api for
   the full metric pipeline? No graph-ingest write endpoint exists today. Product
   decision; client-only is far simpler for seeding/canaries.
3. **Accepted attrset schema:** `toGraphJSON`'s `{nodes, links}` (recommended) vs a
   richer shape that authors `VaultNode.meta`/frontmatter/metrics in Nix. Determines
   how much of the domain mismatch the bridge must cover.
4. **`builtins.toJSON` coercions:** key ordering / null / functions under
   `default-features = false` — does the `serde_json::from_str` round-trip behave?
   (Existence/purity already verified; behaviour under coercion is the open part.)
5. **DoS guard:** output-size cap now vs Web Worker + kill-timeout later? Affects
   whether eval can safely run inline on the egui thread.
6. **Phase 6+:** which stages must be plugin-extensible (PreStep/PostStep/
   PostLayout/Frame?) and what is the per-tick marshalling budget for crossing the
   wasmtime boundary with the CSR graph + f32 buffers? Per-frame interpreted tvix in
   the render loop is likely too slow and is the main feasibility risk for the
   literal "hook into the compute pipeline itself" framing.
7. **Auto-registration:** adopt `inventory`/`linkme` now or keep the explicit
   `EngineRegistry::builtin()` list (explicit ordering vs less boilerplate)?
