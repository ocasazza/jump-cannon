---
doctype: runbook
area: quality
audience: [developer, agent]
status: current
tags: [jump-cannon, browser, webgpu]
---

# Browser Regression

`just test browser-rust` starts graph-api with a test vault, opens the built
Dioxus app in Chromium, exercises Graph header actions, and writes `boot.png`
and `report.json` under `target/test-browser-rust`.

Chromiumoxide arguments are keys without a leading `--`; its launcher adds the
CLI prefix. Cluster browser jobs run as the chart's non-root identity without a
service-account token, and the automation browser explicitly disables its own
sandbox because the pod is already isolated and capability-free.

A nonzero canvas rectangle alone does not prove nodes rendered. Review the
screenshot and browser errors for visual changes to [[Frontend]] or [[Workspace]].
