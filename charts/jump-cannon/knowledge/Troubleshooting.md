---
doctype: runbook
area: operations
audience: [developer, operator, agent]
status: current
tags: [jump-cannon, troubleshooting]
---

# Troubleshooting

Check the layers in order: stable service-name resolution, Gateway or NetBird
routing, graph-api health, asset delivery, graph counts and IDs, browser console,
WebGPU canvas, compute admission, then dashboard ingestion.

Use direct evidence from [[Service Access]], [[Backend API]], [[Browser Regression]],
[[Kueue Scheduling]], and [[Observability]]. Fix the declarative owner instead
of adding host-local repair scripts.
