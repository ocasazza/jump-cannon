---
doctype: guide
area: data
audience: [user, developer, operator]
status: current
tags: [jump-cannon, importer, generation]
---

# Import and Generate

The default source is an Obsidian-style markdown vault. The importer runtime can
also load bounded Kubernetes metadata, import an Open Knowledge Format v0.2
bundle, parse a trusted Pest package (importer package format 3), pull a GitHub
repository tarball (see [[GitHub Importer]]), bind an `httpjson` engine to one
declarative TOML package under `charts/jump-cannon/packages/` (Hindsight ships
as `hindsight-memory-bank.toml`; see [[Hindsight Importer]]), or run an
`engine = "tvix"` generator package that evaluates a parameterised Nix
expression into a graph (`generate-random.toml`, `generate-clusters.toml`) with
its node count, edge count, seed, cluster count, and affinity bound at apply
time. Every server importer publishes its search and facet keys through
`GET /graph/schema`.

Open the **Importers** panel to see the active importer and the sanitized
deployment catalog. Named source instances show their kind, source identity,
filesystem claim/path, and read-only state. When switching to a source that requires building, the boot skeleton, the Graph panel overlay, the Importers panel, and the Progress panel ("Importing <source>") all show the current stage, detail, elapsed time, and a progress bar (determinate when known, indeterminate otherwise) plus a live feed of recent events. Building is never logged as an error or warning. A source build that exceeds a minute is normal for live-paged APIs; if a build fails, a Retry button appears in the Importers panel. For deployment management, select or reconfigure a source with Helm and roll graph-api out.

The built-in `lavender-ingest-okf` profile reads the shared OKF handoff described
in [[Helm Deployment]].

Server-side graph generators are `engine = "tvix"` packages (above), selected
through the importer catalog rather than a `--source` flag. The browser Generate
panel evaluates supported Nix expressions through tvix on the client and creates
a browser-owned graph. Source selection and credentials remain deployment policy.
See [[Importer Runtime]], [[Backend API]], and [[Security Model]].
