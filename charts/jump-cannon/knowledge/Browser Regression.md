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
and writes `nodes-editor.png`, `settings-importers.png`, `boot.png`, and
`report.json` under `target/test-browser-rust`.

Stable fixture notes prove that Nodes renders a horizontal navigator/content
split, uses the importer-neutral `Search fields` label while exposing the core
schema-provided keys, loads selected content, preserves
selection across Flat and Tags, groups exact multi-tags without duplicate rows,
renders the same fixture under both `foo/bar/baz` and `bee/bop/baz`, and exposes
a synthetic `(untagged)` group. It also hit-tests the search and
mode controls below the Panel Kit header, switches to tiling, and verifies that
the Nodes tile remains large enough for a left navigator and wider content
pane. When the wrapper owns the temporary vault, those fixtures are mandatory
rather than silently falling back to a weaker generic check. The structured
result is recorded under `nodes_editor` in `report.json`.

The same run maximizes unified Settings and verifies the Connection, Importers,
Layout, Appearance, and Camera tabs in order. Each tab must have one selected
ARIA tab, a matching tabpanel with real delegated content, and an unobscured
pointer target; the retired standalone configuration panels must be absent.
The importer fixture proves that the active source is identified, the Lavender
source shows its exact read-only claim and `/var/lib/lavender/okf-repository/okf`
input, deployment-provisioned RWX and same-namespace requirements, alternate
`<release>-okf` naming, UID/GID `10001`, and rollout ownership are explained,
and no Apply, Run, or Activate action is present. Restoring Settings must
remount a render-ready Graph canvas. These results are recorded under
`settings_tabs`.

Chromiumoxide arguments are keys without a leading `--`; its launcher adds the
CLI prefix. Cluster browser jobs run as the chart's non-root identity without a
service-account token, and the automation browser explicitly disables its own
sandbox because the pod is already isolated and capability-free. Linux
headless runs keep Vulkan compute but disable the display surface, presenting
through Chromium's headless path so the first WebGPU frame cannot block CDP.
The harness uses an incognito, per-run browser profile and the fixed Chromium
window directly instead of CDP viewport emulation. This prevents persisted
panel choices and concurrent profile locks from affecting results, and
enforces `--timeout-secs` as an overall deadline so software WebGPU cannot leave
a CronJob running indefinitely. Navigation is scheduled from `about:blank`
without waiting on Chromium's page-load lifecycle; application readiness comes
from the boot log and graph render checks instead. Readiness evaluation uses
bounded attempts under that same overall deadline because Linux software WebGPU
can temporarily occupy Chromium's renderer while it creates the device and
pipelines; one stalled CDP evaluation is diagnostic evidence, not by itself a
failed application boot.

The console collector preserves CDP severity. Resource-load errors, unhandled
exceptions, `console.error`, and Rust tracing `ERROR` records all fail the run;
warnings remain diagnostic. A report is not green merely because a failure was
delivered through CDP's Log domain instead of Runtime.

WebGPU requires a secure browser context. The cluster smoke test reaches the
app through its plain-HTTP Kubernetes Service, so its disposable Chromium
process grants secure-context treatment only to the configured `baseUrl`.
Production browser exposure should terminate TLS rather than copy this test
exception. The happy-path assertion records both `window.isSecureContext` and
`navigator.gpu` before accepting the renderer-ready marker.

The runner accepts both HTTP and HTTPS base URLs. Its raw TCP liveness probe is
plain-HTTP only; an HTTPS production-origin run deliberately navigates directly
with Chromium so certificate trust, NetBird authentication, secure-context
classification, and WebGPU exposure are all exercised by the same browser that
performs the assertions.

A nonzero canvas rectangle alone does not prove nodes rendered. Review both
screenshots and browser errors for visual changes to [[Frontend]] or [[Workspace]].
