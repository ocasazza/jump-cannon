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
  distance. The roadmap and references live in
  [[docs/research/computational-cameras]].

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

The Layout tab's parameter surface is being redesigned around capability-honest
regimes: named YAML regimes resolve from the loaded graph, engine capability
manifests decide which controls exist, and data-owned simulation dimensions
(UFF-typed bonds/atoms) render as provenance capsules instead of live knobs.
The measured defects it fixes (dead `spring_len` slider on typed graphs, the
false "sliders scale on top" banner, vault presets offered to molecular
geometry), the registry schema, and the kaizen phasing live in
`docs/layout-ux.md`; the engine truth it binds to (typed rests win outright,
repulsion mixes via √(wᵢ×wⱼ), `seed_mode: none` keeps authored coordinates) is
in `docs/molecular-force-layout.md`.

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
