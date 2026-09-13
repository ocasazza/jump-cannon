---
doctype: architecture
area: development
audience: [developer, agent]
status: current
tags: [jump-cannon, frontend, dioxus, wgpu]
---

# Frontend

`app/ui` is Dioxus 0.6 compiled to WASM. panel-kit owns generic workspace
behavior; Jump Cannon owns panels, graph interactions, API calls, and app CSS.
wgpu draws the graph and `graph-layouts` supplies in-process layout compute.
The renderer requires browser WebGPU; there is no WebGL compute fallback. On
an insecure origin or a browser without a usable adapter, the Graph panel shows
an actionable unavailable state while Nodes and the rest of the workspace stay
usable. Use HTTPS (or localhost) for the browser UI.

The pre-WASM loading shell follows panel-kit's static boot contract: `#main`
stays empty and the marked shell is its immediately following sibling. Dioxus
0.6 does not clear pre-existing mount children; putting the shell inside
`#main` reaches the diff engine as foreign unkeyed DOM and panics with
`invalid key`. App CSS hides the sibling after Dioxus marks the mount root.

Post-boot hydration is store-shaped, from `panel_kit::loading`: panel chrome
renders immediately and async data loads behind a per-source store
(`loading_store(id, label)` + `begin`/`update`/`succeed`/`fail`), panel
bodies gate on `LoadingGate`, and the top bar's `GlobalLoadingBar`
aggregates anything pending. A determinate wait always shows its percentage
via `ProgressBar`; `Spinner` is reserved for tiny inline waits. Building
server sources surface through the same stores: the apply tracker keeps
polling the 503→200 transition and auto-commits when the build finishes —
one Load click, no manual retry — and the Progress panel's poller resets its
`since` cursor on source switch so the new source's log replays from its
start. The Importers panel's Variables section (httpjson rows) edits package
instance variables and applies them with a reload; see [[Importer Runtime]].
UI/UX follows the https://impeccable.style/ design vocabulary noted in
AGENTS.md.

Do not add handwritten JavaScript or a JS bundler. Validate visible changes with
[[Browser Regression]] and keep server contracts in [[Backend API]].
