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

`just test scenarios` runs the YAML configuration-as-code test beds in
`crates/test-scenarios/scenarios/` — a scenario pins the importer package,
input fixture, exact `GpuForceOptions`, and acceptance gates (precision:
bit-identical reruns; accuracy: UFF bond lengths, ring angle sums,
planarity; stochastic: seeded-jitter distribution gates; robustness:
full-noise boundedness). The same caffeine bed drives the browser suite's
molecular-regime scenario, and its layout parameters ship as the
`molecular-uff` registry regime (`app/configs/regimes/`) that any UFF-typed
graph resolves to automatically — `?config=molecular-uff` pins it explicitly.
See `docs/molecular-force-layout.md` for the measured values.

Continuous regression layers are [[Fuzz Testing]], [[Performance Engineering]],
and [[Browser Regression]]. Cluster cadence and admission live in
[[Scheduled Tests]].
