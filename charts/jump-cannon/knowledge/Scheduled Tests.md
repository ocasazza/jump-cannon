---
doctype: runbook
area: operations
audience: [developer, operator, agent]
status: current
tags: [jump-cannon, cronjob, testing]
---

# Scheduled Tests

The chart schedules nightly fuzz, browser, performance, and k6 CronJobs in
the configured time zone. Defaults are daily at 00:00, 00:15, 00:30, and 00:45.

Fuzz and browser work are bounded CPU jobs. GPU performance work is admitted by
[[Kueue Scheduling]] and may remain queued until quota is deliberately granted.
Because a Kueue-held Job stays suspended rather than finishing, the performance
CronJob uses `concurrencyPolicy: Replace`; under `Forbid` a single queued run
would starve every later night. Its `activeDeadlineSeconds` bounds an admitted
run that hangs, and does not expire a Job that is only awaiting GPU quota.
All test pods use the chart's non-root security contexts, drop capabilities,
and disable service-account token automount.
Fuzz and performance workloads stream CPU profiles to Pyroscope
(`tests.fuzz.profiling` / `tests.performance.profiling`); the k6 CronJob runs
the chart-owned k6 script against the deployed graph-api and remote-writes
`k6_*` metrics to Prometheus. k6 is Kueue-opt-in (off by default, non-GPU
queue when enabled) with its own bounded resources, so high-frequency soak
schedules are never blocked behind GPU quota. See [[Observability]].
Results feed [[Observability]], [[Fuzz Testing]], [[Browser Regression]], and
[[Performance Engineering]].
