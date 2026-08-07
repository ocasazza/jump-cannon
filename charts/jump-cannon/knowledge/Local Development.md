---
doctype: runbook
area: development
audience: [developer, agent]
status: current
tags: [jump-cannon, local]
---

# Local Development

Run `just dev-up` for graph-api, the vault watcher, and the browser frontend.
Use `just app-dev` when the Tauri shell is part of the change. The default API
endpoint is `http://127.0.0.1:8765` and can be changed in Settings.

Use `just dev-down` for symmetric cleanup. Check [[Troubleshooting]] when the
backend, assets, browser GPU, or persisted workspace state disagree.
