---
doctype: reference
area: data
audience: [operator, developer, agent]
status: current
tags: [jump-cannon, importer, github]
---

# GitHub Importer

The `github` source kind delivers the knowledge corpus straight from a GitHub
repository instead of the vault filesystem. graph-api downloads the codeload
tarball for the configured repository and ref, extracts it into an ephemeral
pod cache, and parses the configured subdirectory with the same Obsidian
markdown pipeline the vault source uses. This is the docs-importer mode:
knowledge updates flow push-to-main → poll → complete validated rebuild →
atomic snapshot swap, with no chart republish.

Polling uses the tarball ETag. A 304 response re-imports the cached
extraction; a changed tarball replaces the cache and triggers a rebuild. A
failed fetch or import leaves the prior complete revision live.

Node identity is continuous with Obsidian mode: IDs are
`github:{source}:{vault-relative path}` and the local part matches what the
vault importer produces for the same files, so the same corpus keeps stable
node IDs whether it arrives over the filesystem or the tarball. The reserved
`Jump Cannon/` folder semantics are unchanged: the corpus under
`charts/jump-cannon/knowledge` stays chart-owned in both delivery modes.

The chart wires this mode with `graphApi.source: github` and the
`githubImporter` values (`repo`, `ref`, `path`, `pollIntervalSeconds`,
`cacheDir`). This is the **live source in the nixstation cluster** since
envoy-ai-gateway `5e3ed2a` (2026-08-11): the platform component sets
`graphApi.source = "github"` with the default repo/ref/path values, so the
deployed corpus tracks `ocasazza/jump-cannon@main` with a 60 s ETag poll. The
knowledge ConfigMap and vault seed sync are untouched and remain the fallback
under `graphApi.source: obsidian`. Tokens for private
mirrors never enter values; point `githubImporter.tokenSecret.name`/`key` at
an existing Secret and the chart injects `JUMP_CANNON_GITHUB_TOKEN` through a
`secretKeyRef`.

See [[Importer Runtime]], [[Import and Generate]], and [[Helm Deployment]].

## In-browser import (GitHub Pages)

The same corpus also loads with no backend at all. The GitHub Pages
deployment at `https://ocasazza.github.io/jump-cannon/` serves the Dioxus app
as a static site (built by the `pages` GitHub Actions workflow from the
`.#app-web-pages` flake output, trunk `--public-url /jump-cannon/`) and the
app imports in WASM: it lists the repository's markdown through the
CORS-enabled git trees API, fetches each note from `raw.githubusercontent.com`,
and runs the exact `vault-links` extraction plus `github:{source_id}:`
re-namespacing in the browser, so node IDs match a server-side import of the
same corpus. codeload tarballs are not used there — codeload sends no CORS
headers for arbitrary origins.

The GitHub panel drives it: repo / ref / subdir, persisted in the browser.
`?gh=owner/repo&ref=…&path=…` deep-links an import; a bare `*.github.io`
visit imports the default `ocasazza/jump-cannon` knowledge corpus. Public
repositories only (no token field), bounded at 512 notes / 8 MiB, and the
import is a browser-only graph: the canvas, layout, and style panels work,
while server-backed search and node metadata stay unavailable.
