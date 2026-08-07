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
