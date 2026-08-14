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
and writes `nodes-editor.png`, `settings-importers.png`, `sessions-view.png`,
`boot.png`, and `report.json` under `target/test-browser-rust`.

The report also verifies the startup handoff: the marked static boot shell is
the adjacent sibling of `#main`, never a child of the Dioxus mount, and is
hidden after the first Dioxus commit. This protects the empty-mount invariant
documented in [[Frontend]] as well as catching resource-load and WASM errors.

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
`<release>-okf` naming, UID/GID `10001`, and rollout ownership are explained.
The switch-posture assertion is per-viewer: against the main fixture server
(no `JUMP_CANNON_IMPORTER_SWITCH_GROUP`) the runtime switch selector must be
absent (`data-runtime-switch="disabled"`, no `.importer-switch-btn`); against
a switching-enabled deployment the harness asserts the posture it is served —
the group-required note and no controls for a denied viewer, controls for an
authorized one. Restoring Settings must remount a render-ready
Graph canvas. These results are recorded under `settings_tabs`.

The Filter builder contract derives its nested-group expectations from the
served corpus rather than from fixture notes (a deployment whose importer
grants no content-write effect cannot be seeded). The harness decodes
`/graph/meta_summary` — the same facet payload the panel evaluates field
rules against — picks two tag values whose node sets have a strict
intersection-smaller-than-union relationship, and asserts the nested group's
ALL count equals the computed intersection and the ANY count the computed
union. These results are recorded under `filter_builder`.

A second fixture graph-api exercises the runtime switching contract itself.
The harness mirrors the served app dist over raw HTTP into a tempdir, spawns
`graph-api` from PATH with a two-source Obsidian catalog (default vault plus a
tiny alternate vault) and `JUMP_CANNON_IMPORTER_SWITCH_GROUP=test-admins`, and
simulates the authenticating proxy with `Network.setExtraHTTPHeaders`. It
asserts the wire gate directly (no group → 403, group → 200, unknown id →
404), then in the browser: the selector appears for an authorized viewer,
clicking the alternate swaps the Nodes list to the alternate vault and marks
the card `viewing`, the sessionStorage selection persists across an in-tab
reload, a fresh tab returns to the deployment default, and an unauthorized
viewer sees the "Switching requires NetBird group" note with no controls —
plus a stale planted selection recovers through the "return to deployment
default" affordance. The authorized page's console feeds the shared error
gate; the unauthorized page deliberately fetches a 403, so only its wire
status is asserted. These results are recorded under `importer_switch`.

The run then clicks the topbar **Sessions** view switcher. View switches
reload the page (persisted `jc_view` + `location.reload()`), so the suite
detects the fresh runtime by the `window.__jc_boot` stamp changing and
asserts in fresh evaluates: against the embedded single-user host the
contract is deterministic — the Sessions workspace mounts the Worlds panel
with its "no worlds yet" empty state and the dock, and switching back to
**User** remounts a render-ready Graph canvas. These results are recorded
under `sessions_view`, with visual evidence in `sessions-view.png`. The
view contract itself lives in [[Session Manager]].

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
