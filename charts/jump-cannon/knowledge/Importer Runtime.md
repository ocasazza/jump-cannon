---
doctype: architecture
area: data
audience: [developer, operator, agent]
status: current
tags: [jump-cannon, importer]
---

# Importer Runtime

Importers acquire records, map them into a graph and discovery documents, and
publish one complete revision. The six server source kinds are Obsidian, tvix,
generate, Kubernetes, OKF, and a trusted administrator-installed Pest package.
OKF implements the official format version 0.2; its `0.2` version must not be
called `0.0.2`.

Every importer descriptor must supply discovery schema version 1. It declares
input media types, typed search/facet fields, edge semantics, and content
capabilities. `id`, `title`, and `tags` are required searchable fields, and
`tags` must also be facetable so generic clients can build bulk tag navigation
without one metadata request per node. Each successful import must emit exactly
one typed `SearchDocument` for every graph node, using only fields declared by
its schema. The host rejects missing,
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

The default markdown loader resolves wikilinks and is currently the only
importer that advertises readable and writable source content. Kubernetes
queries are explicit, bounded, metadata-only, and namespace-scoped by default.
OKF loads a filesystem bundle under a stable source identity. Pest manifest
format 2 requires package authors to declare every property that can enter
search or facets.

Do not hide network access, credentials, or authorization inside a pure mapper.
Deployment owns those effects through [[Helm Deployment]] and [[Security Model]].
