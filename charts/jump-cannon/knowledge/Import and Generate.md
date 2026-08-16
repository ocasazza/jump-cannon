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
Kubernetes metadata, import an Open Knowledge Format v0.2 bundle, parse a
trusted Pest manifest-format-2 package, pull a GitHub repository tarball
(see [[GitHub Importer]]), or publish one Hindsight memory bank
(see [[Hindsight Importer]]). Every server importer publishes its
search and facet keys through `GET /graph/schema`.

Open **Settings > Importers** to see the active importer and the sanitized
deployment catalog. Named source instances show their kind, source identity,
filesystem claim/path, and read-only state. The panel is intentionally
non-mutating: select or reconfigure a source with Helm and roll graph-api out.
The built-in `lavender-ingest-okf` profile reads the shared OKF handoff described
in [[Helm Deployment]].

Generate evaluates supported Nix expressions through tvix and creates a
browser-owned graph. Source selection and credentials remain deployment policy.
See [[Importer Runtime]], [[Backend API]], and [[Security Model]].
