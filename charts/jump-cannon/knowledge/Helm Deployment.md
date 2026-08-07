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

Vault ConfigMaps preserve the first byte of every note. In particular, YAML
frontmatter must render with `---` immediately after the literal-block marker;
an added blank line makes Obsidian treat the metadata and tags as body text.
Use `indent`, not `nindent`, for these block values and verify the rendered
ConfigMap before publishing the chart.

The chart owns portable workload configuration. The consuming environment owns
cluster policy, credentials, NetBird resources, and Gateway routes. Continue
through [[GitOps Release]], [[Security Model]], and [[Scheduled Tests]].

This is the production infrastructure boundary for Jump Cannon, including the
optional GPU-backed RayCluster. Local Docker Compose remains development-only;
parallel repository-level infrastructure stacks are intentionally not
maintained.
