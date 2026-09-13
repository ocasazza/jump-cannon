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
**Phases 1–4 have landed.** The registry is six YAML regimes under
`app/configs/regimes/`: `molecular-uff`, `vault-large`, `vault-small`
(catch-all), and the migrated `fast` / `balanced` / `pretty` presets
(`auto: false` — picker-only, so they never hijack automatic resolution).
Resolution runs on every graph load from the `typed_bond_coverage`, node-bound,
and `engine_kind` predicates; a UFF-typed molecule boots into `Molecular · UFF`
with no `?config=`. The dead `spring_len` slider is gone at full typed coverage
(`25/25 rests from UFF · override ▸` capsule instead), the false "sliders scale
on top" banner is deleted, vault presets are quarantined from typed graphs, and
the repulsion backend enum only appears at n ≥ 500.

The Layout tab now carries a **regime picker** (`auto` plus every applicable
regime) instead of a preset row. Control edits are stored as *overrides against
the resolved base* in `jc_layout_v2` — multipliers where the base is a nonzero
number, absolutes for enums and toggles — so a registry YAML edit reaches every
user who left that control alone, and no absolute value crosses a regime
boundary. Legacy `jc_layout_v1` absolutes migrate once (divided by the
`vault-large` base). An override on a dimension the loaded graph's data owns is
*parked*: retained, surfaced, never applied. `?config=<regime-id>` pins a
regime through the same loader.

Primary controls are **dimensionless intents** declared by the regime, not
absolute physics constants: vault regimes offer Repulsion / Spread / Stiffness
/ Settle (multipliers on the resolved base, with per-field exponents — Settle
raises the halt threshold while easing cooling), and the molecular regime
offers Repulsion (atoms) plus a Keep authored 3D toggle and — by construction
— no geometry-scale knob, because typed rests win outright in the engine. Raw
engine constants live behind `Advanced ▸`, manifest-filtered so a data-owned
dimension appears on no surface, and `why ▸` carries the resolution reason,
typed coverage, quarantined presets, and any parked overrides.

The three measured defects, the registry schema, and the kaizen phasing live in
`docs/layout-ux.md` (normative engineering spec plus implementation-drift
changelog: `docs/layout-ux-spec.md`); the molecular force details (repulsion
mixes via √(wᵢ×wⱼ), `seed_mode: none` keeps authored coordinates) are in
`docs/molecular-force-layout.md`. Remote and static engines are covered too. A graph-compute engine now serves
its own capability manifest over gRPC (`EngineManifest`, projected from the
engine's settings struct so it cannot drift), graph-api exposes it at
`GET /compute/engines/<id>/manifest`, and the panel builds that engine's
controls from the declaration — filtered by the same rules as the local
engine, with manifest-declared values riding the existing `params` bag on
`PUT /compute/layout`. An engine that declares nothing keeps the honest
`settings as declared by <engine> — applicability unknown` header. One-shot
solvers (`execution: one_shot`, e.g. the `fcose-quality` regime) render the
regime surface, a declared quality choice, and a `last solved` line, with no
live-sim rows at all.

Backlog (needs its own justification): a global geometry scale over typed
rests — an engine change, not a UI one — and generating the gpu-force
manifest from the options struct instead of hand-checking it.

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
