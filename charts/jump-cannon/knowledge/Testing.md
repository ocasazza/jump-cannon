---
doctype: runbook
area: development
audience: [developer, operator, agent]
status: current
tags: [jump-cannon, testing]
---

# Testing

Run `cargo check --workspace --tests` for Rust contract coverage. Use focused
crate tests while iterating, then run `just test browser-rust` for the real
Dioxus and WebGPU path.

Continuous regression layers are [[Fuzz Testing]], [[Performance Engineering]],
and [[Browser Regression]]. Cluster cadence and admission live in
[[Scheduled Tests]].
