---
doctype: runbook
area: operations
audience: [developer, operator, agent]
status: current
tags: [jump-cannon, observability, grafana]
---

# Observability

graph-api exposes health and progress through its HTTP surface and structured
logs. Cluster fuzz, browser, and performance jobs publish bounded status and
result metrics to Pushgateway for Grafana dashboards.

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
