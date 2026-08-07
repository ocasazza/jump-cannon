# Importer architecture

Status: **accepted; compatibility foundation implemented**.

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
  and atomically publishes a graph snapshot. The compatibility slice still
  returns a complete `VaultGraph`; graph-api validates the invariants that
  remain representable there before publication, and failed imports never
  replace the live graph.
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

Every importer exposes a descriptor with:

- stable ID, display name, version, and manifest API version;
- supported snapshot/watch/write modes;
- requested capabilities;
- accepted and emitted media types;
- configuration schema and secret-reference fields;
- deterministic limits for input bytes, records, nodes, edges, and diagnostics.

Source-instance IDs must namespace node and edge IDs before multiple importers
can be composed into one graph. Kubernetes does this now; the current Pest
adapter is still a single active source and keeps package-local IDs. Built-in
mappers use fallible node insertion so duplicate IDs are rejected before an
`IndexMap` can overwrite them. Graph-api additionally rejects dangling edges
and non-finite positions before publication. The planned operation sink must make
duplicate and output-limit enforcement unavoidable for third-party mappers;
post-hoc validation of an already materialized map cannot recover overwritten
duplicate provenance.

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

Planned server API:

- `GET /importers` lists built-ins and installed packages, validation state,
  digest, version, and requested capabilities.
- `PUT /importers/:id` validates a bounded single-file package and its fixtures,
  then atomically installs it. This is gated on authentication/authorization.
- `POST /importers/:id/preview` returns counts, sampled nodes/edges,
  diagnostics, requested effects, and a digest without changing the live graph.
- `POST /importers/:id/runs` starts an asynchronous run and returns a run ID.
- `GET /import-runs/:id` reports durable status and diagnostics.

The Dioxus app gets a dedicated Importers panel rather than expanding Generate.
It reuses the existing Rust file-input pattern and progress feed. A successful
run calls the existing graph reload path. Credentials never enter browser
localStorage or shareable app-state exports.

Until the control plane is secured, a Pest source is selected with explicit
administrator-managed manifest and input paths, normally supplied through
read-only mounts. The server validates the selected package at startup and
fails on an unknown source; it never silently falls back to Obsidian.

## Migration plan

### Phase 1: safe compatibility foundation

- [x] Add composable connector, decoder, mapper, descriptor, capability, and
  error contracts to `data-loader` while preserving the current `Loader` API.
- [x] Add a trusted/admin runtime-Pest importer with a versioned manifest,
  canonical captures, validation, limits, and golden tests.
- [x] Add an async Kubernetes snapshot connector over allowlisted dynamic
  resources, initially read-only.
- [x] Select an importer by stable ID/path without an unknown-source fallback.
- [x] Make watcher startup honor the selected importer instead of always
  watching `$VAULT_ROOT` for Markdown.
- [x] Keep Obsidian, tvix, and generated loaders behavior-compatible.

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
- [ ] Route node content and search through source adapters. Content metadata is
  explicit and `/vault/page` is now guarded as an Obsidian-only compatibility
  write route.
- [ ] Move JSON/CSV/DOT decoders out of `graph-layouts` into importer codecs.

### Phase 3: incremental runtime and secured library

- [ ] Add source event streams and graph deltas; initially coalesce deltas into
  the existing full snapshot publication path.
- [ ] Add the authenticated importer/source-instance/run control plane.
- [ ] Add the Dioxus Importers panel and Rust browser regression.
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
