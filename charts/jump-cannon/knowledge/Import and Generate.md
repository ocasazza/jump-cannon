---
doctype: guide
area: data
audience: [user, developer, operator]
status: current
tags: [jump-cannon, importer, generation]
---

# Import and Generate

The default source is an Obsidian-style markdown vault. The importer runtime can
also evaluate a tvix graph, create a server-side generated graph, load bounded
Kubernetes metadata, import an Open Knowledge Format v0.2 bundle, or parse a
trusted Pest manifest-format-2 package. Every server importer publishes its
search and facet keys through `GET /graph/schema`.

Generate evaluates supported Nix expressions through tvix and creates a
browser-owned graph. Source selection and credentials remain deployment policy.
See [[Importer Runtime]], [[Backend API]], and [[Security Model]].
