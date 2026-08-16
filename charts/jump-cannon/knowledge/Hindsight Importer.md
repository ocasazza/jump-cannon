---
doctype: reference
area: data
audience: [operator, developer, agent]
status: current
tags: [jump-cannon, importer, hindsight, memory]
---

# Hindsight Importer

The `hindsight` source kind publishes one bank of a
[Hindsight](https://hindsight-ui.proxy.cluster.nixstation.internal) memory
service as a graph. Hindsight consolidates agent and fleet observations into
durable facts; this importer turns that memory into something explorable
instead of only queryable through recall.

## Bank selection

A bank is the unit of selection. `hindsightImporter.bank` (CLI
`--hindsight-bank`, env `JUMP_CANNON_HINDSIGHT_BANK`) names it, and the
importer confirms it against `GET /v1/{tenant}/banks` before reading anything.
A mistyped bank fails the import with the banks that do exist rather than
publishing an empty graph. One graph-api instance serves one bank; run another
instance for another bank.

Node IDs are `hindsight:{source_id}:{local}` with `source_id` defaulting to the
sanitized bank id, so two instances of different banks never collide and a
bank keeps stable identity across restarts.

## What becomes a node

|Hindsight record|Node type|Title|
|---|---|---|
|memory unit (a consolidated fact)|`memory`|first line of the fact text, truncated|
|canonical entity|`entity`|the canonical name|
|retained document|`document`|its session id, else the document id|

Only units in the `valid` state publish: invalidated or superseded facts are
retired knowledge and stay out of the graph. Facets follow the bank's own
navigators — `type`, `folder`, `bank`, `fact_type`, `state`, `tags`, and
`entities` — and the full fact text is indexed as `body`, so search over the
graph is search over the bank.

## What becomes an edge

- `temporal`, `semantic`, and `caused_by` links come verbatim from Hindsight's
  own memory graph (`GET .../graph`). Self-loops and duplicate endpoint pairs
  are dropped.
- `mentions` connects each unit to the canonical entities it names.
- `documented_in` connects each unit to the document it was retained from.

Hindsight also reports unit↔unit `entity` links (two units sharing an entity).
Those are deliberately **not** imported: the bipartite `mentions` edges carry
the same adjacency through the entity node without the quadratic blow-up.
Entity names that match no canonical entity, and document references with no
document, are reported as unresolved rather than dropped silently.

## Deployment

The chart wires this mode with `graphApi.source: hindsight` and the
`hindsightImporter` values (`url`, `tenant`, `bank`, `pollIntervalSeconds`,
`maxUnits`, `sourceId`). In-cluster the default URL is the api service
(`http://hindsight-api-proxy.hindsight.svc.cluster.local`); no vault claim is
mounted, because the corpus is remote. Polling is a plain interval — the API
has no ETag validator — and a failed poll leaves the prior complete revision
live.

`maxUnits` is a hard bound, not a truncation: a bank larger than the bound
fails the import loudly so nobody explores a silently partial memory. Content
stays read-only (no `/vault/page` editor path): Hindsight owns consolidation,
and writing facts back through a graph view would bypass it.

An authenticated Hindsight API takes a bearer token only through
`hindsightImporter.tokenSecret.name`/`key`, never through values; the token is
redacted from every log line, capability scope, and error message.

See [[Importer Runtime]], [[Import and Generate]], and [[Helm Deployment]].
