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
