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

panel-kit 1.0 is the workspace contract. Its `SurfaceProfile` selects compact,
tablet, or regular layout and pointer capability; root pointer events and
keyboard window-management commands are routed through `Workspace`. Workspace
storage reads legacy V1 records but writes `SavedLayoutV2` in CSS-pixel units;
Jump Cannon's v6-v8 panel-enum migration also emits V2 before `use_workspace`
reconciles it to the live viewport.

The renderer requires browser WebGPU; there is no WebGL compute fallback. On
an insecure origin or a browser without a usable adapter, the Graph panel shows
an actionable unavailable state while Nodes and the rest of the workspace stay
usable. Use HTTPS (or localhost) for the browser UI.

The pre-WASM loading shell follows panel-kit's static boot contract: `#main`
stays empty and the marked shell is its immediately following sibling. Dioxus
0.6 does not clear pre-existing mount children; putting the shell inside
`#main` reaches the diff engine as foreign unkeyed DOM and panics with
`invalid key`. App CSS hides the sibling after Dioxus marks the mount root.

A returning visitor's own panel layout is server-rendered into that boot shell
before any WASM arrives, with no JavaScript. The app mirrors the primary
(user-view) workspace's *settled* layout into a `jc_shell` cookie
(`app/ui/src/main.rs::serialize_shell_cookie` / `write_shell_cookie`, written
via `web-sys` — the value is percent-encoded because its grammar's `;` and `,`
are cookie delimiters, and `Secure` is set only on an https origin). graph-api's
explicit `GET /` route (`crates/graph-api/src/server.rs::boot_shell_response`)
reads that cookie and rewrites the region of `index.html` delimited by the
`<!--panel-kit-boot-frames-->` … `<!--/panel-kit-boot-frames-->` markers with one
absolutely-positioned `.panel-kit-boot-panel-fixed` frame per panel. The
explicit route outranks the static-asset `fallback_service`, so only the bare
`/` request is intercepted; every other asset (index.html fetched by name
included) still flows through ServeDir's precompressed/304 pipeline. The cookie
is untrusted input: on absence, on ANY validation failure, or in tiling mode
(its geometry is viewport-derived and not knowable server-side) the file is
returned byte-for-byte so the static default shell renders, and injected titles
are HTML-escaped regardless of the cookie charset guard. The value grammar
(`v1;<mode>;<title>,<x>,<y>,<w>,<h>;…`, ≤16 panels, ≤1024 bytes) is defined once
as a prose comment on both the writer and the parser; keep the two in sync. The
`/` response is small and cookie-dependent, so it is served uncompressed with
`Cache-Control: no-cache` and `Vary: Cookie`.

Post-boot hydration is store-shaped, from `panel_kit::loading`: panel chrome
renders immediately and async data loads behind a per-source store
(`loading_store(id, label)` + `begin`/`update`/`succeed`/`fail`), panel
bodies gate on `LoadingGate`, and the top bar's `GlobalLoadingBar`
aggregates anything pending. A determinate wait always shows its percentage
via `ProgressBar`; `Spinner` is reserved for tiny inline waits. Building
server sources surface through the same stores: the apply tracker keeps
polling the 202→200 transition and auto-commits when the build finishes —
one Load click, no manual retry — and the Progress panel's poller resets its
`since` cursor on source switch so the new source's log replays from its
start. The Importers panel's Variables section (httpjson rows) edits package
instance variables and applies them with a reload; see [[Importer Runtime]].
UI/UX follows the https://impeccable.style/ design vocabulary noted in
AGENTS.md.

Do not add handwritten JavaScript or a JS bundler. Validate visible changes with
[[Browser Regression]] and keep server contracts in [[Backend API]].
