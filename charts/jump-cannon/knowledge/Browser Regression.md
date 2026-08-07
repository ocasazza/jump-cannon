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

A nonzero canvas rectangle alone does not prove nodes rendered. Review the
screenshot and browser errors for visual changes to [[Frontend]] or [[Workspace]].
