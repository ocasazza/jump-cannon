---
doctype: runbook
area: quality
audience: [developer, operator, agent]
status: current
tags: [jump-cannon, fuzz]
---

# Fuzz Testing

Fuzzing protects parsers and graph transformations against malformed or unusual
inputs. Reproduce a failure locally, retain the smallest regression case, and
fix the owning layer rather than filtering the symptom at a caller.

The Helm chart runs the bounded suite through [[Scheduled Tests]]. Failures are
operational signals in [[Observability]] and code changes still follow [[Testing]].
