---
doctype: guide
area: development
audience: [user, developer, agent]
status: current
tags: [jump-cannon, worlds, version-control]
---

# Worlds and Versioning

A **world** is an importer-produced shared graph under version control. Where
graph-api serves one configured source, a world is owned by its users: they
edit it through commits, branch it, and merge it back. The [[Session Manager]]
hosts worlds; the app's Sessions view is the console.

Versioning is jj-inspired (`crates/graph-vcs`):

- Commits carry stable **change-ids** that survive rebase, plus
  content-addressed commit ids.
- An **op-log** records every repository operation.
- Merges and rebases operate on whole **snapshots** — three-way,
  attribute-level for nodes, set-based for edges — never on patches.
- **Conflicts are first-class recorded state**: a merge or rebase can land
  with conflicts attached, and resolution is a separate later operation.

Two stores implement the same `VcsStore` contract:

- **minigraf** — embedded, single-file; native and browser. It is the default
  everywhere and the only store that works without a server.
- **TerminusDB** — one database per world in the cluster, for deployments
  that want a real database behind shared worlds. Merge/rebase are implemented
  in Rust on top of its native branch and document APIs.

Both exist so the identical jj model serves a browser alone and a multi-user
cluster, with parity tests holding the two backends to one contract.

The served branch is always `main`: commits landing on a world's `main`
rebuild the graph served under `/worlds/:name/*` through the same atomic
snapshot path as any importer reload. Other branches are working state until
merged or rebased onto `main`.

The VCS layer stores user-authored local IDs; publishing namespaces them to
the unified identity contract, so served node IDs are
`world:<world-slug>:<local>` (commits and ops keep the local form).

In **browser-alone mode** (no session-manager URL configured) the embedded
host keeps worlds in-memory and re-exports each one to localStorage after
every mutation, replaying on boot. The Worlds panel adds a commit editor
(add/remove nodes and edges on `main`) and JSON export/import of a world's
full history, so a single user still gets durable, portable versioned worlds.
