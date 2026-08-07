# jump-cannon chart

This chart packages the jump-cannon frontend/API, an optional Kueue-admitted
RayCluster GPU compute session, and weekly CronJobs for fuzz, performance, and
browser smoke tests. By default, the jobs run on Sunday in
`America/Los_Angeles`: fuzz at 00:00, browser smoke at 00:15, and performance
at 00:30.

The chart intentionally does not create network exposure objects. The
envoy-ai-gateway deployment owns NetBird `NetworkResource` and Gateway API
routes for the nixstation cluster.

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

## Open Knowledge Format importer

Set `graphApi.source=okf` to load an OKF bundle from the same filesystem mount
used by the Obsidian source. `okfImporter.sourceId` supplies the stable source
identity used to namespace graph node IDs. For example:

```yaml
graphApi:
  source: okf

okfImporter:
  sourceId: product-catalog
  filesystemRescanIntervalSeconds: 60

vault:
  persistence:
    enabled: true
    existingClaim: lavender-ingest-data
  seed:
    enabled: false

additionalServiceAccounts:
  lavender-ingest:
    labels:
      app.kubernetes.io/component: ingestion
    automountServiceAccountToken: false
```

Here `lavender-ingest` owns the `lavender-ingest-data` PVC and jump-cannon
mounts it read-only for the OKF source. Setting
`vault.persistence.existingClaim` suppresses this chart's PVC resource. Both
releases must use the same namespace because PVCs and ServiceAccounts are
namespace-scoped. The claim's access mode and storage backend must also support
the ingestion and graph-api pods' intended concurrent mount pattern.

The graph API combines native filesystem notifications with a periodic full
rescan. The rescan is important for cross-pod writers because some CSI, NFS,
and RWX volume implementations do not propagate remote-write notification
events to every mount. `okfImporter.filesystemRescanIntervalSeconds` defaults
to `60`; increase it for very large bundles when scan cost matters, or set it to
`0` only when the storage backend's event propagation is known to be reliable.
Reload publication is atomic, and an invalid or incomplete bundle leaves the
last good graph active until a later scan succeeds.

Obsidian retains notification-only behavior by default. Set
`graphApi.filesystemRescanIntervalSeconds` above `0` if that source is also fed
through a storage backend that needs polling fallback.

For standalone deployments, leave `existingClaim` empty. With persistence
enabled, jump-cannon then creates and owns its normal release-scoped PVC. Both
chart-owned and Lavender-owned storage modes support the Obsidian and OKF
filesystem importers.

`additionalServiceAccounts` is a map keyed by exact ServiceAccount name. It
lets the jump-cannon release create namespace-local identities for companion
ingestion components, with optional annotations and labels. Token automount
defaults to `false` for every additional account. The consuming chart must set
that name in its own pod spec; for example, configure lavender-ingest with
`serviceAccount.create=false` and `serviceAccount.name=lavender-ingest` to use
the account above without rendering a duplicate.

ServiceAccounts do not authorize PVC mounts, so this mechanism deliberately
does not create Roles or RoleBindings. Pod volume references, the PVC/PV access
modes, storage-class behavior, and pod security contexts govern storage access.
The built-in Obsidian example seed is never mounted for the OKF source, even if
`vault.seed.enabled` remains true, because those Markdown files are not an OKF
bundle.
