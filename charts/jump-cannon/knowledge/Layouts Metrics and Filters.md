---
doctype: guide
area: product
audience: [user, developer]
status: current
tags: [jump-cannon, layout, metrics]
---

# Layouts, Metrics, and Filters

Settings groups persistent graph configuration and deployment discovery into
five tabs:

- **Connection** selects graph-api and summarizes the active graph session.
- **Importers** reports the active importer and the sanitized, deployment-owned
  source catalog; switching sources remains a Helm rollout.
- **Layout** chooses the simulation engine, starting positions, solver
  parameters, and playback behavior.
- **Appearance** maps graph attributes to size, shape, and color.
- **Camera** controls navigation, fitting, focus, and depth effects.

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
