---
doctype: architecture
area: data
audience: [developer, operator, agent]
status: current
tags: [jump-cannon, importer]
---

# Importer Runtime

Importers acquire records, map them into validated graph operations, and publish
one complete graph. The default markdown loader resolves wikilinks. Kubernetes
queries are explicit, bounded, metadata-only, and namespace-scoped by default.
OKF loads a filesystem bundle under a stable source identity.

Do not hide network access, credentials, or authorization inside a pure mapper.
Deployment owns those effects through [[Helm Deployment]] and [[Security Model]].
