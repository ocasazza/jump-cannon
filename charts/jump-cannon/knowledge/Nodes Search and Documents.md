---
doctype: guide
area: product
audience: [user, developer]
status: current
tags: [jump-cannon, search, editing]
---

# Nodes, Search, and Documents

The Nodes panel searches the active server importer's validated discovery
documents. Search is source-neutral and supports the field-qualified keys
declared by `GET /graph/schema`; snippets, boosts, and metadata-filter facets
also come from that schema. Obsidian, tvix, generate, Kubernetes, OKF, and Pest
therefore use the same graph-api search path rather than a title-only fallback.
The panel shows the active field-qualified keys and reports invalid query or
schema errors inline.

Selecting a node fills Inspector and, when the source advertises readable
content, Document. The current Obsidian importer is the only built-in with
readable and writable source content. Saving its Document sends
`PUT /vault/page`; graph-api preserves YAML frontmatter and the vault watcher
reloads the complete graph/search snapshot. Other server-hosted sources are
read-only until their importer schema and connector explicitly implement
content access.

Graphs created inside the browser remain client-only and are separate from the
server-side `generate` importer. See [[Backend API]], [[Importer Runtime]], and
[[Workspace]].
