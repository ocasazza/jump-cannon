---
doctype: reference
area: data
audience: [operator, developer, agent]
status: current
tags: [jump-cannon, importer, native, generator]
---

# Native Graph Generator

The `native` source kind serves a graph built entirely in-process at startup —
no tvix evaluation, no vault claim, no remote endpoint, and no filesystem
root. `crates/native-importer` wraps the pure `crates/native-graph-generator`
SoA builder, which reproduces the exact `waterSheet` topology from
`crates/tvix-wasm/src/nix/graph.nix`: a pool of `molecules` disconnected
3-node water molecules (`water-O` + two `water-H`, two bonds each) plus one
`rows` × `cols` sheet lattice (each cell joined to its right and down
neighbour). Node and edge counts are fixed by the parameters:

- nodes = `3 × molecules + rows × cols` — the default 20000/256/256 yields
  125,536 nodes,
- edges = `2 × molecules + 2 × rows × cols − rows − cols` — 170,560 by
  default.

Unlike the Nix path the builder never materialises an attrset, toGraphJSON,
or a `VaultGraph`-shaped intermediate: nodes and edges live in flat
pre-sized vectors (10 bytes per node, 16 bytes per edge) built in one
O(n + e) pass, so the same crate scales from the default sheet to
100M+-node benchmarks before the importer shell materialises the canonical
graph.

Node IDs are `native:{source_id}:{local}` with the waterSheet local names —
`w{m}_O` / `w{m}_H1` / `w{m}_H2` and sheet cells `s{i}` — so a graph is
deterministic for a given `(molecules, rows, cols, source_id)`. Discovery
fields are the required `id` / `title` / `tags` plus a facetable `type`
keyword (`water-O`, `water-H`, `sheet`); edges are untyped (the conventional
`generated` vocabulary, declared undirected).

The chart wires the mode with `graphApi.source: native` and the
`nativeImporter` values (`molecules`, `rows`, `cols`, `sourceId`), which
render as `JUMP_CANNON_NATIVE_MOLECULES` / `JUMP_CANNON_NATIVE_ROWS` /
`JUMP_CANNON_NATIVE_COLS` / `JUMP_CANNON_NATIVE_SOURCE_ID` on the graph-api
container. Nothing is mounted: the deployment renders no vault claim, no
seed init container, and no volumeMounts for this kind (verified with
`helm template`). The importer catalog does not carry this kind — its
parameters are deployment env, so it is selected by chart rollout like
GitHub and Kubernetes, not by runtime source switching.

The startup log reports the counts: `graph loaded n_nodes= n_edges=` should
read `3 × molecules + rows × cols` / `2 × molecules + 2 × rows × cols − rows
− cols` — a wrong number means the parameters did not bind.

See [[Importer Runtime]], [[Import and Generate]], and [[Helm Deployment]].
For the tvix path to the same topology (and the generator packages that
replaced the retired `tvix` source kind), see [[Import and Generate]].
