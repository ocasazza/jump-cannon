---
doctype: runbook
area: operations
audience: [developer, operator, agent]
status: current
tags: [jump-cannon, cronjob, testing]
---

# Scheduled Tests

The chart schedules weekly fuzz, browser, and performance CronJobs in the
configured time zone. Defaults are Sunday at 00:00, 00:15, and 00:30.

Fuzz and browser work are bounded CPU jobs. GPU performance work is admitted by
[[Kueue Scheduling]] and may remain queued until quota is deliberately granted.
Results feed [[Observability]], [[Fuzz Testing]], [[Browser Regression]], and
[[Performance Engineering]].
