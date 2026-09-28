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

The k6 script and the browser test binary reach the cluster on different
cadences. The k6 script ships inside the chart artifact, so it is pinned to
the chart's `app.kubernetes.io/version` (the source revision Hydra packaged);
the browser binary ships in the `jump-cannon-test-runner` image, which the
Browser CronJob pulls with `imagePullPolicy: Always`. After a jump-cannon
commit lands, the two can therefore disagree — the chart artifact can be older
than the image. When a nightly job fails, compare the CronJob's
`app.kubernetes.io/version` against the current source revision before
suspecting the app: a k6 failure against a live graph-api is usually a stale
chart script, and a browser failure can be a newer binary asserting a contract
the older chart's values no longer satisfy. See [[Browser Regression]] and
[[Helm Deployment]].
