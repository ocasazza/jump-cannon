---
doctype: guide
area: development
audience: [developer, agent]
status: current
tags: [jump-cannon, rust]
---

# Rust Workspace

Every product surface is Rust. The root Cargo workspace contains backend,
compute, layout, data, and test crates. `app/` is a separate Dioxus and Tauri
workspace so desktop dependencies do not enter the server build graph.

Use Nix and the repository `just` recipes rather than adding an alternate tool
stack. Preserve the boundaries in [[Architecture]] and finish through [[Testing]].

## Lockfile merge hazard

A clean git merge can still corrupt `Cargo.lock`: the textual union of two
valid lockfiles can leave dependency lists referencing versions the merged
package set no longer contains. Both parents build, the merge tree builds
locally (cargo silently rewrites the lock), and only crane's
`cargo check --locked` fails on Hydra. Verify with `cargo metadata --locked`
after any merge that touched the lockfile; regenerate with a plain
`cargo check` and push the fix before the eval cascade burns.
