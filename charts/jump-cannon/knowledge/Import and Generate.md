---
doctype: guide
area: data
audience: [user, developer, operator]
status: current
tags: [jump-cannon, importer, generation]
---

# Import and Generate

The default source is an Obsidian-style markdown vault. The importer runtime can
also load bounded Kubernetes metadata or an Open Knowledge Format bundle.

Generate evaluates supported Nix expressions through tvix and creates a
browser-owned graph. Source selection and credentials remain deployment policy.
See [[Importer Runtime]], [[Backend API]], and [[Security Model]].
