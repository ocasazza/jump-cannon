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
time. Tvix packages may declare `[[schema.fields]]`; matching flattened node
metadata is type-checked, indexed or faceted as declared, and retained as node
frontmatter. Metadata `title`, `tags`, `doctype`, and `path` populate their
canonical node fields. The same metadata survives the browser Generate panel's
`toGraphJSON` round trip. Every server importer publishes its search and facet
keys through `GET /graph/schema`.

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

Generated graphs can tune the in-browser GPU force layout per node and per
link. A node's `charge` multiplies its repulsion and `mass` its inertia; a
link's `weight` multiplies its spring stiffness and `restLength` its rest
length. Each defaults to 1 on top of the engine's degree-based default. The
"Communities" Generate demos and the "Generated communities · physics regime"
example session show both.

Metric-driven styling works on generated graphs too: the browser computes the
same metric set graph-api serves (community, pagerank, degree, …) on the
client-owned topology, with betweenness skipped above 10k nodes.