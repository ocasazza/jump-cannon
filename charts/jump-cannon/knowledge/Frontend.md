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

Do not add handwritten JavaScript or a JS bundler. Validate visible changes with
[[Browser Regression]] and keep server contracts in [[Backend API]].
