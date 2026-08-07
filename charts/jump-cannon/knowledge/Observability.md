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

Verify that a job ran, its metrics arrived, and the dashboard query returns the
same labels. Missing data is not a passing result. See [[Scheduled Tests]],
[[Performance Engineering]], and [[Troubleshooting]].
