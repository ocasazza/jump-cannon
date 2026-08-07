---
doctype: runbook
area: quality
audience: [developer, agent]
status: current
tags: [jump-cannon, browser, webgpu]
---

# Browser Regression

`just test browser-rust` starts graph-api with a test vault, opens the built
Dioxus app in Chromium, exercises Graph header actions and the Nodes workbench,
and writes `nodes-editor.png`, `boot.png`, and `report.json` under
`target/test-browser-rust`.

Stable fixture notes prove that Nodes renders a horizontal navigator/content
split, exposes the core importer search keys, loads selected content, preserves
selection across Flat and Tags, groups exact multi-tags without duplicate rows,
and exposes a synthetic `(untagged)` group. When the wrapper owns the temporary
vault, those fixtures are mandatory rather than silently falling back to a
weaker generic check. The structured result is recorded under
`nodes_editor` in `report.json`.

Chromiumoxide arguments are keys without a leading `--`; its launcher adds the
CLI prefix. Cluster browser jobs run as the chart's non-root identity without a
service-account token, and the automation browser explicitly disables its own
sandbox because the pod is already isolated and capability-free. The harness
uses the fixed Chromium window directly instead of CDP viewport emulation and
enforces `--timeout-secs` as an overall deadline so software WebGPU cannot leave
a CronJob running indefinitely. Navigation is scheduled from `about:blank`
without waiting on Chromium's page-load lifecycle; application readiness comes
from the boot log and graph render checks instead. Readiness evaluation uses
bounded attempts under that same overall deadline because Linux software WebGPU
can temporarily occupy Chromium's renderer while it creates the device and
pipelines; one stalled CDP evaluation is diagnostic evidence, not by itself a
failed application boot.

WebGPU requires a secure browser context. The cluster smoke test reaches the
app through its plain-HTTP Kubernetes Service, so its disposable Chromium
process grants secure-context treatment only to the configured `baseUrl`.
Production browser exposure should terminate TLS rather than copy this test
exception.

A nonzero canvas rectangle alone does not prove nodes rendered. Review both
screenshots and browser errors for visual changes to [[Frontend]] or [[Workspace]].
