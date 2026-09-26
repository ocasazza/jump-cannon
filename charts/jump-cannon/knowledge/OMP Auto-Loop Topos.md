# OMP Auto-Loop Topos

> The **topos** of a jump-cannon view is the transformation applied to the
> generic canvas that maps it onto a specific regime: which node kinds
> exist, what facets drive arrangement and color, and which panels carry
> which slices. This note defines the topos for the omp auto-loop (the
> port of prime-agent's internal autonomous loop) and is the companion to
> [`omp-auto-loop.toml`](../packages/omp-auto-loop.toml).

## Source

One Pest package over the extension's line projection
(`~/.local/state/omp-auto-loop/graph.lines`):

| Node kind | id | tags | properties | body |
|---|---|---|---|---|
| `session` | `s<omp session id>` | `active-goal` / `no-goal`, `main` / `sub`, last outcome | `cwd`, `continuations`, `max_continuations`, `heartbeats`, `model` | the goal's objective (absent without one) |
| `repo` | `r<hash(shared git dir)>` | repo name | `root` | — |
| `goal` | `g<hash(objective)>` | `active` / `paused` / `complete` | — | the objective |
| `gate` | `c<hash(command)>` | — | — | the verifier command |
| `event` | `e<session id>_<n>` | `continue` / `gate` / `goal` / `settled` / `other` | `ts`, `sess` | the full event message |

Edges (untyped, per the vault contract — the endpoint kinds carry the
meaning): `session -> repo`, `session -> goal`, `session -> gate`,
`session -> session` (parent spawned subagent), `session -> event`, and
`event -> event` (the session's timeline, in order).

Why these kinds: they are the shared anchors. Sessions are keyed by omp's
session id, so a restarted or resumed session stays one hub; worktrees of one
repo resolve to the same shared git dir; the same objective hashes to the
same goal. Work on the same thing therefore forms one connected component
instead of one isolated star per process. Heartbeats are a count on the hub,
not nodes — they were a third of all events and carried no information.

The producer supplies data only: bodies are raw content with newlines
collapsed (the grammar's `body_text` is one line), never pre-rendered
markdown. Every presentation choice — what a node click shows, how the
budget reads, colors, arrangement — belongs to this topos. Properties not
declared as `[[schema.fields]]` stay node metadata without entering search.
Every node is upserted in place by id, so shared nodes appear once and always
carry the current state.

Typed edges (`E|src|tgt|kind`) need upstream work first: `VaultEdge` carries
no attributes, so a declared `[[schema.edge_types]]` key never reaches the
canvas.

## The transformation (generic canvas -> loop regime)

1. **Sessions are hubs.** Every `session` node is pinned to the layout's
   outer ring; the force model treats its event fan as a one-level star,
   so a session's history reads as a radial timeline, newest at the rim.
2. **Facet -> geometry.** The `event` tag orders the fan: `goal` seeds at
   the session anchor, `continue` and `gate` interleave by sequence, and
   `settled` terminates the arc — the arc's angular span IS the cycle's
   continuation budget; a session that hit its cap shows a closed ring.
3. **Facet -> color.** `continue` amber, `gate` rose (failure) / green
   (pass — inferred from the title text), `goal` periwinkle, `heartbeat`
   teal, `settled` dim lavender. `no-goal` sessions render at 40% opacity.
4. **Panels.** The static layout is three panels, no floating mode:
   - left: **Sessions** — interactive table over `kind=session` faceted
     by `active-goal`, columns cwd / event count / last event title;
   - bottom: **Stream** — `kind=event` ordered by id, the tail of every
     live session, colored by tag;
   - right: **Cycle detail** — the selected session's event fan as a list
     with budget math (continuations seen vs the 3-per-cycle default).
5. **Search is the regime query.** `tags:continue cwd:nixos-config` answers
   "which work in this repo needed autonomous continuations"; `tags:gate`
   surfaces every verifier intervention.

## Applying this topos (static layout)

The canvas persists layouts as `jc_layout_v1` in localStorage (re-seeded at
boot); this deployment ships
[`omp-auto-loop.layout.json`](omp-auto-loop.layout.json) — a tiling-mode
`SavedLayoutV2` with Graph full-row, Nodes + Inspector half-row, Timeline
and Importers docked. Apply once per browser by pasting the file's JSON:

```js
localStorage.setItem("jc_layout_v1", <omp-auto-loop.layout.json contents>);
```

Then reload — the workspace comes up in tiling mode with the loop regime
pinned; nothing floats.

## Why a topos and not a dashboard

The generic canvas already owns layout, search, and panels; the topos is
only the mapping — data comes from the importer package, arrangement from
the facet rules above, and the panel set from the regime. A new loop
semantic (a new event class) is a new tag in the projection, not a new
view.
