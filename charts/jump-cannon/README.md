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
