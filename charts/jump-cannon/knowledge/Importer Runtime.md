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
The omp auto-loop runtime is the same shape: the pest package
`omp-auto-loop.toml` over the loop's line projection, with its canvas
mapping defined in [[OMP Auto-Loop Topos]].
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
also switch the viewed source per browser session from
the Importers panel, writes and compute pinned to the deployment-selected
source. A group of `"*"` opens switching to every caller. Switching is
non-blocking: the first request selecting an unbuilt runnable source starts
its import as a background task and answers `202 Accepted` + `Retry-After: 2` with a
`building importer source` body; `GET /progress` with the same selection
header serves that build's live event log (never blocking behind the build),
and the panel/graph overlay poll it until the retry succeeds. The viewer's
selection lives in sessionStorage as the **bare string** `jc_source_id` (no
JSON encoding — the harness and proxy tooling plant and read it through the
DOM storage API), rides as the `x-jump-cannon-source` request header, and as
`?source=` on the layout WebSocket, whose browser API cannot set headers.
Clicking a catalog row selects it for the manifest editor; every runnable row
carries its own inline Load action (`[data-action="load-row"]`, `⟲` on the
default row) that applies the source, so loading a graph is one click from the
list while browsing the catalog triggers no server-side imports. The anchored
row, the graph-area overlay, and the Progress panel all show the build's
stages and fractions while it runs.

## Parameterised sources

A package's declared `parser.variables` entries are the single authoritative
parameter list: every package variable is pickable at apply time rather than
only at rollout. `GET /importers/sources/{source-id}/parameters` enumerates
them all, returning live (bounded, cached 60 s) discovered values where a
catalog **parameter** of the same name declares discovery. The catalog's
optional `parameters` map is UI metadata overlaid on package variables by
name — label, pre-selected default, static values, live discovery. The
catalog schema for an httpjson source gains:

```yaml
parameters:                                    # optional
  bank:                                        # parameter name (must match a package variable)
    label: Memory bank                         # UI label
    default: omp                               # optional; omitted = parameter is required
    values: ["omp", "jira-ithelp"]             # optional static list (overridden by discover)
    discover:                                  # optional live discovery
      path: /v1/{tenant}/banks
      items_pointer: /banks
      id_pointer: /bank_id
      label_pointer: /name
```

When a user selects a source, the frontend picks values and encodes them as a
**selection string**: `<source-id>` or `<source-id>?<k1>=<v1>&<k2>=<v2>`
(params sorted by key, URL-encoded, any declared package variable name).
Catalog-declared parameters always ride the string (picked value or
default); package-only variables ride it only when changed from their
default, so an untouched source keeps its historical string. The string is
stored in sessionStorage and sent as
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
fails validation or shadows a chart id is logged and ignored). Instance
variables are mutable through the same gate without touching the package
text: `GET /importers/{id}/variables` returns an httpjson source's declared
parser-variable declarations (name, description, default) plus the instance's
current values, and `PUT /importers/{id}/variables` fully replaces the set —
keys are validated against the package's declarations, the override persists
to `<packages_dir>/variables.local.json` (its own file because the source
overlay rejects shadowing a chart id), the in-memory entry updates, and the
running alternate is dropped so the next request rebuilds with the new
values. Boot applies the variable overrides after the source overlay. In the
panel this is the Variables section on a selected httpjson row: one input
per declared variable, then "Apply & reload" re-applies the source and
streams the rebuild. The Importers
panel is the surface: selecting a catalog entry offers "Edit server package"
(the Monaco TOML/grammar editor over the served text, then "Save to server"),
and "+ New source" drives `POST /importers`. Browser-local packages in the
same panel never leave localStorage, and the grammar preview always runs in
the sandbox Web Worker — the server never parses a sample input. The preview's
"View as graph" mounts the parsed sample as a client-only graph in the
renderer (bounded at 5k nodes / 20k edges), the same mount the Generate panel
uses, so authoring a package shows its graph without any server round-trip.

Alternate sources selected through the runtime-switch gate are built on a background task. Each importer engine reports progress through `data_loader::ImportProgress` (stage / advance(fraction, detail) / finish / fail / log): the JSON engine emits one stage per collection (`Fetching <collection> from <host>`) and advances after every page with `page N · R records · B MB` (fraction reported only when the collection declares a server total); the pipeline emits `Decoding <n> records` and `Projecting graph`; the pest engine emits `Parsing <package>`. Graph routes for a building selection answer `202 Accepted` with status, elapsed time, stage, detail, and fraction (when available) instead of blocking behind a lock. See [[Backend API]] for the response contract and status/progress/retry endpoints. Building entries persist until eviction or completion; failed builds are retryable and evict on idle TTL. **Operational note:** live-paged APIs like ChEMBL (`chembl-pharmacology`) measure around 4.5 minutes per build on the cluster and evict after ~15 minutes idle, so a later visit pays the import cost again.

The default markdown loader resolves wikilinks and advertises readable and
writable source content. A pest package can too: `[parser.content_file]`
binds one markdown file per node beside the input (`nodes/{id}.md`,
`writable = true`), readable when the file exists and editable through the
same `PUT /vault/page` gate — see [[OMP Auto-Loop Topos]]. Kubernetes
queries are explicit, bounded, metadata-only, and namespace-scoped by default.
OKF loads a filesystem bundle under a stable source identity. Importer package
format 3 wraps both runtime engines (`crates/importer`): a shared
`[metadata]`/`[limits]`/`[schema.fields]` envelope plus `[parser] engine =
"pest"` (inline grammar + capture map) or `"json"` (endpoints + projection).
Package authors must declare every property that can enter search or facets.
Edges may carry a kind: a pest package binds the optional `edge_kind`
capture and a json package's edge rules name theirs, in both cases one of the
package's declared `schema.edge_types` keys (an undeclared kind fails the import
naming it; edges without one stay untyped). graph-api serves the kinds beside
the edge buffer (`/graph/edge-kinds` + `/graph/edge-kinds.bin`) and the Style
panel's "Kind (edge type)" edge-color mode renders them — see
[[OMP Auto-Loop Topos]] for the first typed package.
GitHub reads a polled repository tarball and produces the
same node IDs as Obsidian mode for the same corpus. The httpjson engine
binds an instance to one HTTP/JSON API per `JUMP_CANNON_IMPORTER_*` env
var and reads one selected Hindsight memory bank read-only; bounds and
record caps are loud per collection (see [[Hindsight Importer]]).

Poll-driven reloads are gated on real change: `Importer::import` returns
`ImportOutcome`, and an importer that can prove its source is unmodified
answers `Unchanged` — the watcher then keeps the mounted snapshot and emits
nothing (no rebuild, no `/progress` events). The GitHub importer proves it
with the tarball ETag (a warm 304); every connector pipeline (the json and
pest packages) proves it by hashing the fetched records in collection order
before decode/map, so identical API pages never re-map. Edge value pointers
in the json engine may name an array of ids (`/referenced_works`); each
scalar element resolves as one target, matching `split_csv`'s
one-value-per-element rule.

Two of the shipped packages read public science APIs rather than an
in-cluster service: `chembl-pharmacology.toml` (EMBL-EBI ChEMBL — approved
molecules, human protein targets, the mechanism-of-action records that bridge
molecule to target, and drug indications; ~29.6k nodes and ~22.5k edges at
`max_phase = 4`) and `openalex-works.toml` (OpenAlex — one ranked page of a
search scope plus the citation edges among those works; the `count` package
variable sizes the page, default 200, OpenAlex's per-page cap). Both need node
egress to the open internet, so they build only where the pod has it and fail
loudly with the endpoint in the message where it does not. ChEMBL ships some
measurements as numeric strings (`full_mwt` is `"383.41"`), which is why the
json engine has the `parse_number` field transform: a `number` discovery
field gets a number instead of the schema being weakened. Every shipped
package has a parametrized contract test in `crates/importer/tests/packages.rs`
(rstest, one named case per package): recorded API responses run through the
real connector/decoder/mapper pipeline over a fixture transport, asserting
nodes and — whenever the schema declares edge types — edges, so a silent
projection break (the pre-fix array pointer that shipped OpenAlex with zero
citation edges) fails the suite instead of users' graphs.

Shipped example sessions pair a curated UI state with the source it is about:
`app/ui/assets/sessions/*.yaml` plus an `index.json`, copied into the dist by
trunk so every deployment has them (container, `trunk serve`, browser-only
GitHub Pages). The Instances panel lists them; Load applies the app state
(layout regime, style, camera) and then loads the named source through the
same apply path as a manual row Load, so a session on an unbuilt source shows
the build overlay and streams its stages.

Do not hide network access, credentials, or authorization inside a pure mapper.
Deployment owns those effects through [[Helm Deployment]] and [[Security Model]].

The Lavender OKF profile's data path is pull-based: lavender-ingest git-pushes
its OKF repository to the private `schrodinger/lavender-okf` repo nightly, the
chart-managed okf-sync CronJob (`okfSync.enabled`) fast-forward pulls that repo
into the shared claim, and graph-api's periodic filesystem rescan
(`filesystemRescanIntervalSeconds`, 60 seconds for the shipped profile)
rebuilds the snapshot from the updated bundle. Reload publication stays atomic:
a failed pull or an invalid bundle leaves the last good graph active.
