# Importer architecture

Status: **accepted; mandatory discovery/search schema implemented**.

This document defines how jump-cannon stops treating an Obsidian vault as its
data model and instead treats it as one importer. It supersedes the ingest and
plugin-loading discussion in
[`tvix-graph-generation-and-plugins.md`](tvix-graph-generation-and-plugins.md),
whose tvix implementation notes remain useful but whose opening implementation
status is stale.

## Decision

An importer is a pipeline of independent contracts:

```text
host-owned effects       pure transforms                    graph host

SourceConnector ──bytes──► Decoder ──records──► GraphMapper ──ops──► GraphSink
       │                                                               │
       └────────────── optional Encoder/SinkConnector for writes ───────┘
```

- A **source connector** performs effects: filesystem reads, Kubernetes
  list/watch, HTTP requests, gRPC calls, or UDP receive.
- A **decoder** turns an envelope of bytes into records. JSON, YAML, Protobuf,
  CBOR, and a runtime Pest grammar are decoders, not sources.
- A **graph mapper** is pure. It turns records into validated graph operations.
- A **graph sink** is the target ownership boundary: it validates operations
  and atomically publishes a graph snapshot. The compatibility slice returns a
  complete `VaultGraph` plus one typed `SearchDocument` per node. graph-api
  validates both projections, builds search and facet data, and publishes them
  together; failed imports never replace the live snapshot.
- Writes are a separate, opt-in path: a pure encoder produces a mutation and a
  capability-bearing sink connector applies it. Read access never implies write
  access.

This is the functional-effects boundary: importer logic describes the effects
it needs, while the host owns the interpreters, credentials, policy, retries,
and observability for those effects.

## Why a grammar is not the plugin ABI

A Pest grammar is a good decoder asset for custom UTF-8 formats. Pest's normal
derive path compiles a grammar into Rust during the build, but the official
[`pest_meta`](https://github.com/pest-parser/pest/blob/master/meta/src/lib.rs)
and [`pest_vm`](https://github.com/pest-parser/pest/blob/master/vm/src/lib.rs)
crates can validate, optimize, and execute a grammar at runtime.

A grammar alone does not define:

- where bytes come from or where writes go;
- how parse-tree captures become graph nodes and edges;
- how a stream is framed;
- how Protobuf bytes are decoded;
- an inverse serializer (PEGs are not generally invertible); or
- resource limits and permissions.

The initial Pest importer therefore uses named captures as a deliberately small
mapping convention. The durable public boundary is the importer manifest and
graph-operation schema, not Pest's AST or optimized-rule Rust types.

## Extension tiers

### Declarative importer packages

The common path is a versioned package containing data only:

```text
manifest.toml
grammar.pest          # optional, for custom text
map.rhai              # planned pure record -> graph-op mapping
tests/input/*
tests/expected/*
```

The first implementation uses a bounded single-file TOML package with an inline
grammar. Packages contain no credentials and have no ambient I/O. A
source instance separately binds a package to a concrete path, URL, Kubernetes
cluster/namespace/resource allowlist, or socket.

The checked-in example can be run directly:

```bash
JUMP_CANNON_SOURCE=pest \
JUMP_CANNON_IMPORTER_MANIFEST=crates/pest-importer/examples/line-graph.importer.toml \
JUMP_CANNON_IMPORTER_INPUT=crates/pest-importer/examples/line-graph.txt \
nix develop -c cargo run -p graph-api -- --no-browser
```

### Built-in Open Knowledge Format importer

Open Knowledge Format (OKF) is a schema-aware built-in importer, not a runtime
Pest package. It implements the normative Google Cloud Platform
[OKF v0.2 specification](https://github.com/GoogleCloudPlatform/knowledge-catalog/blob/3fcbb9f828c2f23d109c855ee403c3a4c81f3a96/okf/SPEC.md)
(the published version is named `0.2`) with a YAML-frontmatter parser and a
CommonMark link parser. Pest remains useful for trusted,
administrator-installed custom text grammars; it is not a better fit for a
format whose syntax is already YAML plus Markdown.

The OKF graph mapping is deliberately narrow and follows the format rather than
Obsidian conventions:

- each non-reserved `.md` file is a concept; its ID is the suffixless,
  bundle-relative path;
- `index.md` and `log.md` are reserved at every directory level and do not
  become concept nodes;
- every concept has a non-empty frontmatter `type`, while optional `tags` are
  taken only from a YAML list of strings (not inline hashtags, wikilinks,
  singular fields, or comma-split scalar values);
- standard CommonMark links to other concepts create directed edges from the
  referring concept to the target, and an internal `sources[].resource`
  reference creates the same directed derivation relationship;
- external, missing, self, and image links do not create graph edges; and
- unknown string-keyed frontmatter fields are retained as JSON attributes (or
  canonical YAML text when their nested key shape is not JSON-compatible), so
  an importer update is not required for every domain-specific extension.

The root `index.md` may declare `okf_version: "0.2"`. Its absence is valid, and
an unrecognized declared version is imported best-effort rather than rejected,
which keeps discovery forward-compatible without claiming full support for the
newer version. Neither concept content nor Attested Computation material is
executed.

The native in-process parser applies event, node, depth, scalar, anchor, alias,
merge, and replay budgets before and during expansion. Ordinary YAML reuse
works, while amplification patterns are rejected before a small frontmatter
document can grow into unbounded process memory. Input, identifiers,
frontmatter, references, graph endpoints, diagnostics, and the downstream
filter-summary expansion all have aggregate output budgets before snapshot
publication.

A local bundle can be selected through the same administrator-owned server
configuration as the other built-ins:

```bash
JUMP_CANNON_SOURCE=okf \
VAULT_ROOT=/path/to/okf-bundle \
JUMP_CANNON_OKF_SOURCE_ID=my-bundle \
nix develop -c cargo run -p graph-api -- --no-browser
```

The source ID is a stable ASCII slug (`A-Z`, `a-z`, `0-9`, `.`, `_`, `-`) so
source namespaces cannot produce ambiguous node IDs.
The built-in filesystem implementation currently requires a Unix host
(including Linux Kubernetes nodes and macOS development): its component-wise
`openat`/no-follow/nonblocking reads are the security boundary for
ingestion-writable bundles. Other platforms fail closed until an equivalent
confined-open backend is implemented.

The Helm integration supports either a release-owned PVC or a same-namespace
claim supplied through `vault.persistence.existingClaim`, including a claim
owned and written by Lavender Ingest. Graph-api mounts an OKF claim read-only.
`additionalServiceAccounts` can create exact namespace-local identities for
companion ingestion components without inventing RBAC or transferring PVC
ownership. OKF combines filesystem notifications with a 60-second periodic
full-rescan fallback by default because cross-pod writes are not guaranteed to
produce inotify events on every CSI/NFS/RWX implementation. The interval is
configurable and restarts after each completed reload, so a large graph does
not enter an immediate catch-up loop; unsupported notifications degrade to the
periodic driver. Obsidian retains notification-only behavior unless its generic
filesystem rescan setting is explicitly enabled.

### Wasm Component plugins

Code is needed for genuinely new codecs or connector behavior. Those extensions
use a versioned WebAssembly Component Model world expressed in WIT. Components
can interact only through imports granted by the host, which matches the split
between pure transforms and explicit effects described above. See the
Bytecode Alliance's [Component Model rationale](https://component-model.bytecodealliance.org/design/why-component-model.html)
and [WIT overview](https://component-model.bytecodealliance.org/design/wit.html).

The preferred untrusted-upload design is a fixed, trusted Wasm worker component
that contains the Pest and mapping engines. Users upload only grammar, mapping,
manifest, and fixtures. The host applies Wasmtime fuel, epoch deadlines, memory
limits, batch limits, and a process/pod resource boundary.

## Core contracts

The compatibility output remains `vault_data::VaultGraph` while the rest of the
application is migrated. The contract should converge on snapshot and delta
events rather than only a synchronous full load:

```rust
enum SourceEvent {
    SnapshotStart,
    Upsert(Envelope),
    Delete(ObjectKey),
    Checkpoint(Revision),
    SnapshotEnd,
}

struct Envelope {
    source_instance: SourceId,
    object_key: ObjectKey,
    revision: Option<Revision>,
    media_type: String,
    payload: Bytes,
}

enum GraphOp {
    UpsertNode(Node),
    DeleteNode(NodeId),
    UpsertEdge(Edge),
    DeleteEdge(EdgeId),
}
```

The first vertical slice materializes every source into a full graph and reuses
`GraphSnapshot::build`. Adding the event and operation boundaries is the next
core step so Kubernetes watches, UDP streams, and incremental filesystem
updates do not require another ingestion redesign.

Every importer descriptor now has an enforced stable ID, display name,
implementation version, requested capabilities, watch plan, and mandatory
`ImporterSchema`. Discovery schema version 1 describes:

- accepted input media types;
- fields with a logical type (`text`, `keyword`, `keyword_list`, `number`,
  `boolean`, `date`, or `url`) and required, searchable, facetable, snippet,
  boost, default-value, and sensitive flags;
- the key and direction of every edge type the importer can emit; and
- readable/writable source-content capabilities and their media types.

The core `id`, `title`, and `tags` fields are required and searchable for every
importer, with types `keyword`, `text`, and `keyword_list` respectively. A
schema must contain at least one edge type, may declare at most 128 fields, and
cannot mark a sensitive field searchable, facetable, or eligible for snippets.
Writable content implies readable content, and the schema must agree with the
descriptor's content capabilities.

Each completed import emits exactly one `SearchDocument` for every graph node.
The document must reference that node, may contain only declared fields, and
must satisfy the schema's required fields, logical types, defaults, and global
value-size bound. Its `id`, non-empty `title`, and unique `tags` must match the
canonical graph node exactly. Sensitive fields may be declared for explicit
exclusion but cannot enter a discovery document. Unknown source attributes can
remain in node metadata, but they are neither indexed nor faceted until the
importer explicitly declares and projects them. `HostedImporter` validates the
descriptor before execution and the result afterward; `GraphSnapshot::build`
repeats the graph/schema boundary validation before publication.

Search indexing is a pure host-owned derivative of those validated documents,
so every importer receives consistent search without needing network, storage,
or a separate `Search` effect grant.

Connector-backed importers must declare at least one input media type, and
`ImportPipeline` checks every `SourceRecord` against that declaration before
invoking the decoder. Matching is case-insensitive and permits parameters such
as `charset` on the source record; an undeclared media type fails the import
without running parser code. Native compatibility loaders that read their
source directly do not cross this record boundary and remain responsible for
decoding only their declared input format.

### Implemented importer schemas

All six selectable source kinds satisfy the version-1 contract:

| Source | Discovery projection | Edge/content contract |
| --- | --- | --- |
| Obsidian | `id`, `title`, `tags`, `path`, `type`, `folder`, `body`, `description`, `status`, `authors`, `entities`, `key_topics`, `related` | Directed `wikilink`; Markdown content is readable and writable. |
| tvix | `id`, `title`, `tags`, `path`, `type` | Directed `declared` edge; no source-content operations. |
| generate | `id`, `title`, `tags`, `path`, `type` | Directed `generated` edge; no source-content operations. |
| Kubernetes | `id`, `title`, `tags`, `path`, `type`, `namespace`, `api_version`, `labels`, `uid`, `resource_version` | Directed `owner_reference`; metadata discovery does not expose resource bodies as content. |
| OKF | `id`, `title`, `tags`, `path`, `type`, `folder`, `body`, `description`, `resource`, `status`, `stale_after`, `trust_tier`, `generated_by`, `generated_at`, `verified_by`, `source_resources`, `source_titles`, `source_authors`, `runtime` | Directed `relationship`; no source-content operations yet. The normative format version is OKF v0.2. |
| Pest package | Core `id`, `title`, `tags`, `path`, and `type`, plus only the package's declared property fields | Directed `declared` edge; no source-content operations. Package manifest format 2 makes the property schema mandatory. |

The endpoint returns the searchable and facetable flags rather than asking
clients to infer them from this table. Kubernetes `resource_version` is retained
for discovery metadata but intentionally is not searchable; Pest property flags
come from the validated package manifest. Every version-1 importer schema must
publish the application-wide `tag_hierarchy` contract with `/` as its separator.
The host rejects a missing or different separator and rejects tag values with
empty path segments, so all clients can render the required hierarchy without
source-specific inference or fallback behavior.

Pest capture values are strings, so package-defined discovery properties are
limited to text, keyword, date, and URL fields. A package cannot redeclare the
core keys or put a sensitive property in the discovery projection.

### Search and facet publication

graph-api builds a source-neutral, in-memory Tantivy index from the active
importer's schema and documents. Search boosts, field-qualified query keys, and
snippet eligibility all come from that schema. The metadata summary used for
filters is likewise built only from fields marked facetable; it no longer
assumes Obsidian frontmatter keys.

The graph, schema, search index, metadata-facet summary, metrics, and binary
caches belong to one `GraphSnapshot` revision and become visible through one
atomic swap. A failed schema, document, index, or facet build leaves the prior
revision active, and there is no
title-only fallback for non-Obsidian sources.

`GET /graph/schema` returns the current graph revision, active source ID/name/
version, and complete discovery schema. `GET /search` returns matching IDs in
protobuf; `GET /search/rich` returns JSON results with scores and snippets.
Malformed queries or references to fields absent from the active schema return
HTTP 400. Clients should inspect `/graph/schema` before presenting search keys,
facets, or content actions.

Source-instance IDs must namespace node and edge IDs before multiple importers
can be composed into one graph. Kubernetes and OKF do this now; the current
Pest adapter is still a single active source and keeps package-local IDs.
Built-in mappers use fallible node insertion so duplicate IDs are rejected
before an `IndexMap` can overwrite them. Graph-api additionally rejects
dangling edges and non-finite positions before publication. The planned
operation sink must make duplicate and output-limit enforcement unavoidable for
third-party mappers; post-hoc validation of an already materialized map cannot
recover overwritten duplicate provenance.

## Capability model

Capabilities are explicit tuples of effect, transport, and scope. Examples:

```text
read  + filesystem + /data/team-a/**
watch + kubernetes + cluster-a/apps/deployments:namespace-a
read  + http       + https://inventory.example.test/api/**
write + kubernetes + cluster-a/apps/deployments:namespace-a
bind  + udp        + 10.0.0.4:4317
```

A package declares requested capabilities. An administrator or deployment
grants a subset to a source instance. A descriptor is not proof of authority:
`HostedImporter` stores the host-selected exact grant set separately and checks
every declared read before executing importer code; graph-api uses that same set
for change drivers and source-content access. This permits a package with
optional write support to run read-only without turning its declaration into a
grant. The current CLI grants all exact requests only because every selectable
source is compiled in or explicitly installed and bound by an administrator.
The future upload/control-plane path must select a reviewed subset and must not
reuse that trusted-source policy. Secrets are referenced by name and resolved
by the connector; they are never stored in a package, browser localStorage,
exported app state, logs, or diagnostics.

Direct native `pest_vm` execution has input and output caps but no instruction
fuel or safe preemption. It is therefore restricted to trusted,
administrator-installed packages. Network-facing install/run endpoints must not
ship until graph-api has authentication, authorization, request limits, and
tighter CORS.

## Kubernetes-first connector

Kubernetes is the first asynchronous connector because it exercises discovery,
snapshots, watches, stable identity, and least-privilege effects without
coupling those concerns to a decoder.

The compatibility connector uses `kube::Api<DynamicObject>` for allowlisted
resource kinds and polls bounded, paginated snapshots. Configuration rejects
unknown fields, and every query must state `namespaces` explicitly (`[]` means
cluster/all-namespace scope), so an omission or typo cannot silently widen a
selector. Unless `include_object` is explicit, acquisition uses
`Api::list_metadata` and records retain only identity, labels, resource version,
and owner references; annotations, specs, status, and data are not published.
Object count and cumulative serialized source bytes have safe defaults and hard
ceilings. All API pages are kept small because even metadata-only responses can
contain large fields that are discarded only after deserialization.
Each read/watch capability scope encodes the source ID, GVK, namespaces,
selectors, representation, and Secret opt-in, so widening a query changes the
exact grant tuple. Polling skips missed interval ticks rather than replaying a
burst of expensive full imports.
The next incremental phase will replace polling with
`kube_runtime::watcher` list/watch events. It must not watch every discovered
resource. Each source instance explicitly allowlists:

- cluster/source ID;
- group, version, resource, and kind;
- namespace(s);
- label and field selectors; and
- whether full objects or metadata-only envelopes are needed.

Identity is `(source instance, GVR, namespace, UID)`. `resourceVersion` is a
checkpoint, not a globally ordered sequence. Today, every successful poll is
validated and atomically published as a complete snapshot; a failed poll leaves
the prior snapshot live. In the incremental phase, initial watch events will be
buffered through snapshot completion and subsequent apply/delete events will
become graph deltas. Relists repair missed events.

The initial mapper creates resource nodes and `ownerReferences` edges. Later
pure mapping rules can add selector, volume, ingress/backend, RBAC-subject, and
custom-resource relationships without changing the connector.

Cluster deployment keeps service-account token automount disabled by default.
Its optional namespace Role grants only `list` for explicit resources and
rejects wildcard API groups or resources. A future watch-based connector and
all writes require distinct grants. Secrets are excluded by default because
listing them exposes their contents. Follow the Kubernetes
[RBAC good practices](https://kubernetes.io/docs/concepts/security/rbac-good-practices/)
and [watch semantics](https://kubernetes.io/docs/reference/using-api/api-concepts/).

## Control plane and UI

Importer packages and source instances are separate resources.

### Current experience

Loading data is an administrator/deployment action. One active source is
selected when graph-api starts, using CLI flags or environment variables
locally and equivalent server/volume configuration in Helm. The six supported
selections are Obsidian, tvix, generate, Kubernetes, OKF, and Pest. Helm can
also declare named source instances under `importers.sources` and select one
with `importers.selected`; an empty selector preserves the legacy source
settings. The chart currently wires named Obsidian, Kubernetes, and OKF
profiles; it does not advertise tvix or Pest profiles until their required
source-specific inputs can be represented and mounted. The Dioxus app
automatically loads the graph published by that source and the existing
Progress panel reports reload work. Switching sources changes server
configuration and normally restarts the server.

Every server importer publishes its current discovery/search contract at
`/graph/schema`, and the Nodes search endpoint indexes the fields its importer
declares. `GET /importers` publishes a sanitized, read-only view of the active
importer and configured source instances. **Settings > Importers** renders that
catalog, including the selected instance, filesystem claim/path, read-only
state, and operator-facing producer handoff metadata. It deliberately contains
no credentials or Apply, Run, or Activate action: selection remains Helm policy
and requires a rollout. The Nodes panel displays the active searchable keys and
surfaces invalid queries, but the current UI does not yet provide a full
query-builder panel.
OKF exposes nodes, typed discovery fields, frontmatter attributes, tags, and
edges, but deliberately does not advertise source-body read or write
capabilities. Inspector metadata and graph filtering work; the Document body
and editor remain unavailable until content reads are routed through the
importer's bounded, no-follow filesystem boundary instead of graph-api's
Obsidian compatibility reader.

There is no browser package upload, importer marketplace, runtime source
mutation, or browser-owned credential flow. Adding a custom importer today
means an administrator supplies a trusted Pest manifest and input through
explicit paths, normally read-only mounts, or deploys a compiled-in source. The
server validates both the configured catalog and selected source at startup and
fails on an invalid or mismatched selection; it never silently falls back to
Obsidian. The Helm path similarly binds a selected importer to server-side
mounts or Kubernetes access rather than transferring a dataset through the
browser.

### Target experience

The future authenticated control plane will extend the read-only Dioxus
Importers tab while keeping package installation separate from source
configuration:

1. **Importer Library** shows built-ins and installed packages with their
   version, digest, validation state, requested capabilities, trust level, and
   declared search/facet/content contract.
2. **Add Source** selects an importer and binds its declared effects to a
   concrete server-side directory, URL, Kubernetes scope, or other supported
   connector. Secrets remain references resolved by the host.
3. **Preview** validates the package and source without publishing a graph. It
   shows requested effects, diagnostics, counts, and sampled nodes, tags, and
   edges so an administrator can grant an exact reviewed subset.
4. **Run** reports progress through the existing progress experience and
   atomically publishes a validated result. A failed run leaves the last good
   graph snapshot active.

Untrusted user packages require a sandboxed Wasmtime worker with fuel, epoch
deadlines, memory and batch limits, and process or pod isolation. Until that
boundary and graph-api authentication/authorization exist, browser upload and
network-facing install/run operations remain intentionally unavailable.

Current server API:

- `GET /importers` returns the active importer and sanitized configured source
  instances. Its activation mode is `helm_rollout`; no mutation endpoint is
  exposed.

Planned authenticated server API:

- `PUT /importers/:id` validates a bounded single-file package and its fixtures,
  then atomically installs it. This is gated on authentication/authorization.
- `POST /importers/:id/preview` returns counts, sampled nodes/edges,
  diagnostics, requested effects, and a digest without changing the live graph.
- `POST /importers/:id/runs` starts an asynchronous run and returns a run ID.
- `GET /import-runs/:id` reports durable status and diagnostics.

The mutation and run endpoints are a design target, not an implemented public
API. Any future upload surface will reuse the existing Rust file-input pattern
and progress feed rather than expanding Generate. Credentials never enter
browser localStorage or shareable app-state exports.

## Migration plan

### Phase 1: safe compatibility foundation

- [x] Add composable connector, decoder, mapper, descriptor, capability, and
  error contracts to `data-loader` while preserving the current `Loader` API.
- [x] Add a trusted/admin runtime-Pest importer with a versioned manifest,
  canonical captures, validation, limits, and golden tests.
- [x] Add a schema-aware OKF v0.2 filesystem importer with specification-based
  concept IDs, tags, links, provenance edges, and forward-compatible fields.
- [x] Add an async Kubernetes snapshot connector over allowlisted dynamic
  resources, initially read-only.
- [x] Select an importer by stable ID/path without an unknown-source fallback.
- [x] Make watcher startup honor the selected importer instead of always
  watching `$VAULT_ROOT` for Markdown.
- [x] Add a configurable periodic filesystem rescan fallback for cross-pod PVC
  writers while keeping Obsidian's default event-driven behavior.
- [x] Let Helm use either a chart-owned or externally owned PVC and create
  explicitly named companion ingestion ServiceAccounts without implicit RBAC.
- [x] Keep Obsidian, tvix, and generated loaders behavior-compatible.
- [x] Require a versioned discovery schema and one validated search document
  per node from Obsidian, tvix, generate, Kubernetes, OKF, and Pest.
- [x] Build generic Tantivy search and schema-driven facets inside the same
  atomic graph snapshot, and expose the active contract at `/graph/schema`.
- [x] Add a deployment-selected source-instance catalog, sanitized read-only
  `/importers` discovery, and a non-mutating Dioxus Settings tab.

### Phase 2: generic graph ownership

- [x] Introduce source-neutral `Graph`, `Node`, and `Edge` compatibility names.
- [ ] Add first-class `NodeOrigin` and richer `ContentCapabilities` types while
  retaining compatibility aliases.
- [ ] Separate imported attributes from derived metrics and layout positions.
- [ ] Give edges stable IDs, direction, kind, weight, and properties.
- [x] Reject duplicates in built-in mappers and validate representable graph
  invariants at the host boundary.
- [ ] Add source-instance identity and namespace IDs across composed importers;
  Kubernetes already namespaces its IDs, while Pest remains single-source.
- [ ] Make third-party mappers emit through a host-owned operation sink so
  duplicate and output-limit enforcement cannot be bypassed or erased.
- [x] Route search through importer-owned, host-validated discovery documents.
- [ ] Route node content through source adapters. Content metadata is explicit
  and `/vault/page` remains an Obsidian-only compatibility write route.
- [ ] Move JSON/CSV/DOT decoders out of `graph-layouts` into importer codecs.

### Phase 3: incremental runtime and secured library

- [ ] Add source event streams and graph deltas; initially coalesce deltas into
  the existing full snapshot publication path.
- [ ] Add the authenticated importer/source-instance/run control plane.
- [ ] Add authenticated install, preview, and run actions to the read-only
  Dioxus Importers tab and extend its Rust browser regression.
- [ ] Add a read-only multi-package importer-library volume to the Helm chart.
- [x] Add opt-in, namespace-scoped Kubernetes token/RBAC templates; preserve
  the secure defaults.

### Phase 4: sandboxed extensibility and writes

- [ ] Define versioned WIT mapper, connector, encoder, and sink worlds.
- [ ] Run untrusted declarative packages in a bounded Wasmtime worker process or
  pod with fuel, epoch interruption, memory limits, and concurrency quotas.
- [ ] Add pure mapping scripts with explicit evaluator limits.
- [ ] Add idempotent write intents, acknowledgements, optimistic concurrency,
  audit records, and separate write grants.
- [ ] Add HTTP, descriptor-driven Protobuf/gRPC, and UDP connectors based on
  real use cases rather than speculative configuration fields.

## Non-goals

- Loading native Rust dynamic libraries into graph-api.
- Giving grammars ambient filesystem, network, Kubernetes, clock, random, or
  secret access.
- Treating a parser as a serializer or transport.
- Granting wildcard Kubernetes discovery/RBAC so a UI can browse everything.
- Replacing the renderer, layout engines, metrics, or compute broker as part of
  the importer migration.
