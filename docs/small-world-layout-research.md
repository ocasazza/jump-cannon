# Small-world graph layout — research notes

Why force-directed layouts turn highly-clustered, low-diameter ("small-world")
graphs into a central hairball, and the verified techniques that fix it. Compiled
from a fan-out + adversarial-verification research pass (2026-05-31); each claim
below cites a primary source and carries the verification vote (e.g. `3-0` = all
three independent verifiers confirmed).

Companion to [`layout-algorithms.md`](layout-algorithms.md) (algorithm survey) and
[`geometric-engine-plan.md`](geometric-engine-plan.md) (the engine these techniques
plug into).

---

## The problem (verified `3-0`)

> "for graphs with low diameter … these techniques begin to break down visually
> even when the graph has only a few hundred nodes. Typical algorithms produce
> images where nodes clump together in the center of the screen."
> — van Ham & van Wijk, *Interactive Visualization of Small World Graphs* (CGF 2008)

Root cause: a conventional spring model keeps **all** neighbours close, so a dense
low-diameter graph relaxes to a near-uniform blob. Global "shortcut" edges (low
neighbourhood overlap) pull otherwise-distinct communities on top of each other.

## The unifying insight

All four technique families reduce to **one hook**: produce a per-edge
*"intra-cluster vs. global-shortcut"* signal and feed it into the layout. The
families differ in where they inject it (edge weight, force law, initial
embedding, or interaction).

---

## Family 1 — edge-strength weighting (best validated, cheapest, GPU-friendly)

Two-stage pipeline (`2-1`): (i) compute a per-edge importance score from
neighbourhood overlap / triangle count; (ii) use it to weight or filter edges.
`T(u,v)` = common neighbours of the endpoints = number of triangles on the edge.

| Metric | Formula | Notes |
|---|---|---|
| **Jaccard / topological overlap** (`3-0`) | `o(e) = T / ((deg u −1)+(deg v −1) − T)` | = Jaccard over neighbour sets `N(u)\{v}`, `N(v)\{u}`. Cheapest. Over-emphasises edges in tiny dense subgraphs. |
| **Batagelj corrected overlap** (`3-0`) | `o'(e) = T / (μ + M(e) − T)`, `μ = max_e T(e)`, `M(e) = max(deg u, deg v) − 1` | Normalised `[0,1]`; fixes the small-dense-subgraph over-emphasis (`o>0.8 ⇒ o'<0.2`). |
| **Quadrilateral Simmelian `Q`** (`3-0`) | `Q(u,v) = q(u,v)/√(q(u)·q(v))`, `q(v)=Σ_{w∈N(v)} q(v,w)` | `q` = quadrangles on the edge. *Purpose-built for de-hairballing*; "clearly dominates when normalised" (Nocaj/Ortmann/Brandes, JGAA 2015). Needs quadrangle enumeration → heavier. |

Complexity `O(m·a(G))` with arboricity `a(G) ≤ √m` (small for real graphs);
triangle listing (Chiba–Nishizeki) is the parallel kernel → **embarrassingly
parallel, GPU-amenable** (`3-0`).

**How to feed a force/stress engine:** map the strength to a per-edge spring
**rest length** (strong → short, weak → long) and/or attraction multiplier.
Strong edges pull communities tight; weak shortcuts are allowed to stretch, so
clusters separate. *(This is the lever this repo uses: `edge_len` → the geometric
engine's per-edge `target_len`.)*

Sources: van Ham & van Wijk CGF 2008; Satuluri et al. *Local Similarity* (arXiv
1505.00564); Nocaj/Ortmann/Brandes *Untangling the Hairballs* (Konstanz, JGAA
2015); Batagelj *Corrected overlap weight* (arXiv 1906.04581).

## Family 3 — the force-model fix: Noack's LinLog (most directly relevant, `3-0`)

> "Noack proves that by using the 1-PolyLog (LinLog) model we obtain an embedding
> in which the distance between two clusters is inversely proportional to their
> coupling `E(C1,C2)/|C1||C2|` … intra-cluster edges grow while inter-cluster edges
> shrink."

LinLog: constant attraction `f(x)=1`, repulsion `g(x)=1/x`. Unlike a spring model
(which minimises edge-length *variance* → uniform blob), LinLog provably places
clusters by inverse coupling — exactly the separation we want, expressed in the
force law itself. GPU-parallelisable like any n-body/spring system.

**Annealing (`2-1`)** to escape local minima — vary the repulsion exponent `r`
over `M` steps: `r = r_start (≥2)` for `m < t1·M`; linear blend toward `1` for
`t1·M ≤ m < t2·M`; then `r = 1`. (`0 ≤ t1 < t2 < 1`.) Reached far lower energy than
a LinLog random start in the same iteration count.

Source: van Ham & van Wijk CGF 2008 (reporting Noack).

## Family 2 — fast embeddings as a seed (use as initialisation, not the energy)

- **Spectral / Laplacian eigenmaps (Fiedler)** (`3-0` on *which* eigenvectors):
  positions come from the eigenvectors of the **smallest non-zero** Laplacian
  eigenvalues (2nd-smallest = best axis, 3rd = next, …; the all-ones vector at
  `λ=0` is excluded). Fast, gives clean cluster separation, and is an excellent
  **seed** for force/geometric refinement.
- ⚠️ **Correction (`0-3`, refuted):** `xᵀLx` does **not** equal the force/stress
  edge-length objective — spectral is a *different* energy. Treat it as a seed,
  not "the same as force layout solved exactly."
- ⚠️ **SPLEE + t-SGNE** (arXiv 2310.11186): the headline "300k nodes / 1M edges in
  <5 min" was **refuted (`1-2`)** for lack of corroboration, and "linear time" is
  the authors' self-report. Prefer classical spectral (settled, simple) for the
  seed; revisit t-SGNE only with independent benchmarking.

Source: Koren, *Drawing Graphs by Eigenvectors* (2005).

## Family 4 — focus+context (no surviving verified claims)

Topological fisheye, Tulip/SWViz, semantic/geometric zooming, and the hierarchical
enclosure / 3D Gaussian-density "landscape metaphor" produced **no claims that
survived verification** in this pass. Lean on the repo's existing
`topo_fisheye.rs` and `multilevel`/`coarsen` rather than unsourced detail; treat
formulas/parameters here as open questions.

---

## Implications for this repo (GPU geometric backend) — implementation status

1. ✅ **Edge-strength weighting** — `graph_metrics::compute_edge_strength`
   (`edge_strength.rs`) computes the per-edge Jaccard / Batagelj-corrected-overlap
   metric once (CPU, `O(m·a(G))`); the attribute resolver
   (`graph-api/src/attribute_resolver.rs`) maps it to the injected `edge_len`
   vector via `EdgeStrength::to_rest_lengths`; the geometric (GPU) engine consumes
   it as per-edge `target_len`. No new wire fields or GPU buffers. Exposed as the
   `EdgeLengthLens::JaccardStrength` / `CorrectedOverlapStrength` lens options +
   the `edge_strength_spread` knob, and bundled into the "Separate communities"
   preset. *(Family 1.)*
2. ✅ **Spectral (Fiedler) seed** — `SpectralLayout` static layout
   (`graph-layouts/.../algorithms/spectral.rs`): deflated power iteration on
   `B = cI − L` (no eigensolver dep), registered as the `spectral` layout. Produces
   cluster-separated initial positions; follow with a geometric layout to refine.
   *(Family 2 — seed, not energy.)*
3. ✅ **Hierarchical layout mode** — the `use_multilevel` lens toggle routes the
   subscription through the `MultilevelEngine` wrapper with the geometric (GPU)
   engine as `inner` (coarsen → solve → prolong → refine; Walshaw/FM³/sfdp).
   Community separation itself comes from `ClassLens::Louvain → node_class →
   class_affinity` on the flat path. *Limitation:* injected attributes apply on the
   flat path only — the cascade currently runs on topology (attribute coarsening is
   a follow-up, plan-doc Phase F).
4. 🔧 **Server gate fix (load-bearing):** `graph_layout_stream` previously resolved
   the lens only for `layout_id == "geometric"`, so the GPU backend
   (`geometric-gpu`, what the renderer sends with GPU on) silently received **no
   injected attributes**. Now both backends (and the multilevel promotion) resolve
   the lens — without this, items 1 and 3 never reach the GPU engine.
5. ⏭️ **LinLog mode** (Family 3, Noack — constant attraction + `1/x` repulsion,
   annealed exponent) is the highest-value *not-yet-implemented* follow-up: a
   force-law toggle in the geometric engine + WGSL shader. The most directly
   validated cure to the force model itself.

## Open questions

- Best way to feed continuous strength into a stress-majorisation engine (rest
  length vs. attraction multiplier vs. shortest-path-distance modifier) — which
  gives the cleanest separation in practice?
- Relative GPU cost on this codebase's target sizes: triangle/quadrangle
  enumeration vs. eigensolve vs. t-SGNE neighbour structures.
- Implementable Family-4 detail (fisheye / landscape) — unsourced after
  verification.
</content>
</invoke>
