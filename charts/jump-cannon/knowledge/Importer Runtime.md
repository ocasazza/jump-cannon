---
doctype: architecture
area: data
audience: [developer, operator, agent]
status: current
tags: [jump-cannon, importer]
---

# Importer Runtime

Importers acquire records, map them into a graph and discovery documents, and
publish one complete revision. The server source kinds are Obsidian,
Kubernetes, OKF, a trusted administrator-installed Pest package, GitHub, and
two package engines: `httpjson` (paged JSON APIs) and `tvix` (Nix-expression
graph generators). Graph generators are no longer compiled CLI sources: the
retired `generate`/`tvix` `--source` kinds are now `engine = "tvix"` packages
that bind their expression's runtime parameters (node count, edge count, seed,
cluster count, affinity) at apply time, exactly as `httpjson` binds its
variables — see `charts/jump-cannon/packages/generate-random.toml` and
`generate-clusters.toml`. Every JSON API — including Hindsight — is a
declarative package under `charts/jump-cannon/packages/`
bound to an instance at runtime via the `JUMP_CANNON_IMPORTER_*` env vars;
Hindsight is the package `hindsight-memory-bank.toml`, not a source kind.
See [[Hindsight Importer]] and AGENTS.md "Importers: packages, not crates".
GitHub delivers a repository tarball over HTTP with ETag polling and reuses
the Obsidian markdown pipeline; see [[GitHub Importer]]. OKF implements the
official format version 0.2; its `0.2` version must not be called `0.0.2`.

A package is exactly one `format_version = 3` TOML file. A Nix serialization
of the same manifest, evaluated server-side by the tvix evaluator, was tried
and superseded: one authored format keeps the editor, the ConfigMap glob
(`packages/*.toml`), and validation single-pathed, and the shared envelope
already carries what the `let`-bound Nix form was collapsing. `.nix` package
files are still not loaded: the tvix engine's generator is an inline TOML
`expr` field — a function of the declared variables — evaluated by
`tvix_wasm::eval_graph` against the embedded `graph.nix` library. The browser
Generate panel uses the same evaluator on the client.

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

Generic byte acquisition lives in `crates/importer-connectors`: `https`
(reqwest natively, gloo-net in the browser), `ssh` and `grpc` (native only;
the gRPC connector invokes unary and server-streaming methods dynamically
from a descriptor set or server reflection), plus pure-Rust `envelope`
expansion of tar/tar.gz/zip/gzip payloads into one record per entry. Each
connector declares its exact capability scope and enforces byte, entry, and
message bounds at acquisition. Tokens and key paths are runtime
configuration — never package fields — and are redacted from `Debug`.

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
also switch the viewed source per browser session from the Importers panel,
writes and compute pinned to the deployment-selected source. The viewer's
selection lives in sessionStorage as the **bare string** `jc_source_id` (no
JSON encoding — the harness and proxy tooling plant and read it through the
DOM storage API), rides as the `x-jump-cannon-source` request header, and as
`?source=` on the layout WebSocket, whose browser API cannot set headers.
## Parameterised sources

A catalog source may declare **parameters** — instance-level variables whose
values are bound at apply time rather than at rollout. Parameters are discoverable
through `GET /importers/sources/{source-id}/parameters`, which returns live
(bounded, cached 60 s) discovered values or falls back to static lists. The
catalog schema for an httpjson source gains:

```yaml
parameters:                                    # optional
  bank:                                        # parameter name (must match a package variable without a default)
    label: Memory bank                         # UI label
    default: omp                               # optional; omitted = parameter is required
    values: ["omp", "jira-ithelp"]             # optional static list (overridden by discover)
    discover:                                  # optional live discovery
      path: /v1/{tenant}/banks
      items_pointer: /banks
      id_pointer: /bank_id
      label_pointer: /name
```

When a user selects a source with parameters, the frontend picks values and
encodes them as a **selection string**: `<source-id>` or
`<source-id>?<k1>=<v1>&<k2>=<v2>` (params sorted by key, URL-encoded, only
declared names). The string is stored in sessionStorage and sent as
`x-jump-cannon-source` on every request. graph-api treats
`(source-id, params)` as a distinct build target: `POST
/importers/sources/{selection}/build` starts a dedicated importer task with
those parameters bound, and status/progress/retry routes accept the full
selection string as `{id}` (URL-encoded). Each parameterised selection's graph
and schema are cached per source id.


Package definitions are editable through the same gate. `GET
/importers/{id}/definition` returns an httpjson source's authored TOML
(`package`, `source`, `writable`) with the catalog's read posture;
`PUT /importers/{id}/definition` and `POST /importers` require the caller's
groups header to contain `importers.runtimeSwitchGroup` (403 otherwise, and
always 403 when switching is disabled). Writes validate first (400 with the
validator's message verbatim), then land atomically in
`JUMP_CANNON_IMPORTER_PACKAGES_DIR` (409 when that directory is read-only),
and drop the cached alternate so the next switch rebuilds from the new file;
the deployment default's own importer keeps the package it loaded at boot.
`POST /importers` also appends the new source to
`<packages_dir>/catalog.local.json`, a runtime overlay graph-api merges into
the chart catalog at boot (entries carry `origin: runtime`; an overlay that
fails validation or shadows a chart id is logged and ignored). The Importers
panel is the surface: selecting a catalog entry offers "Edit server package"
(the Monaco TOML/grammar editor over the served text, then "Save to server"),
and "+ New source" drives `POST /importers`. Browser-local packages in the
same panel never leave localStorage, and the grammar preview always runs in
the sandbox Web Worker — the server never parses a sample input.
Alternate sources selected through the runtime-switch gate are built on a background task. Each importer engine reports progress through `data_loader::ImportProgress` (stage / advance(fraction, detail) / finish / fail / log): the JSON engine emits one stage per collection (`Fetching <collection> from <host>`) and advances after every page with `page N · R records · B MB` (fraction reported only when the collection declares a server total); the pipeline emits `Decoding <n> records` and `Projecting graph`; the pest engine emits `Parsing <package>`. Graph routes for a building selection answer `202 Accepted` with status, elapsed time, stage, detail, and fraction (when available) instead of blocking behind a lock. See [[Backend API]] for the response contract and status/progress/retry endpoints. Building entries persist until eviction or completion; failed builds are retryable and evict on idle TTL. **Operational note:** live-paged APIs like ChEMBL (`chembl-pharmacology`) measure around 4.5 minutes per build on the cluster and evict after ~15 minutes idle, so a later visit pays the import cost again.

The default markdown loader resolves wikilinks and is currently the only
importer that advertises readable and writable source content. Kubernetes
queries are explicit, bounded, metadata-only, and namespace-scoped by default.
OKF loads a filesystem bundle under a stable source identity. Importer package
format 3 wraps both runtime engines (`crates/importer`): a shared
`[metadata]`/`[limits]`/`[schema.fields]` envelope plus `[parser] engine =
"pest"` (inline grammar + capture map) or `"json"` (endpoints + projection).
Package authors must declare every property that can enter search or facets.
GitHub reads a polled repository tarball and produces the
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
