---
doctype: architecture
area: data
audience: [developer, operator, agent]
status: current
tags: [jump-cannon, importer]
---

# Importer Runtime

Importers acquire records, map them into a graph and discovery documents, and
publish one complete revision. The eight server source kinds are Obsidian,
tvix, generate, Kubernetes, OKF, a trusted administrator-installed Pest
package, GitHub, and httpjson (the engine name). Every JSON API — including
Hindsight — is a declarative package under `charts/jump-cannon/packages/`
bound to an instance at runtime via the `JUMP_CANNON_IMPORTER_*` env vars;
Hindsight is the package `hindsight-memory-bank.toml`, not a source kind.
See [[Hindsight Importer]] and AGENTS.md "Importers: packages, not crates".
GitHub delivers a repository tarball over HTTP with ETag polling and reuses
the Obsidian markdown pipeline; see [[GitHub Importer]]. OKF implements the
official format version 0.2; its `0.2` version must not be called `0.0.2`.

Every importer descriptor must supply discovery schema version 2. It declares
input media types, typed search/facet fields, edge semantics, and content
capabilities. `id`, `title`, and `tags` are required searchable fields, and
`tags` must also be facetable so generic clients can build bulk tag navigation
without one metadata request per node. Every schema must also declare the
application-wide `tag_hierarchy` contract with `/` as its separator. This is
mandatory for all importers, not an optional field capability: single-segment
tags remain roots and slash-delimited tags form nested paths. Empty path
segments are invalid. Each successful import must emit exactly one typed
`SearchDocument` for every graph node, using only fields declared by its
schema. The host rejects missing,
duplicate, unknown, mistyped, oversized, or capability-inconsistent output.
Core document identity, title, and unique tags must match the canonical node;
sensitive values cannot enter discovery documents.

Connector-backed pipelines require at least one declared input media type and
validate each acquired record's content type against that declaration before
decoder code runs. Media type matching is case-insensitive and accepts source
parameters such as a charset. Compatibility loaders that read inputs directly
remain responsible for honoring their own declared format until they move onto
the connector record boundary.

graph-api builds the in-memory search index and filter facets from those
validated documents. The graph, schema, search index, facets, metrics, and
binary caches share one atomic snapshot revision, so a failed rebuild leaves
the prior complete revision active. `GET /graph/schema` is the client-visible
contract for the active source. See [[Nodes Search and Documents]] and
[[Backend API]].

Helm can declare named source instances under `importers.sources` and select
one with `importers.selected`. graph-api validates the bounded catalog against
the source that actually started and exposes only its sanitized form through
`GET /importers`. The rollout-based selection remains the deployment default;
when `importers.runtimeSwitchGroup` is set, viewers in that NetBird group can
also switch the viewed source per browser session from the Settings tab, with
writes and compute pinned to the deployment-selected source. The viewer's
selection lives in sessionStorage as the **bare string** `jc_source_id` (no
JSON encoding — the harness and proxy tooling plant and read it through the
DOM storage API), rides as the `x-jump-cannon-source` request header, and as
`?source=` on the layout WebSocket, whose browser API cannot set headers.

The default markdown loader resolves wikilinks and is currently the only
importer that advertises readable and writable source content. Kubernetes
queries are explicit, bounded, metadata-only, and namespace-scoped by default.
OKF loads a filesystem bundle under a stable source identity. Pest manifest
format 2 requires package authors to declare every property that can enter
search or facets. GitHub reads a polled repository tarball and produces the
same node IDs as Obsidian mode for the same corpus. The httpjson engine
binds an instance to one HTTP/JSON API per `JUMP_CANNON_IMPORTER_*` env
var and reads one selected Hindsight memory bank read-only; bounds and
record caps are loud per collection (see [[Hindsight Importer]]).

Do not hide network access, credentials, or authorization inside a pure mapper.
Deployment owns those effects through [[Helm Deployment]] and [[Security Model]].

The Lavender OKF profile's data path is pull-based: lavender-ingest git-pushes
its OKF repository to the private `schrodinger/lavender-okf` repo nightly, the
chart-managed okf-sync CronJob (`okfSync.enabled`) fast-forward pulls that repo
into the shared claim, and graph-api's periodic filesystem rescan
(`filesystemRescanIntervalSeconds`, 60 seconds for the shipped profile)
rebuilds the snapshot from the updated bundle. Reload publication stays atomic:
a failed pull or an invalid bundle leaves the last good graph active.
