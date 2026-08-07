---
doctype: guide
area: product
audience: [user, developer]
status: current
tags: [jump-cannon, search, editing]
---

# Nodes, Search, and Documents

The Nodes panel combines fuzzy file lookup, full-text search, and metadata
filters. Selecting a node fills Inspector and Document. Saving a Document sends
`PUT /vault/page`; graph-api preserves YAML frontmatter and the vault watcher
reloads the graph.

Server-hosted graphs are editable. Browser-generated graphs are intentionally
client-only. See [[Backend API]], [[Importer Runtime]], and [[Workspace]].
