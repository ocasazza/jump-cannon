---
doctype: runbook
area: operations
audience: [operator, agent]
status: current
tags: [jump-cannon, helm]
---

# Helm Deployment

`charts/jump-cannon` packages graph-api, frontend assets, optional Ray compute,
scheduled tests, and this knowledge graph. Build the release artifact with
`nix build .#chart-tarball`; Hydra publishes the resulting chart through the
secured chart cache.

The chart owns portable workload configuration. The consuming environment owns
cluster policy, credentials, NetBird resources, and Gateway routes. Continue
through [[GitOps Release]], [[Security Model]], and [[Scheduled Tests]].
