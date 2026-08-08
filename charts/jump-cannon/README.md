# jump-cannon chart

This chart packages the jump-cannon frontend/API, an optional Kueue-admitted
RayCluster GPU compute session, and weekly CronJobs for fuzz, performance, and
browser smoke tests. By default, the jobs run on Sunday in
`America/Los_Angeles`: fuzz at 00:00, browser smoke at 00:15, and performance
at 00:30.

The chart intentionally does not create network exposure objects. The
envoy-ai-gateway deployment owns NetBird `NetworkResource` and Gateway API
routes for the nixstation cluster.

## Default knowledge graph

The chart packages the canonical markdown under `knowledge/` and synchronizes
it to the reserved `Jump Cannon/` folder in the Obsidian vault. The graph starts
at `Start Here` and maps product use, development, operations, compute,
observability, testing, security, troubleshooting, and the agent workflow used
to maintain Jump Cannon itself.

Packaged knowledge is chart-owned and replaced on each graph-api rollout.
Additional `vault.seed.files` are copied to the vault root only when absent, so
the chart never overwrites user-owned seed content. Set
`vault.seed.includeKnowledgeBase=false` to omit the packaged corpus. Seed files
are stored in ConfigMaps and must never contain credentials or sensitive data.

GPU compute and performance tests run at batch priority on the `gpu` LocalQueue.
In nixstation, render them into `gpu-workloads` with `graphCompute.namespace`
and `tests.performance.namespace` so they use the existing LocalQueue. With the
ClusterQueue GPU quota at `0`, the scheduled performance Job stays queued until
an operator deliberately grants quota and frees silicon from a serving workload.

## Kubernetes importer

Set `kubernetesImporter.enabled=true` to select the Kubernetes graph source and
mount its JSON configuration. The default imports Pods only from the Helm
release namespace. Resource queries are explicit and Secrets are disabled.
Each snapshot is bounded by both object count and cumulative serialized source
bytes (`max_objects` and `max_bytes`).

The pod still has `automountServiceAccountToken: false`. To use an in-cluster
identity, explicitly set `kubernetesImporter.serviceAccountToken.enabled=true`.
The chart then projects a short-lived token, cluster CA, and namespace into the
standard service-account path used by the Rust Kubernetes client.

The chart creates no importer RBAC by default. To create namespace-scoped
authorization, set `kubernetesImporter.rbac.create=true` and provide rules with
explicit `apiGroups` and `resources`. The only granted verb is `list`, matching
the metadata-only polling client; wildcard groups and resources are rejected. For
example:

```yaml
kubernetesImporter:
  enabled: true
  serviceAccountToken:
    enabled: true
  rbac:
    create: true
    rules:
      - apiGroups: [""]
        resources: ["pods"]
      - apiGroups: ["apps"]
        resources: ["deployments", "replicasets"]
```

Keep each importer query's `namespaces` within the Helm release namespace when
using the chart-created Role. Use externally managed credentials and RBAC for
cluster-wide or cross-namespace queries.

## Importer source catalog

`importers.sources` is the deployment-time catalog of configured source
instances. Set `importers.selected` to one catalog key to activate it. Leaving
the selector empty preserves the existing `graphApi.source` and
`kubernetesImporter.enabled` behavior, so existing releases can migrate without
changing their active source. Named profiles are currently wired for Obsidian,
Kubernetes, and OKF. Kubernetes profiles use the existing
`kubernetesImporter.config`, token, and RBAC values; tvix, generate, and Pest
remain source-specific graph-api CLI configurations until the chart has
complete profile shapes for them.

The chart ships an inactive `lavender-ingest-okf` item for the shared Lavender
OKF repository. Select it with:

```yaml
importers:
  selected: lavender-ingest-okf
```

That item mounts the deployment-provisioned `lavender-okf-shared` RWX claim
read-only at `/var/lib/lavender/okf-repository` and imports only
`/var/lib/lavender/okf-repository/okf`. It sets the stable source ID to
`lavender-ingest` and performs a full filesystem rescan every 60 seconds. The
selected item, sanitized source details, and active importer are visible in the
application's read-only **Settings > Importers** tab. Switching remains a Helm
configuration and rollout operation; the browser has no Apply, Run, or
Activate control.

The producer and consumer paths are deliberately different:

| Contract | PVC | Repository root | OKF input |
|---|---|---|---|
| `lavender-ingest` writer | `lavender-ingest-okf` or `<release>-okf` by default | `/data/okf-repository` | `/data/okf-repository/okf` |
| jump-cannon reader | deployment-provisioned `lavender-okf-shared` | `/var/lib/lavender/okf-repository` | `/var/lib/lavender/okf-repository/okf` |

For shared mode, configure the writer chart to use the same externally
provisioned claim:

```yaml
okf:
  persistence:
    existingClaim: lavender-okf-shared
```

Both releases must be in the same namespace. The claim and storage backend must
support concurrent RWX mounts, and its directories and files must be readable
and traversable by jump-cannon's UID/GID `10001`. Do not point this profile at
Lavender lake or state volumes. The chart never creates, annotates, or takes
ownership of `lavender-okf-shared`.

The shared Git working tree is a live handoff, not an immutable workflow
snapshot. A workflow that needs a reproducible input must record the repository
HEAD or copy the selected tree to immutable storage before processing it.

The graph API combines native filesystem notifications with a periodic full
rescan. The rescan is important for cross-pod writers because some CSI, NFS,
and RWX volume implementations do not propagate remote-write notification
events to every mount. Named sources use their own
`filesystemRescanIntervalSeconds`; legacy OKF selection uses
`okfImporter.filesystemRescanIntervalSeconds`, which defaults to `60`. Increase
it for very large bundles when scan cost matters, or set it to `0` only when
the storage backend's event propagation is known to be reliable. Reload
publication is atomic, and an invalid or incomplete bundle leaves the last good
graph active until a later scan succeeds.

Obsidian retains notification-only behavior by default. Set
`graphApi.filesystemRescanIntervalSeconds` above `0` if that source is also fed
through a storage backend that needs polling fallback.

For standalone deployments, continue to select `graphApi.source=okf` and leave
`vault.persistence.existingClaim` empty. With persistence enabled, jump-cannon
then creates and owns its normal release-scoped PVC. The named catalog is only
required when source-instance-specific mounts and paths are needed.

`additionalServiceAccounts` is a map keyed by exact ServiceAccount name. It
lets the jump-cannon release create namespace-local identities for companion
ingestion components, with optional annotations and labels. Token automount
defaults to `false` for every additional account:

```yaml
additionalServiceAccounts:
  lavender-ingest:
    labels:
      app.kubernetes.io/component: ingestion
    automountServiceAccountToken: false
```

The consuming chart must set that name in its own pod spec; for example,
configure lavender-ingest with `serviceAccount.create=false` and
`serviceAccount.name=lavender-ingest` to use the account without rendering a
duplicate.

ServiceAccounts do not authorize PVC mounts, so this mechanism deliberately
does not create Roles or RoleBindings. Pod volume references, the PVC/PV access
modes, storage-class behavior, and pod security contexts govern storage access.
The built-in Obsidian example seed is never mounted for the OKF source, even if
`vault.seed.enabled` remains true, because those Markdown files are not an OKF
bundle.
