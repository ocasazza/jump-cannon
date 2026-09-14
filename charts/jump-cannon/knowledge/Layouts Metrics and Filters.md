---
doctype: guide
area: product
audience: [user, developer]
status: current
tags: [jump-cannon, layout, metrics]
---

# Layouts, Metrics, and Filters

Settings groups persistent graph configuration and deployment discovery into
four tabs:

- **Connection** selects graph-api and summarizes the active graph session.
- **Layout** chooses the simulation engine, starting positions, solver
  parameters, and playback behavior.
- **Appearance** maps graph attributes to size, shape, and color.
- **Camera** controls navigation, fitting, focus, and the camera-effects
  stack: perspective/orthographic projection with fov, depth of field as a
  view-space focal band (depth + band width, aperture, max CoC), attribute
  focus (defocus driven by node degree or size), depth-cue fog, a clipping
  slab for section views, and named saved views that persist across
  sessions. Canvas keybindings: **F** fits to graph bounds, **C** toggles
  follow-centroid, **⇧C** snaps to the centroid once at the current
  distance. The roadmap and references live in the repo at
  `docs/research/computational-cameras.md` (not shipped in the vault).

Importer packages moved to the standalone **Importers** panel: it lists the
sanitized deployment catalog alongside browser-local TOML packages, edits
manifests and pest grammars in Monaco, and previews parses in a sandboxed Web
Worker. Switching the server source remains a Helm rollout (or the gated
runtime view described in [[Helm Deployment]]).

The Layout tab's header carries a **This Device / Compute Cluster** switch that
selects which engine gallery the tab shows; engines are rich cards (kind,
processor, description) that activate on one click. The Compute Cluster
segment's dot reports [[Compute]] worker health, and browsing one backend while
an engine from the other is running surfaces a banner that jumps back to the
running engine's gallery.

The command palette's existing Go to Layout, Go to Style, and Go to Camera
actions open Settings on the corresponding tab. Existing Layout, Style, and
Camera values keep their independent localStorage keys, so consolidating the
surface does not reset graph configuration. Workspace layouts from the prior
versions migrate to one Settings panel and preserve the frontmost visible
configuration panel's geometry and state.

Metrics and Filter remain standalone tools: Metrics evaluates the current
result, while [[Filter Builder]] composes repeatable search and metadata rules
inside nested Match all, Match any, and Exclude groups. It can hide non-matches
or keep context dimmed, and reports live evidence for each subexpression.
Generate, Instances, Timeline, and Debug also remain separate because they are
active workflows rather than persistent configuration.

The default renderer runs `graph-layouts` in the browser. Larger or shared work
can use [[Compute]]. Treat layout speed, readability, and interaction latency as
separate measurements under [[Performance Engineering]].

## GPU force engine controls

The in-browser `gpu-force` engine exposes two independent choices on the
Layout tab:

- **Force model** — *Spring-electrical* (Hooke springs + Coulomb repulsion,
  the historical look) or *t-FDP* (Student-t forces: bounded short-range
  repulsion, tighter clusters, clearer inter-cluster gaps). t-FDP adds the
  α / β / γ sliders; the defaults (0.1 / 8 / 2) are the published ones.
- **Repulsion backend** — *Exact* (every pair, O(n²): tiny graphs and
  reference comparisons), *Barnes-Hut* (default; octree, best on clustered
  vaults), or *Negative sampling* (K random partners per node per step; the
  only backend whose cost does not depend on how clustered the layout is, so
  it is the large-graph path). With t-FDP, negative sampling weights each
  sampled pair by the endpoints' degrees and exposes a *k weight* slider —
  the SNAP-tFDP estimator, which reproduces the degree-weighted t-FDP
  objective in expectation.

**Barnes-Hut tree construction is now fully GPU-accelerated.** The tree builds entirely on the device in each layout step: 30-bit Morton-key indexing and an 8-pass 4-bit LSD radix sort with multi-workgroup exclusive scan produce canonical tree order; prefix sums of mass and mass-weighted position cache every node's center of mass as a difference of two prefix entries (no atomics, no CPU tree traversal). The pipeline emits DFS-order nodes with next/skip ropes for fast traversal. Zero host readback is needed per step except for position export to the renderer.

**GPU multilevel seed for large graphs.** Graphs with more than 10,000 nodes automatically start from a multilevel seed computed entirely on device: heavy-edge matching builds a hierarchy, the coarsest level is laid out in a ball, and positions are progressively prolonged down the hierarchy with adaptive step counts. This eliminates the slow global-untangling phase and reduces steps to convergence from minutes to seconds on million-node graphs.

A persisted backend or force-model value the current build does not recognise
falls back to the default rather than to the exact O(n²) path. The scale
limits of this engine and the planned steps beyond them are tracked in
`docs/layout-algorithms.md` §"Scale ladder".

## Region map

The **Regions** section in the Style panel controls an aggregate visualization mode for graphs too large to render node-by-node. Each region is a color-filled Voronoi cell, and cells are indexed by the active `community` metric (often a clustering or partition algorithm output like Louvain modularity).

**Mode**: Select *Off* (disabled), *Underlay* (render regions under edges and nodes), or *Only* (regions replace the graph; all edges and node labels disappear and only cell outlines remain).

**Radius**: Cell size in pixels (4–128). Larger radii merge nearby cells; smaller radii fine-grain the view.

**Fill opacity**: The transparency of the cell color fill (0–1). Set to 0 for outline-only regions; increase toward 1 for opaque colored areas.

**Show outlines**: When enabled, the boundary of each Voronoi cell is darkened where the 4-neighbor cluster ID differs, making region edges stand out.
**Level**: When the graph has a multilevel community hierarchy (e.g. from Louvain), this select controls which level to display. *Auto* adjusts the level based on camera zoom: coarser (level 0) at zoomed-out views and progressively finer levels as you zoom in. Manual options range from 0 (coarsest communities) to L-1 (finest). A live readout shows the current level as "level k of L".


Per frame, the seed pass projects the graph's current positions through the camera and writes the nearest cluster ID into a 512×512 screen-space grid. Ten jump-flood passes (distance steps 256, 128, …, 2, 1) compute the nearest-neighbor Voronoi. Cells farther than the *Radius* setting from any seed become transparent. A fullscreen draw then fills each cell with a color from the current palette (rotated by `id % palette.length()`) at the *Fill opacity*, and stroking outlines where neighbors differ. The entire view recomputes each frame and costs nothing beyond a single O(n) camera-space projection pass, regardless of how many nodes the graph has — making it the only view viable for 10⁷+ node layouts.
