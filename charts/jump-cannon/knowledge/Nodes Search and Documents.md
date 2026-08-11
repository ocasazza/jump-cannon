---
doctype: guide
area: product
audience: [user, developer]
status: current
tags: [jump-cannon, search, editing]
---

# Nodes, Search, and Documents

Nodes is an editor-style workbench. Its left navigator lists nodes and its main
area follows the selected node, showing source identity, path, content state,
frontmatter badges, and any readable body. Select from the navigator or the
Graph; both routes focus the same node. Inspector and Document remain available
as detachable views, while Nodes keeps navigation and content together for the
normal workflow.

With an empty query, switch the navigator between Flat and Tags. Tags groups by
the canonical keyword emitted by the importer: a multi-tagged node appears
under each tag, while nodes without tags appear under the visually distinct
synthetic `(untagged)` group. Discovery schema v1 requires the same `/` tag
hierarchy for every importer. `foo/bar/baz` and `bee/bop/baz` therefore render
as two nested paths and a node carrying both tags appears at both `baz` leaves;
single-segment tags remain at the root. Groups expand lazily and visible lists
are bounded for large graphs; an already selected node remains pinned into view
beyond those bounds. Tag navigation uses the bulk facet snapshot rather than
requesting metadata for every node.

Typing switches the navigator to search results. Search is source-neutral and
supports the field-qualified keys declared by `GET /graph/schema`; snippets,
boosts, and metadata-filter facets also come from that schema. Obsidian, tvix,
generate, Kubernetes, OKF, Pest, and GitHub therefore use the same graph-api
search path rather than a title-only fallback. The toolbar labels this source-neutral
contract as `Search fields` and renders only the active schema's searchable
keys; the importer identity remains metadata rather than user-facing search
terminology. Invalid query or schema errors appear inline.

The current Obsidian importer is the only built-in with readable and writable
source content. Saving from the focused editor sends `PUT /vault/page`;
graph-api preserves YAML frontmatter and the vault watcher reloads the complete
graph/search snapshot. Other server-hosted sources show read-only content or a
truthful metadata-only state until their importer schema and connector
explicitly implement content access.

Graphs created inside the browser remain client-only and are separate from the
server-side `generate` importer. See [[Backend API]], [[Importer Runtime]], and
[[Workspace]].
