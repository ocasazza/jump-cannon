---
doctype: runbook
area: operations
audience: [developer, operator, agent]
status: current
tags: [jump-cannon, cronjob, testing]
---

# Scheduled Tests

The chart schedules nightly fuzz, browser, and performance CronJobs in the
configured time zone. Defaults are daily at 00:00, 00:15, and 00:30.

Fuzz and browser work are bounded CPU jobs. GPU performance work is admitted by
[[Kueue Scheduling]] and may remain queued until quota is deliberately granted.
Because a Kueue-held Job stays suspended rather than finishing, the performance
CronJob uses `concurrencyPolicy: Replace`; under `Forbid` a single queued run
would starve every later night. Its `activeDeadlineSeconds` bounds an admitted
run that hangs, and does not expire a Job that is only awaiting GPU quota.
All test pods use the chart's non-root security contexts, drop capabilities,
and disable service-account token automount.
Results feed [[Observability]], [[Fuzz Testing]], [[Browser Regression]], and
[[Performance Engineering]].
