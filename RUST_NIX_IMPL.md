# RUST_NIX_IMPL.md — jump-cannon Rust Monorepo

> Implementation checklist. Check items off as they complete. Each phase is a coherent milestone that can be built and run independently.

---

## Context

Consolidating three separate repos into a single Cargo workspace rooted in `jump-cannon`:
- `rust-graph-layouts` → `crates/graph-layouts` (layout algorithms, WASM + native)
- `rust-graph-renderer` → `crates/graph-renderer` (Bevy 0.16 force-directed renderer)
- `jump-cannon` (Nuxt/Vue) → `crates/graph-ui` (rewritten in Rust: egui + bevy_egui)
- `obsidian/nix/packages/vault-search/` → `crates/vault-search` (axum + Tantivy, unchanged API)
- NEW: `crates/tvix-wasm` — tvix-eval compiled to WASM; powers `.nix` query expressions
- NEW: `crates/query-lang` — bird-combinator-inspired SPL-like pipeline language

The query language draws on combinator logic (from bird-nix) for its operator semantics: pipe as composition, filter as projection, map as application. A `.nix {}` escape hatch routes to tvix-wasm for full Nix evaluation.

---

## Target Structure

```
jump-cannon/
├── Cargo.toml                    # workspace root
├── RUST_NIX_IMPL.md              # this file
├── flake.nix                     # bird-nix-inspired: per-crate packages + devShell
├── flake.lock
├── archive/nuxt/                 # archived Nuxt source (reference during port)
├── crates/
│   ├── graph-layouts/            # rust-graph-layouts
│   ├── graph-renderer/           # rust-graph-renderer (Bevy 0.16)
│   ├── graph-ui/                 # NEW: egui jump-cannon port
│   │   └── src/
│   │       ├── main.rs
│   │       ├── command_palette.rs
│   │       ├── sidebar.rs
│   │       ├── actions.rs
│   │       ├── query/            # bird-combinator query language
│   │       │   ├── mod.rs
│   │       │   ├── parser.rs
│   │       │   ├── combinators.rs
│   │       │   └── eval.rs       # tvix-wasm bridge
│   │       ├── state/
│   │       │   ├── graph.rs
│   │       │   ├── selection.rs
│   │       │   └── ui.rs
│   │       └── vault.rs          # wikilink extractor (port of vault-links bash)
│   ├── vault-search/             # moved from obsidian repo
│   └── tvix-wasm/                # tvix-eval WASM bridge
└── nix/packages/                 # per-crate nix derivations
```

---

## Build Environment (crane + omnix + flake-parts)

> No existing repos use crane or omnix — both obsidian and nixstation use snowfall-lib. We introduce crane fresh here. snowfall-lib is **not** used (it's NixOS/home-manager focused; crane/flake-parts is the right layer for a Rust workspace).

### flake.nix skeleton

```nix
{
  inputs = {
    nixpkgs.url     = "github:nixos/nixpkgs/nixos-unstable";
    flake-parts.url = "github:hercules-ci/flake-parts";
    systems.url     = "github:nix-systems/default";
    crane.url       = "github:ipetkov/crane";
    crane.inputs.nixpkgs.follows = "nixpkgs";
    rust-overlay.url = "github:oxalica/rust-overlay";
    rust-overlay.inputs.nixpkgs.follows = "nixpkgs";
    omnix.url       = "github:juspay/omnix";
  };

  outputs = inputs: inputs.flake-parts.lib.mkFlake { inherit inputs; } {
    systems = import inputs.systems;

    perSystem = { pkgs, system, self', ... }:
      let
        pkgs = import inputs.nixpkgs {
          inherit system;
          overlays = [ inputs.rust-overlay.overlays.default ];
        };

        # Two craneLib instances: native and wasm32
        craneLib     = inputs.crane.mkLib pkgs;
        craneLibWasm = craneLib.overrideToolchain (
          pkgs.rust-bin.stable.latest.minimal.override {
            targets = [ "wasm32-unknown-unknown" ];
          }
        );

        src = craneLib.cleanCargoSource ./.;

        # Dep caches split by target (native vs wasm32 compile different std)
        depsNative = craneLib.buildDepsOnly { inherit src; };
        depsWasm   = craneLibWasm.buildDepsOnly { inherit src; };

        bevyInputs = with pkgs; [
          libGL vulkan-loader alsa-lib udev
          wayland libxkbcommon xorg.libX11 pkg-config
        ];

      in {
        packages = {
          default     = self'.packages.graph-ui;
          graph-ui    = craneLib.buildPackage {
            inherit src; cargoArtifacts = depsNative;
            cargoExtraArgs = "-p graph-ui";
            buildInputs = bevyInputs;
            nativeBuildInputs = [ pkgs.pkg-config ];
          };
          vault-search = craneLib.buildPackage {
            inherit src; cargoArtifacts = depsNative;
            cargoExtraArgs = "-p vault-search";
          };
          graph-layouts = craneLibWasm.buildPackage {
            inherit src; cargoArtifacts = depsWasm;
            cargoExtraArgs = "-p graph-layouts --target wasm32-unknown-unknown";
            nativeBuildInputs = [ pkgs.wasm-bindgen-cli ];
          };
          tvix-wasm = craneLibWasm.buildPackage {
            inherit src; cargoArtifacts = depsWasm;
            cargoExtraArgs = "-p tvix-wasm --target wasm32-unknown-unknown";
          };
        };

        checks = {
          clippy   = craneLib.cargoClippy {
            inherit src; cargoArtifacts = depsNative;
            cargoClippyExtraArgs = "--all-targets -- -D warnings";
          };
          clippy-wasm = craneLibWasm.cargoClippy {
            inherit src; cargoArtifacts = depsWasm;
            cargoClippyExtraArgs = "--target wasm32-unknown-unknown -- -D warnings";
          };
          test = craneLib.cargoTest { inherit src; cargoArtifacts = depsNative; };
          fmt  = craneLib.cargoFmt  { inherit src; };
        };

        devShells.default = craneLib.devShell {
          packages = with pkgs; [
            # Rust tools
            cargo-nextest cargo-watch cargo-expand rust-analyzer
            # WASM tools
            trunk wasm-pack wasm-bindgen-cli
            # Bevy system deps
          ] ++ bevyInputs;
          # Point linker at Vulkan/GL at runtime
          LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath bevyInputs;
        };
      };

    # omnix CI
    flake.om.ci.default = {
      root.dir = ".";
      root.steps.build.enable  = true;
      root.steps.checks.enable = true;
    };
  };
}
```

### Workspace Cargo.toml

```toml
[workspace]
resolver = "2"
members = [
    "crates/graph-layouts",
    "crates/graph-renderer",
    "crates/graph-ui",
    "crates/vault-search",
    "crates/tvix-wasm",
]

[workspace.dependencies]
bevy       = { version = "0.16", default-features = false }
bevy_egui  = "0.34"
serde      = { version = "1", features = ["derive"] }
serde_json = "1"
tokio      = { version = "1", features = ["full"] }
wasm-bindgen = "0.2"
```

### Per-crate WASM feature gating pattern

```toml
# crates/graph-layouts/Cargo.toml
[features]
default = []
wasm = ["wasm-bindgen", "js-sys", "web-sys", "console_error_panic_hook"]
```

```rust
// src/lib.rs
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
pub fn compute_layout(graph_json: &str) -> String { ... }
```

### Key NixOS/Bevy gotchas
- `LD_LIBRARY_PATH` must include `libGL` + `vulkan-loader` at **runtime** (devShell sets this; nix package doesn't need it since `rpath` is baked in)
- In CI (headless), renderer tests will fail with "no GPU" — gate Bevy integration tests behind `#[cfg(not(ci))]` or use `cargo test --lib` only
- `wasm-bindgen-cli` version must match `wasm-bindgen` crate version exactly — crane pins both via `Cargo.lock`

---

## Phase 1 — Monorepo Scaffold

**Milestone:** `cargo build --workspace` succeeds; all crates compile.

- [x] Archive current Nuxt source: `git mv` everything non-Cargo to `archive/nuxt/`
- [x] Create root `Cargo.toml` as `[workspace]` listing all crate paths
- [x] Copy `rust-graph-layouts/` → `crates/graph-layouts/` (adjust internal paths)
- [x] Copy `rust-graph-renderer/` → `crates/graph-renderer/`
  - [x] Update dep on `graph-layouts` to use workspace path dep (`path = "../graph-layouts"`)
- [x] Copy `obsidian/nix/packages/vault-search/` → `crates/vault-search/`
- [x] Create stub `crates/graph-ui/` (empty `fn main(){}`, `Cargo.toml`)
- [x] Create stub `crates/tvix-wasm/` (empty lib, `Cargo.toml`)
- [x] Write `flake.nix`: crane + flake-parts + omnix + rust-overlay; native + WASM toolchains; per-crate packages; devShell with LD_LIBRARY_PATH for Bevy
- [ ] **Verify:** `cargo check --workspace` passes inside `nix develop` ✓

---

## Phase 2 — tvix-wasm Crate

**Milestone:** Nix expressions evaluable from Rust (`tvix_wasm::eval_nix("1 + 1")` → `"2"`).

- [ ] Pin `tvix-eval` dependency (crate from `github.com/tvlfyi/tvix`; choose a stable commit)
- [ ] Expose `eval_nix(expr: &str) -> Result<String, EvalError>` for native builds
- [ ] Expose `wasm_bindgen`-annotated `eval_nix_wasm(expr: &str) -> Result<String, JsValue>` for WASM builds
- [ ] Gate any C/LLVM deps behind `#[cfg(not(target_arch = "wasm32"))]`
- [ ] Use `tvix-eval` without `nix_builtins` feature for WASM build (avoids native syscall deps)
- [ ] **Verify:** `cargo test -p tvix-wasm` — eval `builtins.toString 42` returns `"42"` ✓

---

## Phase 3 — graph-ui: Bevy + egui Skeleton

**Milestone:** Bevy window opens with egui panels; `Ctrl+P` opens command palette.

- [ ] Add `bevy_egui 0.34` to `graph-ui` `Cargo.toml` (matches Bevy 0.16)
- [ ] Port Bevy `App` builder from `graph-renderer/src/lib.rs::run_app()` into `graph-ui/src/main.rs`
- [ ] Add `EguiPlugin` and systems:
  - [ ] `sidebar_system` — left `egui::SidePanel` with activity bar tabs (Search, Info, Settings, Files)
  - [ ] `command_palette_system` — floating `egui::Window`, triggered by `Ctrl+P`
  - [ ] `status_bar_system` — bottom `egui::TopBottomPanel` (app name, theme toggle hint)
- [ ] Port `stores/ui.ts` → `state/ui.rs` as `UiState` Bevy resource (sidebar open/width/tab, palette open)
- [ ] Wire `Ctrl+P` via Bevy `ButtonInput<KeyCode>` in an Update system
- [ ] Port light/dark theme: `egui::Visuals::dark()` / `light()` toggled from `UiState`
- [ ] **Verify:** `cargo run -p graph-ui` — window opens, sidebar visible, `Ctrl+P` opens palette ✓

---

## Phase 4 — Action System + Command Palette

**Milestone:** Command palette fuzzy-searches and executes built-in actions.

- [ ] Define `Action` trait in `actions.rs`:
  ```rust
  trait Action: Send + Sync {
      fn id(&self) -> &str;
      fn label(&self) -> &str;
      fn category(&self) -> &str;
      fn parameters(&self) -> Vec<ActionParameter>;
      fn execute(&self, params: &ActionParams, world: &mut World);
  }
  ```
- [ ] `ActionRegistry` Bevy resource: `HashMap<String, Box<dyn Action>>`
- [ ] Port 13 built-in actions from `plugins/register-actions.ts` as Rust structs:
  - Settings: toggle-theme, font-size, font-family, line-numbers, edit-options
  - Node ops: filter, search-nodes, create-node, + others
- [ ] Fuzzy search in palette using `fuzzy-matcher` crate (replaces `fuse.js`)
- [ ] Palette UI: category browse at root, arrow-key nav, Enter to execute, Esc to close
- [ ] Parameter form: egui form fields per `ActionParameter::Type` (string, number, bool, select)
- [ ] Query input line in palette routes to query language parser (Phase 5)
- [ ] **Verify:** Open palette, type "theme", select toggle-theme → theme changes ✓

---

## Phase 5 — Query Language (Bird-Combinator SPL)

**Milestone:** Pipeline expressions execute against graph state from the command palette.

### Language design

```
# Pipe (compose) — sequential stages
nodes | filter tag=it-ops | sort pagerank desc | top 10

# Apply — parallel application then combine
nodes | apply {pagerank} {betweenness} | merge-with max

# Map — transform each element
nodes | map {n -> n.community}

# Recurse — apply stage until fixed point (graph traversal)
nodes where id=target | recurse neighbors | top 50

# Pair/zip — combine two streams
nodes | zip edges | with {n, e -> e.source == n.id}

# On — apply same transform to both sides of a join
(source_nodes, target_nodes) | on {n -> n.pagerank} | compare

# .nix escape hatch — evaluated by tvix-wasm
nodes | nix { nodes: filter (n: n.pagerank > 0.01) nodes }
```

### Full combinator → operator mapping (all Smullyan combinators, no bird names)

| Combinator | Lambda | Query operator | Graph meaning |
|-----------|--------|---------------|--------------|
| **I** | λx.x | `passthrough` / `id` | Identity; no-op stage |
| **K** | λxy.x | `const` / `keep` | Always return first arg; used in `filter` logic |
| **KI** | λxy.y | `second` / `drop-first` | Ignore first input; return second |
| **S** | λfgx.f x (g x) | `apply` | Apply f and g to same input, combine results |
| **B** | λfgx.f(gx) | `\|` (pipe) | Sequential composition of stages |
| **B1** | λfghi.f(g h i) | `compose3` | Three-stage sequential composition |
| **B2** | λfghi.f g (hi) | `compose-right` | Compose with two-arg final stage |
| **B3** | λfghij.f(g(hi)j) | `compose-deep` | Deep sequential composition |
| **C** | λfxy.fyx | `flip` | Swap argument order; reverse a binary op |
| **W** | λfx.fxx | `self-join` / `duplicate` | Apply stage to self (cross-join a set with itself) |
| **W1** | λfxy.fxyx | `self-apply` | Variant: apply to x then (y x) |
| **T** | λxf.fx | `into` / `apply-to` | Pipe a value into a function (thrush/arg-flip) |
| **M** | λff.ff | `twice` / `iterate-2` | Apply stage to its own output once |
| **M2** | λfxy.f x(fxy) | `twice-on` | Apply twice with argument threading |
| **Y** | fix-point | `recurse` | Apply stage until stable (graph closure/traversal) |
| **L** | λfg.f(gg) | `partial-recurse` | One step of Y unrolling |
| **O** | λfg.g(fg) | `chain-into` | g receives (f's output + g's prior) |
| **Q** | λfgx.g(fx) | `compose-rev` | Reverse composition (pipe right-to-left) |
| **Q1** | λfgx.f(gx) | same as B | Alias |
| **Q2** | λfgx.g(xf) | `reverse-apply` | Apply g to (x applied to f) |
| **Q3** | λfgx.f(xg) | `swap-apply` | Apply f to (x applied to g) |
| **Q4** | λfgx.x(fg) | `inject` | Apply (f∘g) into x |
| **R** | λfxy.yfx | `rotate` | Rotate three-arg order: f x y → y f x |
| **V** | λxyf.fxy | `pair` / `zip` | Cons/pair two streams; pass to combining fn |
| **V\*** | variant | `zip-with` | Zip with explicit combiner |
| **Φ** (S') | λfghi.f(gi)(hi) | `parallel` / `on-both` | Apply g and h to same input, combine with f |
| **Φ1** | λfghij.f(gi)(hij) | `parallel-3` | Three-way parallel application |
| **Ψ** | λfgxy.f(gx)(gy) | `on` / `map-both` | Apply same transform g to both x and y, combine |
| **Γ** | λfgxy.f(gxy)y | `fold-step` | One step of left fold |
| **E** | λfghij.fg(hij) | `split-apply` | Apply f to g and to (h i j) separately |
| **E\*** | λfghijkl.fg(hijkl) | `split-apply-5` | — |
| **F** | λfxy.yxf | `reverse-3` | Rotate args the other way: f x y → y x f |
| **G** | λfgxy.f(gy)x | `over` | Apply g to y, then f with x |
| **H** | λfgx.fxgx | `share` | Pass x to both positions |
| **J** | λfgxy.fx(fyx) | `join` | Apply f to x and to (f y x); used for joins |
| **J1** | variant | `join-rev` | — |
| **J2** | variant | `cross-join` | Full cross-join of two sets |
| **Θ** | Y variant | `fix` | Another fixed-point operator |

### Stats / aggregation operators (SPL parity)

| Operator | Meaning |
|---------|---------|
| `stats count` | Count nodes in stream |
| `stats avg field` | Average of a numeric field |
| `stats sum field` | Sum of a numeric field |
| `stats min/max field` | Min/max of a field |
| `dedup field` | Remove duplicates by field value |
| `eval field=expr` | Compute new field from expression |

### Field projection operators

| Operator | Meaning |
|---------|---------|
| `fields f1 f2` | Select only named fields |
| `rename old as new` | Rename a field |
| `where expr` | Filter with boolean expression (richer than `filter key=val`) |

### Graph-specific operators (built on combinators above)

| Operator | Meaning |
|---------|---------|
| `neighbors [depth=N]` | Expand to neighbor nodes (uses `recurse` internally) |
| `shortest-path to=id` | BFS shortest path to target |
| `subgraph` | Induce subgraph on current node set |
| `community` | Run Louvain community detection |
| `centrality [pagerank\|betweenness\|kcore]` | Compute centrality metric |
| `traverse expr` | Walk graph applying expr until fixed point (Y) |

- [ ] `parser.rs`: recursive descent parser
  - `Pipeline = Stage ("|" Stage)*`
  - `Stage = Ident Args*`
  - `Args = key=value | quoted_string | NixBlock`
- [ ] `combinators.rs`: `Op` enum (`Filter`, `Map`, `Sort`, `Top`, `Group`, `Nix(String)`, ...)
- [ ] `eval.rs`: interpreter walks `Pipeline` against `GraphState` (operates on `Vec<NodeId>`)
  - `Nix` op: serialize node stream as JSON → `tvix_wasm::eval_nix()` → deserialize result
- [ ] Results rendered in egui panel below palette (node list, click → metadata modal)
- [ ] **Verify:** `nodes | top 5` returns 5 nodes; `nix { builtins.toString 42 }` returns `"42"` ✓

---

## Phase 6 — Graph Data Wiring

**Milestone:** Real vault data visible in renderer; filterable via query language.

- [ ] `vault.rs`: Rust wikilink extractor using `ignore` crate (same exclusion contract as vault-search)
  - Two-pass: basename pre-pass for disambiguation → edge emission
  - Output: `Vec<(NodeId, NodeId)>` edge list
- [ ] `graph-ui` spawns `vault-search` subprocess on startup (or links as lib)
- [ ] HTTP client (`reqwest` + `tokio`) for `/ids?q=`, `/node/:id` (sidebar search)
- [ ] Load vault edges → `graph-layouts` fCoSE → positions → `graph-renderer` Bevy entities
- [ ] `state/selection.rs`: sync between egui sidebar clicks and Bevy entity highlights
- [ ] Metadata modal: egui `Window` on node click; fields from `/node/:id` response
- [ ] **Verify:** Load vault → graph renders with layout → click node → modal shows title/tags ✓

---

## Phase 7 — Nix Packaging

**Milestone:** `nix run .#graph-ui` opens app; `nix build` succeeds for all crates.

- [ ] Flake outputs (bird-nix pattern — lib + per-system packages):
  - `packages.graph-ui` — native binary (rustPlatform.buildRustPackage)
  - `packages.graph-ui-wasm` — WASM bundle via trunk
  - `packages.vault-search` — existing binary
  - `devShells.default` — cargo, wasm-pack, trunk, wasm-bindgen-cli, cargo-watch
- [ ] Add `graph-ui` as an input to the obsidian repo's flake (replaces vault-graph-cosmos long-term)
- [ ] **Verify:** `nix run .#graph-ui` ✓; `nix build .#vault-search` ✓

---

## Key Reference Files

| File | Purpose |
|------|---------|
| `rust-graph-renderer/src/lib.rs` | Bevy app builder to extend |
| `rust-graph-renderer/src/systems/graph_rendering.rs` | Rendering system |
| `rust-graph-renderer/todos.md` | Confirmed integration gaps |
| `rust-graph-layouts/src/types.rs` | Shared graph data model |
| `rust-graph-layouts/src/lib.rs` | LayoutManager WASM API |
| `jump-cannon/stores/actions.ts` | Action system to port |
| `jump-cannon/components/CommandPalette.vue` | UX reference for egui port |
| `jump-cannon/plugins/register-actions.ts` | 13 built-in actions |
| `obsidian/nix/packages/vault-search/src/` | Rust HTTP API (keep unchanged) |

---

## vault-graph-cosmos Feature Port

The Python app (`vault-graph-cosmos`, ~3600 lines) is the feature reference. These phases port its ~100 features into the Rust monorepo. All new crates added to `Cargo.toml` workspace members.

### New workspace dependencies (add to root `Cargo.toml`)

```toml
petgraph   = "0.6"
indexmap   = { version = "2", features = ["serde"] }
serde_yaml = "0.9"
regex      = "1"
rayon      = "1"
reqwest    = { version = "0.12", features = ["json"] }
tokio-util = { version = "0.7", features = ["rt"] }
rand       = "0.8"
```

---

## Phase 0 — vault-data: Shared Types Crate

**Milestone:** All other crates can `use vault_data::*` for the canonical graph types.

- [ ] Create `crates/vault-data/Cargo.toml` (`name = "vault-data"`, deps: serde, serde_json, indexmap)
- [ ] Add `crates/vault-data` to workspace `members`
- [ ] `src/lib.rs` — re-export all public types
- [ ] `src/types.rs`:
  ```rust
  pub type NodeId = String;

  #[derive(Debug, Clone, Serialize, Deserialize)]
  pub struct NodeMeta {
      pub id: NodeId,
      pub title: String,
      pub path: String,
      pub tags: Vec<String>,
      pub frontmatter: IndexMap<String, serde_json::Value>,
  }

  #[derive(Debug, Clone, Serialize, Deserialize)]
  pub struct NodeMetrics {
      pub pagerank: f64,
      pub betweenness: f64,
      pub degree: usize,
      pub kcore: usize,
      pub community: usize,
      pub wcc: usize,
  }

  #[derive(Debug, Clone, Serialize, Deserialize)]
  pub struct VaultNode {
      pub meta: NodeMeta,
      pub metrics: NodeMetrics,
      pub x: f32,
      pub y: f32,
      pub pinned: bool,
  }

  #[derive(Debug, Clone, Serialize, Deserialize)]
  pub struct VaultEdge {
      pub source: NodeId,
      pub target: NodeId,
  }

  #[derive(Debug, Default, Clone, Serialize, Deserialize)]
  pub struct VaultGraph {
      pub nodes: IndexMap<NodeId, VaultNode>,
      pub edges: Vec<VaultEdge>,
  }
  ```
- [ ] `src/schema.rs`:
  ```rust
  #[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
  pub enum FieldType { Text, Number, Bool, List, Entity, Date, Url, Unknown }

  #[derive(Debug, Clone, Serialize, Deserialize)]
  pub struct FieldSchema {
      pub name: String,
      pub field_type: FieldType,
      pub values: Vec<String>,   // enumerated values for filter dropdowns
      pub count: usize,
  }
  ```
- [ ] `src/palette.rs` — 17-color community palette as `pub const PALETTE: [Color32; 17]` (egui-compatible)
- [ ] **Verify:** `cargo check -p vault-data` ✓

---

## Phase 1 — vault-links: Wikilink Extraction

**Milestone:** Given a vault root, emit `VaultGraph` (nodes + edges) with populated `NodeMeta` and `FieldSchema` list.

- [ ] Create `crates/vault-links/Cargo.toml` (deps: vault-data, serde, serde_json, serde_yaml, regex, ignore, rayon)
- [ ] Add to workspace members
- [ ] `src/walker.rs` — `build_walker(vault_root)` using `ignore::WalkBuilder`:
  - Exclusions: `.obsidian/`, `Excalidraw/`, `Ink/`, `_hippo/`, `*.base`, `*.canvas`
  - Returns `ignore::Walk` iterator over `.md` files
- [ ] `src/frontmatter.rs` — `split_frontmatter(content) -> (Option<IndexMap<String,Value>>, &str)`:
  - Parse YAML block between `---` delimiters via `serde_yaml`
  - Returns `(frontmatter_map, body)`
- [ ] `src/extractor.rs` — `extract_links(body) -> Vec<String>`:
  - Regex: `\[\[([^\]|#]+)(?:[|#][^\]]*)?\]\]` captures wikilink targets
  - Strip path prefix, extension, heading anchors
- [ ] `src/lib.rs` — `pub fn build_vault_graph(vault_root: &Path) -> (VaultGraph, Vec<FieldSchema>)`:
  1. First pass (rayon par_iter): collect all basenames → `HashMap<basename, Vec<full_path>>` for disambiguation
  2. Second pass: for each `.md` file:
     - `split_frontmatter` → populate `NodeMeta` (id=relative path, title=H1 or filename, tags from frontmatter)
     - `extract_links` → emit edges (resolve basename via disambiguation map)
  3. Collect `FieldSchema` by iterating all frontmatter keys across all nodes; infer `FieldType` from value shape
  4. Return `(VaultGraph, Vec<FieldSchema>)`
- [ ] `src/doctype.rs` — detect doctype from frontmatter `type`/`doctype` field or path prefix (maps to `.opencode/doctypes.json` categories)
- [ ] Unit tests: disambiguation, wikilink extraction, frontmatter parse, doctype detection
- [ ] **Verify:** `cargo test -p vault-links` ✓; test against vault fixture with ≥3 notes

---

## Phase 2 — graph-metrics: Petgraph Analytics

**Milestone:** Given `VaultGraph` edges, compute and attach all `NodeMetrics` fields.

- [ ] Create `crates/graph-metrics/Cargo.toml` (deps: vault-data, petgraph, rand, rayon)
- [ ] Add to workspace members
- [ ] `src/lib.rs` — `pub fn compute_metrics(graph: &mut VaultGraph)`:
  - Build `petgraph::Graph<NodeId, ()>` from `graph.edges`
  - Attach computed metrics to each `graph.nodes[id].metrics`
- [ ] `src/pagerank.rs` — damping=0.85, max_iter=100, ε=1e-6 power iteration; normalize to [0,1]
- [ ] `src/betweenness.rs` — Brandes algorithm on petgraph; normalize by `(n-1)(n-2)/2`; rayon parallelized
- [ ] `src/kcore.rs` — iterative k-core decomposition; store core number as `kcore`
- [ ] `src/wcc.rs` — petgraph `connected_components` for undirected version; label each component
- [ ] `src/degree.rs` — in+out degree per node; stored as `degree`
- [ ] `src/community.rs` — greedy Louvain (~150 lines):
  - Initialize each node as its own community
  - Phase 1: iterate nodes, move to neighbor community that maximizes modularity gain; repeat until stable
  - Phase 2: contract graph; repeat until no improvement
  - Seed with `rand::SeedableRng::seed_from_u64(42)` for reproducibility
- [ ] Unit tests: PageRank on star graph, k-core on known graph, WCC on disconnected graph, community on two cliques
- [ ] **Verify:** `cargo test -p graph-metrics` ✓

---

## Phase 3 — Bevy Resource Wiring

**Milestone:** `VaultGraphResource` loaded at startup; graph data accessible to all Bevy systems.

- [ ] Add `vault-data`, `vault-links`, `graph-metrics` as deps to `graph-ui/Cargo.toml`
- [ ] `src/state/graph.rs`:
  ```rust
  #[derive(Resource, Default)]
  pub struct VaultGraphResource {
      pub graph: VaultGraph,
      pub schema: Vec<FieldSchema>,
      pub loaded: bool,
  }

  #[derive(Resource, Default)]
  pub struct SelectionState {
      pub hovered: Option<NodeId>,
      pub selected: Option<NodeId>,
      pub pinned: HashSet<NodeId>,
  }
  ```
- [ ] `src/systems/graph_load.rs` — `load_vault_graph` startup system:
  - Read `VAULT_ROOT` env var (default `~/vault` or argv[1])
  - Call `vault_links::build_vault_graph(root)`
  - Call `graph_metrics::compute_metrics(&mut graph)`
  - Store in `VaultGraphResource`
  - Spawn Bevy entities (one per node) with `Transform`, custom marker component `GraphNode { id }`
- [ ] Register `VaultGraphResource` and `SelectionState` as resources in `main.rs`
- [ ] Add `load_vault_graph` to `Startup` systems
- [ ] **Verify:** `cargo run -p graph-ui -- /path/to/vault` → logs node count at startup ✓

---

## Phase 4 — Force Simulation + Rendering

**Milestone:** Nodes laid out by force-directed simulation; edges drawn; color modes working.

### Force simulation

- [ ] `src/systems/force_sim.rs` — Verlet integration, run each `Update` frame when not converged:
  - Repulsion: `F = k² / dist` (Barnes-Hut quadtree optional, O(n log n))
  - Attraction (edges): `F = dist² / k` with spring constant
  - Gravity: weak pull toward origin, strength=0.05
  - Damping: velocity *= 0.9 each step
  - Pinned nodes: zero velocity, fixed position
  - Convergence: stop when max displacement < 0.5px
  - Expose `SimState { running: bool, alpha: f32 }` resource; UI can pause/resume

### Rendering

- [ ] `src/systems/render_nodes.rs` — sync `VaultNode.{x,y}` → Bevy `Transform` each frame
- [ ] Node visual: `Sprite` with circle mesh (radius proportional to `degree.sqrt()`); color by active color mode
- [ ] Color modes (enum in `UiState`):
  - `Community` — PALETTE[community % 17]
  - `Pagerank` — lerp red→yellow→green by normalized pagerank
  - `Degree` — lerp blue→orange by normalized degree
  - `Kcore` — lerp purple→white by kcore / max_kcore
  - `Wcc` — PALETTE[wcc % 17]
  - `Tag` — color by first matching tag group (it-ops, research, etc.)
- [ ] `src/systems/render_edges.rs` — draw edges with `bevy::prelude::Gizmos`:
  - `gizmos.line_2d(src_pos, dst_pos, edge_color)`
  - Edge alpha = 0.3 at rest; 1.0 if either endpoint is hovered/selected
  - Edge color: dim gray at rest; highlight color when active
- [ ] Labels: `Text2d` on each node, font size 10, hidden below zoom threshold (scale < 0.5)
- [ ] **Verify:** `cargo run -p graph-ui -- /path/to/vault` → nodes visible, edges drawn, colors by community ✓

---

## Phase 5 — Camera Controls

**Milestone:** Full camera navigation matching vault-graph-cosmos spec.

- [ ] `src/systems/camera.rs` — single Bevy system on `Update`:
  | Input | Action |
  |-------|--------|
  | WASD | Pan (speed proportional to zoom level) |
  | Q / E | Rotate ±15° |
  | R / F | Zoom in / out (multiply scale by 1.1) |
  | Shift+WASD | Pan 4× faster |
  | Ctrl+0 | Reset to default view |
  | Scroll up/down | Zoom toward/away from cursor position |
  | Right-click drag | Pan |
  | Double-click background | Fit-to-view (compute AABB of all nodes, set camera to center + scale) |
- [ ] Smooth interpolation: lerp camera toward target at 0.15/frame (avoids jarring snaps)
- [ ] Fit-to-view: AABB with 10% padding, clamp min scale
- [ ] **Verify:** All 9 input methods tested manually ✓

---

## Phase 6 — Node Interaction

**Milestone:** Hover, select, drag, and pin nodes; interaction state drives rendering.

- [ ] `src/systems/interaction.rs` — raycast from cursor → nearest node within 4px radius:
  - Convert `CursorMoved` window coords → world coords via `Camera::viewport_to_world_2d`
  - Hover: `SelectionState.hovered = Some(id)`; node outline brightens; tooltip shows title
  - Click (left, no drag): `SelectionState.selected = Some(id)`; opens metadata panel in sidebar
  - Drag (left, moved > 4px threshold): translate node `Transform`; sync to `VaultNode.{x,y}`; `SimState.alpha` bumped to 0.5 to re-settle neighbors
  - Double-click node: fit-to-view centered on node + 2-hop neighborhood
  - Right-click node: context menu (pin/unpin, isolate, copy id)
  - Pin toggle: `SelectionState.pinned.insert/remove(id)`; pinned nodes get lock icon overlay
- [ ] **Verify:** Click node → sidebar shows metadata; drag node → physics re-settles ✓

---

## Phase 7 — Metadata Modal / Sidebar Panel

**Milestone:** All frontmatter fields rendered correctly in sidebar; 8 field type renderers.

- [ ] `src/systems/sidebar.rs` — extend existing sidebar to show node metadata when `selected` is Some:
  - Header: title (H2), path (monospace small), doctype badge
  - Tags: each tag as a chip (click to filter graph to tag)
  - Frontmatter fields: iterate `schema` order, render by `FieldType`:

  | FieldType | Renderer |
  |-----------|---------|
  | `Text` | `ui.label(value)` |
  | `Number` | right-aligned `ui.label` |
  | `Bool` | checkbox (read-only) |
  | `List` | bullet list |
  | `Entity` | `[[wikilink]]` rendered as clickable button → select that node |
  | `Date` | formatted string (YYYY-MM-DD) |
  | `Url` | `ui.hyperlink(value)` |
  | `Unknown` | raw JSON string |

  - Special sections: `## Related` / `## See also` → extract wikilinks, render as entity list
  - `## Authors` → comma-separated entity chips
- [ ] Back button clears `selected`; breadcrumb trail for multi-hop navigation
- [ ] **Verify:** Click each field type in fixture notes → correct renderer used ✓

---

## Phase 8 — Search & Filtering

**Milestone:** Full-text search, regex, field drilldown, and focus mode all working.

### vault-search subprocess

- [ ] `src/systems/vault_search_client.rs`:
  - On startup: `tokio::process::Command::new("vault-search").arg("--port=0")` to pick ephemeral port; parse port from stdout
  - HTTP client via `reqwest`: `GET /ids?q={query}` → `Vec<NodeId>`; `GET /node/{id}` → `NodeMeta`
  - Reconnect on failure with 1s backoff

### Sidebar Search tab

- [ ] Text input → debounce 150ms → send to vault-search `/ids?q=` → highlight matching nodes
- [ ] Results list: node title + path, click → select node
- [ ] Regex toggle: prefix query with `re:` to route to regex search path
- [ ] Field filter drilldown (per `FieldSchema`):
  - `Text`/`Entity`: substring match input
  - `Number`: min/max range sliders
  - `Bool`: checkbox
  - `List`/`Tag`: multi-select checkboxes from `FieldSchema.values`
  - `Date`: date range picker
- [ ] Active filters shown as removable chips; "Clear all" button
- [ ] Filter logic: intersection of all active filters → highlight matching nodes, dim others

### Focus mode

- [ ] Toggle in action registry (`focus-mode` action):
  - When on: hide all nodes not in `SelectionState.selected`'s N-hop neighborhood (N configurable, default 2)
  - Unhide on toggle-off
- [ ] "Expand focus" / "Contract focus" actions adjust N

### Query language integration

- [ ] Palette query input (`|`-prefixed line) routes to `query::eval::evaluate(pipeline, &graph_resource.graph)`
- [ ] Results rendered in palette result panel: node list, click → select
- [ ] **Verify:** FTS "meeting" returns meeting notes; field filter `tags=it-ops` shows only IT-ops nodes; focus mode hides non-neighbors ✓

---

## Phase 9 — Persistence

**Milestone:** View state survives restarts; per-vault config supported.

- [ ] `src/persistence.rs`:
  ```rust
  #[derive(Serialize, Deserialize, Default)]
  pub struct ViewState {
      pub camera_x: f32,
      pub camera_y: f32,
      pub camera_scale: f32,
      pub color_mode: String,
      pub sidebar_open: bool,
      pub sidebar_width: f32,
      pub active_tab: String,
      pub pinned_nodes: Vec<NodeId>,
  }
  ```
- [ ] Config path: `~/.config/jump-cannon/view.json` (or `$XDG_CONFIG_HOME/jump-cannon/view.json`)
- [ ] Load on startup (before `load_vault_graph`); save on graceful exit (`AppExit` event) and every 60s
- [ ] Per-vault config: hash vault root path → separate config file per vault
- [ ] **Verify:** Pin nodes, adjust camera → restart → state restored ✓

---

## Key Reference Files (vault-graph-cosmos)

| Feature area | Python source | Rust target |
|---|---|---|
| Wikilink extraction | `vault-graph-cosmos/graph.py` L1-300 | `crates/vault-links/src/extractor.rs` |
| Force simulation | `vault-graph-cosmos/simulation.py` | `crates/graph-ui/src/systems/force_sim.rs` |
| PageRank | `vault-graph-cosmos/metrics.py` L1-60 | `crates/graph-metrics/src/pagerank.rs` |
| Betweenness | `vault-graph-cosmos/metrics.py` L61-130 | `crates/graph-metrics/src/betweenness.rs` |
| Community | `vault-graph-cosmos/metrics.py` L131-220 | `crates/graph-metrics/src/community.rs` |
| Node rendering | `vault-graph-cosmos/renderer.py` | `crates/graph-ui/src/systems/render_nodes.rs` |
| Camera | `vault-graph-cosmos/camera.py` | `crates/graph-ui/src/systems/camera.rs` |
| Metadata modal | `vault-graph-cosmos/ui/modal.py` | `crates/graph-ui/src/systems/sidebar.rs` |
| Search/filter | `vault-graph-cosmos/search.py` | `crates/graph-ui/src/systems/vault_search_client.rs` |
| Persistence | `vault-graph-cosmos/state.py` | `crates/graph-ui/src/persistence.rs` |
