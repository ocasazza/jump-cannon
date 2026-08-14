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

The importer catalog is deployment policy. Define named instances under
`importers.sources` and activate one with `importers.selected`; an empty
selector preserves `graphApi.source` and `kubernetesImporter.enabled`. The
application displays a sanitized catalog in **Settings > Importers**. The
rollout-based `importers.selected` flow remains the deployment default for
choosing the active source; named chart profiles are currently wired for
Obsidian, Kubernetes, and OKF, and source kinds with additional required
inputs are not accepted until the chart owns their complete configuration.

When `importers.runtimeSwitchGroup` names a NetBird group, every configured
filesystem source is also mounted read-only in the graph-api pod (not only the
selected one) and graph-api lets viewers whose proxy-injected
`x-netbird-groups` header contains that group switch the viewed source at
runtime from **Settings > Importers** (per browser session, read-only graph
views; writes, generation, and compute stay on the deployment-selected
source). Only set the group once every dormant profile's producer claim
exists in the release namespace — a mounted claim is a hard pod dependency,
so a missing claim blocks pod startup and fails the rollout. An empty
`runtimeSwitchGroup` disables runtime switching, skips the dormant mounts,
and preserves the rollout-only behavior. See [[Security Model]] for
the trust boundary.

`graphApi.source: github` selects the docs-importer mode: graph-api polls the
GitHub repository tarball named by `githubImporter.repo`/`ref`/`path` and
rebuilds on change, so knowledge updates ship with a push to main instead of a
chart republish. The vault PVC, seed init container, and knowledge ConfigMap
sync are skipped in this mode; they remain the fallback under the default
`obsidian` source. Private mirrors take a token only through
`githubImporter.tokenSecret`, never through values. See [[GitHub Importer]].

The chart's inactive `lavender-ingest-okf` instance consumes the externally
provisioned `lavender-okf-shared` RWX claim read-only. It mounts the repository
at `/var/lib/lavender/okf-repository` and imports
`/var/lib/lavender/okf-repository/okf`. Activate it with:

```yaml
importers:
  selected: lavender-ingest-okf
```

Lavender's writer normally owns a dedicated claim named
`lavender-ingest-okf` or `<release>-okf`, with repository root
`/data/okf-repository` and workflow input `/data/okf-repository/okf`. Shared
mode instead requires this writer value:

```yaml
okf:
  persistence:
    existingClaim: lavender-okf-shared
```

Keep both releases in the same namespace. The storage class must support RWX,
and files must be readable and traversable by UID/GID `10001`. Never substitute
Lavender lake or state storage for the OKF repository. A live Git working tree
is not an immutable workflow snapshot; record its HEAD or copy it before a run
that requires reproducibility.

## okf-sync CronJob

lavender-ingest git-pushes the OKF repository to the private
`schrodinger/lavender-okf` GitHub repo nightly. Set `okfSync.enabled=true` to
render a CronJob (default schedule `17 * * * *`) that fast-forward pulls that
repo into the profile's claim, so the OKF source serves real data without a
writer deployment sharing the volume. Claim name and mount path are derived
from the importer catalog profile itself — the selected OKF profile, or the
catalog's single OKF entry — never duplicated in values; the chart fails to
render when no runnable OKF filesystem profile exists or when the derived
claim is the vault claim.

The deploy key always comes from a user-created Secret named by
`okfSync.deployKeySecret` (default `lavender-okf-reader`) holding the
read-only GitHub deploy key at key `id_ed25519`, mounted at
`/secrets/okf-reader` with mode `0400`. The key never appears in values.

The derived claim is RWO `local-path` and graph-api already mounts it
read-only, so the sync pod carries a hard `podAffinity` on the graph-api pod
labels with `kubernetes.io/hostname` topology: it only schedules on the
graph-api node. Syncs are fast-forward-only; divergence or a non-`main`
checkout fails the Job loudly for an operator — the claim is a read replica
and the job never merges or force-resets.

The chart owns portable workload configuration. The consuming environment owns
cluster policy, credentials, NetBird resources, and Gateway routes. Continue
through [[GitOps Release]], [[Security Model]], and [[Scheduled Tests]].

This is the production infrastructure boundary for Jump Cannon, including the
optional GPU-backed RayCluster. Local Docker Compose remains development-only;
parallel repository-level infrastructure stacks are intentionally not
maintained.
