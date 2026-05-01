# jump-cannon — agent rules

## RUST ONLY

This repo is **Rust everywhere**. No JavaScript, no TypeScript, no React,
no Vue, no DOM frameworks, no virtual DOM. The frontend is Rust + wgpu +
egui compiled to WebAssembly. The backend is Rust + axum.

The single allowed JS shim is `crates/graph-renderer/assets/main.js`,
which **must stay under 50 lines** and do exactly one thing: load the
WASM module and hand control to Rust. No DOM manipulation, no event
handlers, no fetch helpers, no protobuf decoding, no UI state. All of
that lives in Rust.

If you're tempted to "just add a quick JS function" — stop. Add it in
Rust. The egui surface in `graph-renderer/src/ui/` is where UI changes
go. The bindings in `graph-renderer/src/web.rs` are where browser-side
event hooks live.

Why: the project already drifted into a 1100-line `main.js` doing
sidebar / modal / filters / query builder / DOM events / protobuf —
and that's exactly the failure this rule prevents.

## Stack

- `crates/graph-api` — axum HTTP server (vault load, metrics, search proxy, protobuf endpoints)
- `crates/graph-renderer` — Rust + wgpu + egui (via eframe). Native binary and WASM (trunk-built). All UI lives here.
- `crates/graph-layouts` — wgpu compute force-sim. Native + WASM.
- `crates/graph-metrics` — PageRank, Louvain, k-core, etc.
- `crates/vault-data`, `crates/vault-links`, `crates/vault-search` — data pipeline.

## Build pipeline: nix + crane + trunk

**One toolchain.** Everything builds through the flake:
- Native crates: `crane.buildPackage` with the workspace's rust-overlay toolchain.
- WASM frontend: `crane.buildTrunkPackage` (or equivalent) — trunk drives wasm-bindgen, wasm-opt, asset hashing, output to `dist/`.
- `nix build .#graph-renderer-web` produces the dist directory.
- `nix build .#graph-api` produces the server binary that serves `dist/` via include_dir or runtime path.
- `nix build` (default) gives you a deployable bundle with both.

No `npm install`. No `wasm-pack`. No yarn lockfile. Trunk + cargo + nix only.

`just dev`, `just wasm`, `just test-browser` are convenience wrappers around the nix outputs — never standalone command stacks.

## Wire format

- Bulk numeric (positions, edges, metrics): raw little-endian f32/u32 buffers.
- Structured (init, NodeMeta, search results): protobuf via `prost` on the server, `prost` on the WASM client too. Don't bring `protobufjs` into the JS shim.

## Testing rule

Run `just test-browser` before claiming any visual change works. The
Playwright-driven Chromium test launches the app with WebGPU, captures
a screenshot + console log, and asserts the canvas isn't black. Don't
commit visual changes without it returning `ok: true`.

## Do not

- Do not add JS dependencies to `assets/`.
- Do not write CSS files. egui owns all styling in Rust.
- Do not create `tests/browser/*.js` UI logic — only the test harness lives there.
- Do not introduce a JS bundler (vite, esbuild, webpack).
- Do not propose three.js, Cosmograph, OrbitControls, or any JS lib for rendering. wgpu + egui only.

<!-- BEGIN BEADS INTEGRATION v:1 profile:minimal hash:ca08a54f -->
## Beads Issue Tracker

This project uses **bd (beads)** for issue tracking. Run `bd prime` to see full workflow context and commands.

### Quick Reference

```bash
bd ready              # Find available work
bd show <id>          # View issue details
bd update <id> --claim  # Claim work
bd close <id>         # Complete work
```

### Rules

- Use `bd` for ALL task tracking — do NOT use TodoWrite, TaskCreate, or markdown TODO lists
- Run `bd prime` for detailed command reference and session close protocol
- Use `bd remember` for persistent knowledge — do NOT use MEMORY.md files

## Session Completion

**When ending a work session**, you MUST complete ALL steps below. Work is NOT complete until `git push` succeeds.

**MANDATORY WORKFLOW:**

1. **File issues for remaining work** - Create issues for anything that needs follow-up
2. **Run quality gates** (if code changed) - Tests, linters, builds
3. **Update issue status** - Close finished work, update in-progress items
4. **PUSH TO REMOTE** - This is MANDATORY:
   ```bash
   git pull --rebase
   bd dolt push
   git push
   git status  # MUST show "up to date with origin"
   ```
5. **Clean up** - Clear stashes, prune remote branches
6. **Verify** - All changes committed AND pushed
7. **Hand off** - Provide context for next session

**CRITICAL RULES:**
- Work is NOT complete until `git push` succeeds
- NEVER stop before pushing - that leaves work stranded locally
- NEVER say "ready to push when you are" - YOU must push
- If push fails, resolve and retry until it succeeds
<!-- END BEADS INTEGRATION -->
