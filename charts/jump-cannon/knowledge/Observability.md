---
doctype: runbook
area: operations
audience: [developer, operator, agent]
status: current
tags: [jump-cannon, observability, grafana]
---

# Observability

graph-api exposes health and progress through its HTTP surface and structured
logs. Cluster fuzz, browser, performance, and k6 jobs publish bounded status
and result metrics to Pushgateway / Prometheus for Grafana dashboards.

The chart ships its Grafana dashboards with the release: every JSON under
`charts/jump-cannon/dashboards/` renders as a `grafana_dashboard`-labeled
ConfigMap (see `grafanaDashboards` in `values.yaml`), and the monitoring
stack's Grafana sidecar loads them into its provisioned charts folder. The
test-results dashboard queries the Pushgateway gauges from [[Scheduled Tests]]
(`test_run_passed` / `test_run_failed` / `test_duration_seconds` /
`jump_cannon_test_last_run_timestamp_seconds`, all `app="jump-cannon"`).

Verify that a job ran, its metrics arrived, and the dashboard query returns the
same labels. Missing data is not a passing result. See [[Scheduled Tests]],
[[Performance Engineering]], and [[Troubleshooting]].

## CPU flame graphs (Pyroscope)

Two profiling paths feed the monitoring stack's Grafana Pyroscope server:

- **Perf benches** (`tests.performance.profiling` in `values.yaml`, opt-in):
  criterion's pprof profiler (`BENCH_PPROF`) runs for the bench, writes one
  `profile.pb` per benchmark, and the jump-cannon-perf wrapper pushes each to
  Pyroscope as a `jump-cannon-perf.<bench>` series.
- **Fuzz + geometric harnesses** (`tests.fuzz.profiling`, opt-in): the
  streaming agent (`crates/test-profiling`, ctor hook) samples 100 Hz for the
  whole libtest run and pushes live, labeled `test=<dashboard row>` and
  `run=<pod name>` via `PYROSCOPE_URL` / `JUMP_CANNON_TEST_NAME` /
  `JUMP_CANNON_RUN_ID`. No-op when unset, so local runs are unaffected.

The test-results dashboard's flame-graph panel (Pyroscope datasource) shows
the selected test + trace (run), the cubism panel packs every test's duration
over time and click-drills into a test's flame graph. Verify with:
`curl http://pyroscope.monitoring.svc.cluster.local:4040/ready`.

## Grafana-native k6 tests

The nightly `k6-api-smoke` CronJob runs a k6 HTTP regression script
(`charts/jump-cannon/k6/api-smoke.js`, mounted from a ConfigMap) against the
deployed graph-api and streams results to the monitoring stack's Prometheus
via `experimental-prometheus-rw`. `k6_*` series carry `app="jump-cannon"`,
`test="k6-api-smoke"`, and a per-run `testid`, so the dashboard's k6 panels
and any Grafana alerting can filter and segment runs natively.
