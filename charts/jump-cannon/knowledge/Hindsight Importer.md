---
doctype: reference
area: data
audience: [operator, developer, agent]
status: current
tags: [jump-cannon, importer, http-json, hindsight, memory]
---

# Hindsight Importer

Hindsight consolidates agent and fleet observations into durable facts. The
[`hindsight-memory-bank.toml`](../../packages/hindsight-memory-bank.toml)
**package** bound to an instance turns one Hindsight memory bank into a
graph; the engine is the shared
[`http-json-importer`](https://github.com/ocasazza/jump-cannon/tree/main/crates/http-json-importer)
crate, not a Hindsight-specific one. There is no `Hindsight` source kind:
the kind is `httpjson` and Hindsight is the package that maps its endpoints
(see [[Importer Runtime]] and AGENTS.md "Importers: packages, not crates").

## How a bank becomes an instance

A bank is the unit of selection. Two instance-level variables feed into the
package's path templates:

| Variable | Default | Set with |
|---|---|---|
| `tenant` | `"default"` | `--importer-var tenant=…` (or rely on the package default) |
| `bank` | *required* | `--importer-var bank=…` |

The package's preflight (`GET /v1/{tenant}/banks`) lists the banks the
tenant actually offers. A mistyped `bank` fails the import loudly with the
banks that do exist rather than publishing an empty graph; one graph-api
instance serves one bank, run another instance for another bank.

Node IDs are `httpjson:{source_id}:{local}` where `source_id` is the bound
instance's slug (set with `--importer-source-id`), so two instances of
different banks never collide and one bank keeps stable identity across
restarts.

## What becomes a node

The Hindsight package declares four collections against the verified 0.9.1
API:

|Hindsight record|Node type|Title|
|---|---|---|
|memory unit (a consolidated fact)|`memory`|first line of the fact text, truncated|
|canonical entity|`entity`|the canonical name|
|retained document|`document`|its session id, else the document id|
|unit↔unit link (from `GET .../graph`)|edge only|—|

Only units in the `valid` state publish: invalidated or superseded facts
are retired knowledge and stay out of the graph. The collection's API-side
`?state=valid` filter is repeated client-side in a `skip_unless` predicate
so an older deployment that ignores the query parameter still drops retired
units. Facets follow the bank's own navigators — `type`, `folder`, `bank`,
`fact_type`, `state`, `tags`, `session_id`, `entities`, and indexed numbers
(`proof_count`, `mention_count`, `memory_count`) — and the full fact text
is indexed as `body` (searchable + snippet), so search over the graph is
search over the bank.

## What becomes an edge

- `temporal`, `semantic`, and `caused_by` links come verbatim from
  Hindsight's own memory graph (`GET .../graph`). Self-loops and
  duplicate endpoint pairs are dropped (default `drop_self_loops = true`,
  default `dedupe = unordered`).
- `mentions` connects each unit to the canonical entities it names. The
  unit's `entities` field is a CSV string (`"tofu, Hydra"`); the
  `split_csv` transform turns it into `["tofu", "Hydra"]` and resolves
  against the `entities` collection by canonical name (`match_on =
  "title"`).
- `documented_in` connects each unit to the document it was retained
  from, resolved against the `documents` collection by document id
  (`match_on = "id"`).

Hindsight also reports unit↔unit `entity` links (two units sharing an
entity). Those are deliberately **not** imported into the `links`
collection (`include_kinds = ["temporal", "semantic", "caused_by"]` on the
package's `EdgeListRules`); the bipartite `mentions` edges carry the same
adjacency through the entity node without the quadratic blow-up, and the
schema's `semantic` edge type is declared `directed = false` to match.
Entity names that match no canonical entity, and document references with
no document, are reported as unresolved rather than dropped silently.

## Bound variables, defaults, and read-only nature

| Knob | Default | Bound with |
|---|---|---|
| package path | chart mounts `hindsight-memory-bank.toml` read-only | `--importer-manifest <path>` |
| API endpoint | unset → no instance | `--importer-endpoint <http(s) URL>` (e.g. the in-cluster `http://hindsight-api-proxy.hindsight.svc.cluster.local`) |
| polling | `60000` ms (0 → static one-shot) | `--importer-poll-interval-ms` |
| bearer token | unset (no Authorization header) | `--importer-token` (redacted from every log line, capability scope, and error message) |
| source id | the package id (`hindsight.memory-bank`) | `--importer-source-id` |

`limits.max_records = 50000` (with `request_timeout_seconds = 120`) is a
hard bound, not a truncation: a bank that exceeds it fails the import
loudly so nobody explores a silently partial memory. Content stays
read-only (no `/vault/page` editor path): Hindsight owns consolidation,
and writing facts back through a graph view would bypass it.

## Adding another bank

Same package, new instance — no Rust touched. Drop a copy of the TOML
verbatim (it's a single source-of-truth mapping) and bind it through the
chart's `httpJsonImporter` values:

- `httpJsonImporter.package` → `hindsight-memory-bank.toml`
- `httpJsonImporter.endpoint` → API root for that bank
- `httpJsonImporter.variables` → `{ tenant: <t>, bank: <b> }`

Two graph-api pods running the same package against two different
endpoints publish two distinct graphs (`httpjson:omp:…`,
`httpjson:jira-ithelp:…`, …). See [[Helm Deployment]] for the chart
wiring and [[Importer Runtime]] for the engine's instance model.

See also [[Import and Generate]] (settings panel), [[Importer Runtime]],
[[Backend API]], and [[Helm Deployment]].
