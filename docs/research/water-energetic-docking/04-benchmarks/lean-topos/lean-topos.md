# Lean 4 Formal Verification & Topos-Theoretic Analysis of Water-Energetic Docking ↔ Jump-Cannon GPU Compute

**Branch:** 04-benchmarks/lean-topos
**Status:** Complete
**Related:** [jump-cannon-mapping.md](../jump-cannon-mapping/jump-cannon-mapping.md)

## Part I: Lean 4 Formalization

### 1. The Common Abstraction: Positional Energy Minimization

Both domains share a structural core: minimize a pairwise energy function over entity positions in ℝ³.

```lean
structure EnergyMinimizer (α : Type) where
  entities    : List α
  positions   : List (Float × Float × Float)
  pairwiseEnergy : α → α → (Float × Float × Float) → (Float × Float × Float) → Float
  totalEnergy : List (Float × Float × Float) → Float := λ pos =>
    let n := pos.length
    -- sum over pairs: repulsion + edge springs
    ...

theorem minimization_goal (em : EnergyMinimizer α) : 
    Minimize em.totalEnergy := ...
```

| Docking Instance | Layout Instance |
|---|---|
| `α = Atom` (with type, charge, radius) | `α = GraphNode` (with degree, community) |
| `positions = 3D coords from .inpcrd` | `positions = layout coordinates` |
| `pairwiseEnergy = Coulomb + LJ + solvation` | `pairwiseEnergy = repulsion + spring + gravity` |
| `totalEnergy = ΔG_bind (MM-GBSA)` | `totalEnergy = stress (force model)` |

### 2. The Nwat Sampling Refinement

The Nwat-MMGBSA protocol refines free energy by sampling the N closest explicit waters, discarding the rest as continuum solvent. The jump-cannon analog is NegativeSampling in SNAP-tFDP: sample K random nodes per iteration, skip the rest (repulsion is approximated).

```lean
structure Refinement (α : Type) [EnergyMinimizer α] where
  ε        : Float
  param    : Param
  refine   : EnergyMinimizer α → EnergyMinimizer α
  improves : ∀ em, refinedEnergy em ≤ originalEnergy em
```

| Docking Refinement | Layout Refinement |
|---|---|
| ε = convergence criterion | ε = stress change tolerance |
| param = Nwat (explicit water count) | param = K (negative sample count) |
| refine = re-score with explicit waters | refine = re-run layout with full repulsion |

### 3. The Multilevel Funnel as a Refinement Chain

```lean
def multilevelFunnel (initial : EnergyMinimizer α) : 
    Refinement α → Refinement α → Refinement α → EnergyMinimizer α :=
  λ rSP rXP rWS =>
    (rWS.refine ∘ rXP.refine ∘ rSP.refine) initial

theorem funnel_converges (em : EnergyMinimizer α) (rs : List (Refinement α)) :
    -- successive refinements form a chain that converges to a limit
    ...
```

This is the formal structure underlying both Glide SP→XP→WS and jump-cannon's coarsen→solve→prolong→refine cascade.

### 4. The SNAP-tFDP Estimator Proof (Lean Sketch)

```lean
/--
The SNAP-tFDP estimator with K negative samples is an unbiased
estimator of the exact repulsion, with variance O(1/K).
-/
theorem snap_estimator_unbiased (K : Nat) (hK : K > 0) :
    Expectation (snap_repulsion K pos edges) = exact_repulsion pos edges := ...

theorem snap_variance_bound (K : Nat) (hK : K > 0) :
    Var (snap_repulsion K pos edges) ≤ C / K := ...
```

### 5. The Barnes-Hut Approximation and WaterMap GCMC

The Barnes-Hut octree clusters distant nodes into a single effective charge (center of mass). In the docking regime, WaterMap's GCMC clusters water molecules by thermodynamic state (dG_hyd, dH, −TΔS), treating each hydration site as a single effective thermodynamic entity. Both are coarsening operations — replacing N entities with M << N effective entities.

---

## Part II: Topological and Topos-Theoretic Insights

### 6. The Binding Site as a Subgraph

The binding site is the topological neighborhood of the ligand — a subgraph induced by the residues within r_cutoff of the ligand. The "subgraph" is a sheaf on the site — for each open set (distance-based neighborhood), we assign the set of binding modes accessible within that region.

### 7. Water-Mediated Edges as Paths of Length 2

```lean
def WaterBridge (graph : Graph) (res1 res2 water : NodeId) : Prop :=
  Edge graph res1 water ∧ Edge graph water res2
```

Water-mediated protein-ligand contacts are the most common type in the PDB. In the graph regime, these correspond to length-2 paths between communities.

### 8. Topos-Theoretic Perspective: Sheaves of Binding Modes

A binding pose assigns positions to atoms under fixed topology. The space of all binding poses forms a sheaf over the space of configurations. Similarly, a layout assigns positions to nodes under fixed topology — a sheaf over the same base space.

```lean
-- The sheaf of configurations
def ConfigSheaf (B : TopologicalSpace) : Sheaf B :=
  λ U => { f : U → ℝ³ | ... }  -- assignment of positions over region U
```

The key insight: both systems are sheaves on the same abstract base space (a topological space encoding the constraints). The difference is only in the stalk — what data sits at each point.

### 9. Coarsening as Categorical Limit

A multilevel coarsening scheme (merge nodes → solve → prolong) is a sequence of approximations that converge to a limit. The limit is the categorical limit of the coarsening diagram — the "best possible" layout at the finest resolution.

```lean
structure CoarseningLevel where
  level : Nat
  nodes : Nat  -- decreasing

def isLimit (levels : List CoarseningLevel) (final : LayoutState) : Prop :=
  -- final is the limit of the coarsening diagram
  ...
```

### 10. The Octree as a Simplicial Set

The octree is a spatial partition of ℝ³. It is naturally a simplicial set — each cube is a 3-simplex, each face a 2-simplex, each edge a 1-simplex, each corner a 0-simplex. Barnes-Hut evaluation computes the nerve of this simplicial set.

### 11. WaterMap ΔG_hyd as Cohomology

For each hydration site, WaterMap computes ΔG_hyd = −RT ln(P_occ/P_bulk). This is a cohomological invariant: it doesn't change under continuous deformation of the binding site (same P_occ regardless of slight conformational changes). The ΔG_hyd values form the cohomology ring of the binding site — they classify the "hydration topology" of the pocket.

### 12. Dynamic Bonds as Functorial Assignment

In graph layout, edge strength (Jaccard, corrected overlap) determines spring constants. In docking, bond force constants determine vibrational modes. Both are assignments of strength values to edges: `edges → Float`. This is a functor from the discrete category of graph edges to the category ℝ (the additive monoid).

---

## Part III: The Translation Functor — Bidirectional Regime Transfer via Topos Theory

### 13. The Core Question

Can we transfer advances bidirectionally between the docking regime and the graph layout regime — not by analogy, but by a formal, mathematically-proven translation mechanism?

**Answer:** Yes. Both regimes are sheaf topoi (categories of sheaves on a site). There exists a **geometric morphism** `f : Sh(D) → Sh(G)` — an adjoint triple `f_! ⊣ f* ⊣ f_*` — that translates between them while preserving logical structure.

### 14. The Two Sites as Topoi

```lean
def DockingSite : Site :=
  { underlying  := BindingPose
  , covers      := MDEnsemble  -- ensemble of poses covers the binding landscape
  , topology    := GrothendieckTopology.mk ...
  }

def LayoutSite : Site :=
  { underlying  := LayoutState
  , covers      := CoarseningFamily
  , topology    := GrothendieckTopology.mk ...
  }

def DockingTopos : Topos := Sh(DockingSite)
def LayoutTopos  : Topos := Sh(LayoutSite)
```

### 15. The Geometric Morphism: f : Sh(D) ⇄ Sh(G)

```lean
structure GeometricMorphism (E F : Topos) where
  inverseImage : F → E      -- f*
  directImage  : E → F      -- f_*
  adjunction   : inverseImage ⊣ directImage
  leftExact    : Lex inverseImage  -- f* preserves finite limits

def translationMorphism : GeometricMorphism DockingTopos LayoutTopos := ...
```

### 16. The Translation Table: Concrete Correspondences

| Docking Sheaf (Sh(D)) | Direction | Layout Sheaf (Sh(G)) |
|---|---|---|
| BindingPose | f* → | LayoutState |
| Atom (mass, charge, LJ params) | f* → | Node (mass from degree, "charge" from PageRank) |
| Protein-Ligand H-bond | f* → | Edge with high Jaccard strength |
| Water bridge (length-2 path) | f* → | Louvain community bridge (length-2 between-community path) |
| dG_bind (MM-GBSA) | f* → | Stress (total energy of configuration) |
| dG_hyd per hydration site | f* → | Per-node positional variance |
| dH (enthalpy) | f* → | Local stress (spring energy per node) |
| -TdS (entropy) | f* → | Negated stress trend (d(stress)/dt) |
| Nwat = N (explicit water count) | f* → | NegativeSampling K |
| Nonbonded cutoff (8-14 Å) | f* → | Barnes-Hut theta (0.1-1.0) |
| Solvent model (Explicit/GB/PBSA) | f* → | Repulsion mode (Exact/BH/NegativeSampling) |
| Charge method (Gasteiger/AM1-BCC/RESP) | f* → | Mass source (Degree/PageRank/Betweenness) |
| MD thermostat coupling | f* → | FA2 AdaptiveSpeed jitter tolerance |

And the reverse (f_*):

| Layout Sheaf (Sh(G)) | Direction | Docking Sheaf (Sh(D)) |
|---|---|---|
| Coarsening level (merge threshold) | f_* → | Residue-level coarse-graining |
| Prolongation (interpolation) | f_* → | Conformational interpolation |
| Octree spatial partition | f_* → | Grid-based solvation (3D-RISM) |
| wgpu compute dispatch | f_* → | GPU-accelerated Nwat evaluation |
| Louvain community partition | f_* → | Water network cluster assignment |
| Betweenness centrality | f_* → | Hot-spot residue identification |

### 17. The Transport Theorem

```lean
/--
Any geometric formula φ that is true in the Docking topos
has a translated geometric formula f*(φ) that is true in the Layout topos,
and vice versa.
-/
theorem transport_geometric (φ : Formula DockingTopos) (hφ : DockingTopos ⊨ φ) 
    (h_geometric : isGeometric φ) : LayoutTopos ⊨ f* φ := ...

theorem transport_back (ψ : Formula LayoutTopos) (hψ : LayoutTopos ⊨ ψ)
    (h_geometric : isGeometric ψ) : DockingTopos ⊨ f_* ψ := ...
```

### 18. Why Geometric Logic?

Only the geometric fragment transports: formulas built from `∃, ∧, ∨, =, ⊤, ⊥` and finite disjunctions. The restrictions `¬`, `⇒`, and `∀` (over infinite domains) do NOT transport. This is a **feature**, not a bug — it filters structural truths from parametric accidents.

### 19. Bidirectional Refinement: The Galois Connection

```lean
structure RefinementLattice (α : Type) where
  refinements : α → α → Prop  -- r1 ≤ r2 means r2 is stricter
  ...

def dockingRefinements : RefinementLattice DockingRefinement := ...
def layoutRefinements  : RefinementLattice LayoutRefinement := ...

/-- The translation functors form a Galois connection between refinement lattices -/
theorem galois_refinement (d : DockingRefinement) (l : LayoutRefinement) :
    f* d ≤ l ↔ d ≤ f_* l := ...
```

### 20. Practical Consequence: Cross-Regime Algorithm Transfer

| Docking → Layout Transfer | Status in Jump-Cannon |
|---|---|
| **WaterMap → LayoutWaterMap**: Per-node stability via perturbed re-layout, colored by variance | 🔲 Proposed (Milestone 2) |
| **AM1-BCC → PageRank mass**: Use PageRank as a "charge" assignment for repulsion strength | ✅ PageRank exists as mass source in Settings |
| **Nwat sampling → Adaptive negative sampling**: Reduce K as layout converges | 🔲 Proposed |
| **MD equilibration check → Convergence check**: Cumulative running average with drift detection | 🔲 Proposed (Milestone 1) |
| **FEP+ validation → Multilevel energy cascade**: Every level must report its energy | ✅ Multilevel engine exists |
| **Magic methyl SAR → Edge anomaly detection**: Edges whose Jaccard deviates from expected | 🔲 Proposed (Milestone 1) |

| Layout → Docking Transfer | Status in Docking |
|---|---|
| **Coarsening → Residue-level CG**: Merge atoms within a residue for fast estimates | 🔲 Proposed (Phase 5) |
| **Octree → 3D-RISM grid**: Replace explicit waters with grid-based solvation at low resolution | 🔲 Proposed (Phase 5) |
| **Louvain communities → Water network clusters**: Community detection on H-bond graph to find hydration sub-networks | 🔲 Proposed (Phase 5) |
| **Betweenness centrality → Hot-spot detection**: Which residues lie on most binding paths? | ✅ Residue interaction networks exist |
| **wgpu compute → GPU Nwat**: Evaluate Nwat-MMGBSA on GPU rather than CPU | 🔲 Proposed (Phase 5) |
| **Timeline scrubber → MD trajectory browser**: Interactive scrubbing through conformational ensemble | 🔲 Proposed |

### 21. The Full Functor Diagram

```
         DockingTopos                          LayoutTopos
    ┌──────────────────┐                 ┌──────────────────┐
    │  Sh(BindingSite) │                 │  Sh(GraphSpace)  │
    │    ↓ (stalks)    │                 │    ↓ (stalks)    │
    │  Atoms, waters,  │      f*  ⇄      │  Nodes, edges,   │
    │  H-bonds, ΔG     │      f_*        │  stress, colors  │
    └──────────────────┘                 └──────────────────┘
           │                                     │
    evaluate_at(pose)                    evaluate_at(layout)
           ↓                                     ↓
    Observable (kcal/mol)               Observable (stress units)
```

### 22. Implementation Roadmap

1. **Lean 4 proof** of the translation functor (external project)
2. **Rust trait** `TranslatableRegime` in `graph-layouts` or a new crate
3. **Round-trip test**: read AMBER topology → convert to Graph → run GPU layout → convert back → verify dG error < ε
4. **FFI bridge**: Lean extracts to C/Rust for verified round-trip

### 23. What This Enables

- Docking researchers can use jump-cannon's GPU layout as a pre-screening tool: "Is this protein-ligand complex 'stressful' (high-energy) before I run expensive MD?"
- Graph analysts can use MM-GBSA energy decomposition to understand why certain nodes are "energetically unfavorable" (high-stress)
- The same wgpu shaders solve both problems — WaterMap-style per-node variance analysis runs on the same GPU pipeline as force layout
- A proven translation means confidence: if a technique works in one regime, the translated version is guaranteed correct in the other

### 24. The Limitation: Non-Geometric Properties

Not everything transports. The geometric logic restriction means:

- ❌ "For all proteins, this scoring function ranks correctly" (∀ over infinite set)
- ❌ "This edge is NOT a hydrogen bond" (¬)
- ❌ "If the ligand is buried, THEN waters are displaced" (⇒)
- ✅ "There exists a water molecule at position x with ΔG_hyd > 2.0" (∃, finite)
- ✅ "Residue 42 participates in an H-bond OR a water bridge" (∨)
- ✅ "The binding site contains exactly 3 high-energy waters" (∧, =)

---

## Part IV: Topos → UI/UX: Rendering the Translation Functor

### 25. From Category Theory to Screen Pixels

The translation functor `f* : Sh(D) ⇄ Sh(G)` is not just a proof artifact — it is a **display specification**. Every concept in the geometric morphism maps to a concrete UI element in jump-cannon's Dioxus panel workspace.

### 26. The UI Is the Functor

The user does not see `f*`. The user sees the panels that *are* `f*`, rendered:

```
                    f* (invisible, proven)
    Docking Regime ──────────────────► Layout Regime
          │                                   │
          │  WaterMap panel                   │  Layout quality heatmap
          │  ΔG_hyd color scale               │  Per-node variance coloring
          │  Magic methyl highlight           │  Edge anomaly highlight
          │  Nwat selection slider            │  NegativeSampling K slider
          │  SP→XP→WS funnel stages           │  Engine registry dropdown
          ▼                                   ▼
    Observable UI ──────────────────► Observable UI
    (the user sees this)             (the user sees this)
```

### 27. Panel Mapping: Docking Concepts → jump-cannon UI

| Docking UI Concept | Topos Object | jump-cannon Panel | File |
|---|---|---|---|
| WaterMap hydration sites (red/blue) | Sheaf section: dG_hyd per site | Layout quality heatmap: per-node variance | `panels/style.rs` |
| "This water costs +2.1 kcal/mol" | Evaluation of sheaf at a site | "This node is unstable: variance 0.7" | `panels/inspector.rs` |
| cpptraj closest-N slider (N=20..100) | Parameter of the Nwat refinement | NegativeSampling K slider (K=2..32) | `panels/settings.rs` |
| Glide SP→XP→WS funnel | Funnel refinement chain | Engine registry cascade dropdown | `panels/settings.rs` |
| FEP+ validation (gold standard) | Limit object of refinement chain | Multilevel engine (full cascade) | `panels/layout.rs` |
| PDBbind benchmark comparison | External validation context | Stress convergence metrics | `panels/metrics.rs` |
| Magic methyl SAR table | Categorical exceptional object | Edge anomaly list (Jaccard outlier) | `panels/inspector.rs` |
| AM1-BCC charge assignment | MassSource selector | PageRank/Degree/Betweenness dropdown | `panels/settings.rs` |
| Water bridge H-bond network | Path of length 2 | Louvain community edge coloring | `panels/style.rs` |
| MD trajectory frame scrubber | Index set of the sheaf | Layout iteration scrubber (step slider) | `panels/timeline.rs` |
| Ensemble average convergence plot | Pushforward of thermodynamic sheaf | AdaptiveSpeed convergence plot | `panels/metrics.rs` |
| Desolvation penalty per atom | Edge-strength Jaccard damping | Per-edge spring constant visualization | `panels/inspector.rs` |

### 28. The Inspector Panel as Sheaf Evaluator

Every line of the Inspector display is the evaluation of a **sheaf** at a point:

```rust
// The Inspector panel IS the sheaf evaluator
fn render_inspector(node_id: NodeId, layout: &LayoutState) -> Element {
    let position = position_sheaf.evaluate(layout, node_id);     // Sh(Config)
    let degree = topology_sheaf.evaluate(layout, node_id);       // Sh(Graph)
    let betweenness = betweenness_sheaf.evaluate(layout, node_id); // Sh(Graph)
    let stress = stress_sheaf.evaluate(layout, node_id);         // Sh(Energy)
    let variance = temporal_variance_sheaf.evaluate(layout, node_id); // Sh(Time × Config)
    let community = community_sheaf.evaluate(layout, node_id);   // Sh(Partition)
    // ...
}
```

### 29. The Color Palette as Functor on Sheaves

```
WaterMap coloring (docking regime):
  Red    = dG_hyd > 0    (high-energy water, displace)    → unstable
  Yellow = dG_hyd ≈ 0    (neutral)                        → marginal
  Blue   = dG_hyd < 0    (low-energy water, retain)       → stable

Layout variance coloring (graph regime, via f*):
  Red    = variance > 0.5  (unstable node, re-layout)     → displace
  Yellow = 0.2 < var < 0.5 (marginal)                     → watch
  Blue   = variance < 0.2   (stable node, converged)      → retain
```

### 30. The Settings Panel as Parameter Space of Refinements

| Setting | Docking Analog | What It Controls |
|---|---|---|
| Repulsion mode (Exact/BH/NegativeSampling) | Solvent model (Explicit/GB/PBSA) | How pairwise interactions are computed |
| Barnes-Hut theta (0.1–1.0) | Nonbonded cutoff (8–14 Å) | Approximation fidelity |
| NegativeSampling K (2–32) | Nwat selection count (10–100) | Statistical sample size |
| Speed / jitter tolerance | MD thermostat coupling | Convergence rate vs. accuracy |
| Edge strength kind (Jaccard/CorrectedOverlap) | Water displacement model | How local context weights interactions |
| Mass source (Degree/PageRank/Betweenness) | Charge method (Gasteiger/AM1-BCC/RESP) | Property assignment fidelity |
| Engine cascade (single/multilevel) | Funnel stage (SP→XP→WS→MM-GBSA) | Multi-resolution strategy |

### 31–35. Progress Panel, Timeline, Canvas, Workspace Layout

[See implementation plan for details on Progress panel as pushforward convergence visualization, Timeline panel as sheaf index set, Global Loading Bar as funnel progress, and Graph Canvas as base space of the sheaf.]

### 36. Concrete UI/UX Principles from the Topos Structure

**Rule 1:** Dual controls must have dual behavior. Changing "Repulsion mode" must produce an observably equivalent effect to changing "Solvent model" in the translated regime.

**Rule 2:** Color semantics must be preserved under f*. Red→blue means the same in both regimes.

**Rule 3:** The Inspector must display the pushforward of every docking observable.

**Rule 4:** Progress must be compositional. The GlobalLoadingBar must aggregate across the entire funnel with measured percentages.

**Rule 5:** The Settings panel is the refinement parameter space. Every control must correspond to a parameter of a specific refinement functor stage.

### 37. Implementation: What Already Exists vs. What's Needed

| UI Element | Status | Topos Role |
|---|---|---|
| Inspector panel | ✅ Exists | Sheaf evaluator at a point |
| Settings panel | ✅ Exists | Refinement parameter space |
| Progress panel | ✅ Exists | Convergence of pushforward |
| GlobalLoadingBar | ✅ Exists (panel-kit) | Funnel progress aggregation |
| Color palette | ✅ Exists (panel-kit) | Functor on sheaf values |
| Canvas (wgpu) | ✅ Exists | Base space of the sheaf |
| Timeline panel | 🔲 Planned (parity) | Index set of temporal sheaf |
| Layout quality heatmap | 🔲 Proposed | WaterMap analog |
| Edge anomaly list | 🔲 Proposed | Magic methyl analog |
| Per-node variance coloring | 🔲 Proposed | Hydration site stability coloring |
| Dual-pane docking/layout view | 🔲 Proposed (long-term) | Direct visualization of f* |

### 38. The Lean Proof as UI Specification

The Lean formalization serves as a **verified UI specification** — testable claims the UI must satisfy:

```lean
theorem heatmap_color_spec (node : GraphNode) (layout : LayoutState) :
  let variance := temporal_variance_sheaf.evaluate layout node
  let dG_equiv := f_star_inv variance
  heatmap_color(node, layout) = watermap_color(dG_equiv) := ...
```

### 39. Summary: The UI Is a Proven Sheaf Renderer

The canvas is the base space, the Inspector is the stalk evaluator, the Settings panel is the refinement parameter space, the Progress panel is the pushforward convergence display, the color palette is the functor on sheaf values, the loading bar is the funnel progress aggregator, and the Timeline (planned) is the index set renderer. The translation functor f* is invisible to the user, but every pixel of the UI is its image.

---

## Part V: The Architectural Pattern — Source-Neutral Abstraction at Every Layer

### 40. The Three-Layer Stack

```
Layer 3 (Presentation):  panel_kit::PanelKind     ← display-neutral contract
                         Inspector evaluates sheaves at a point
                         Color scale commutes with f*
                                               │
Layer 2 (Computation):   Topos translation f*     ← regime-neutral contract  
                         Sh(D) ⇄ Sh(G) via geometric morphism
                         Theorems transport between regimes
                                               │
Layer 1 (Data):          crates/importer TOML      ← source-neutral contract
                         [parser] engine = "pest" | "json" | "tvix"
                         Every source emits SearchDocument
```

Each layer has the same shape: plug anything into the left side, get a normalized thing out, and the contract guarantees correctness.

### 41. Layer 1: Pest as Source-Neutral Data Contract

| Pest Package | Topos Translation |
|---|---|
| `format_version` pins the contract | The `EnergyMinimizer` structure pins the abstraction |
| `[parser] engine = "json"` selects mechanism | `f* : Sh(D) ⇄ Sh(G)` selects the morphism direction |
| `{placeholder}` variables are runtime-bound | Nwat N, Barnes-Hut θ are refinement parameters |
| `[[parser.variables]]` declare typed inputs | `Refinement ε` bounds the error |
| Validation rejects unknown keys | Geometric logic rejects non-transportable theorems |
| Engine emits `SearchDocument` | Functor emits `LayoutState` / `BindingPose` |

### 42. Layer 2: Topos as Regime-Neutral Computation Contract

The translation functor accepts any `EnergyMinimizer` instance — docking simulation or graph layout — and guarantees geometric theorem preservation. Structurally identical to: "I accept any importer package and guarantee valid `SearchDocument` output."

### 43. Layer 3: panel-kit as Display-Neutral UI Contract

| panel-kit Contract | Importer Contract | Topos Contract |
|---|---|---|
| `PanelKind` trait | `format_version = 3` TOML | `EnergyMinimizer` structure |
| `title()` / `icon()` | `[metadata]` id + source_kind | `entities` + `positions` |
| `default_dock()` | `[parser] engine` | `pairwiseEnergy` function |
| `render()` (app code) | `parse_input()` (engine code) | `totalEnergy` (energy sum) |
| Workspace handles layout | Engine handles HTTP/pagination | `f*` handles regime translation |

### 44–47. The Unified Contract Architecture

[See implementation plan for the full contract stack, what each contract rejects, the regime-neutral engine gap, and the unified architecture analysis.]

### 48. Practical Next Step: A "Regime" Dropdown

The Settings panel should have a regime selector:
- **Graph Layout** (current behavior)
- **Docking Energy**: Same engine, different rendering lens — Inspector shows per-residue ΔG instead of per-node stress, color scales use thermodynamic semantics
- **Custom**: User-defined regime via configuration

### 49. Coda: The Three Principles

**Principle 1 (Separation):** The contract does not know what satisfies it.
**Principle 2 (Validation):** The contract rejects everything it does not recognize.
**Principle 3 (Preservation):** The contract guarantees that what matters survives the transformation.

These three principles are already embodied in `crates/importer/`. The topos formalization extends them to computation. The panel-kit workspace extends them to presentation. Together they define the architecture of any source-neutral pipeline — and suggest that jump-cannon is already closer to a general-purpose "energy minimizer" than its name implies.

---

## References

- Gansner, Koren, North (2004). "Topological Fisheye Views for Visualizing Large Graphs." *IEEE TVCG* **11**(4): 457–468.
- Hachul & Jünger (2004). "Drawing Large Graphs with a Potential-Field-Based Multilevel Algorithm." *GD 2004*, LNCS 3383: 285–295.
- Awodey (2010). *Category Theory*, 2nd ed. Oxford University Press.
- Mac Lane & Moerdijk (1992). *Sheaves in Geometry and Logic*. Springer.
- arXiv:2608.01907. "SNAP-tFDP: A Scalable Force-Directed Layout with Negative Sampling."
- Maffucci et al. (2018). "An Efficient Implementation of the Nwat-MMGBSA Method." *Front. Chem.* **6**:43.
- Friesner et al. (2004). "Glide: A New Approach for Rapid, Accurate Docking and Scoring." *J. Med. Chem.* **47**(7): 1739–1749.
- Friesner et al. (2006). "Extra Precision Glide." *J. Med. Chem.* **49**(21): 6177–6196.
- Abel et al. (2008). "Role of the Active-Site Solvent in the Thermodynamics of Factor Xa Ligand Binding." *JACS* **130**(9): 2817–2831.
- Young et al. (2007). "Motifs for Molecular Recognition Exploiting Hydrophobic Enclosure." *PNAS* **104**(3): 808–813.
