---
doctype: design
area: compute
audience: [developer, operator, agent]
status: planned
tags: [jump-cannon, batch, training]
---

# Large Jobs and Training

Jump Cannon does not currently expose a general training API. The correct
extension is a graph-api job contract backed by a Kueue-admitted RayJob or Job,
with explicit inputs, outputs, resource bounds, cancellation, TTL, and status.

Keep credentials and storage policy environment-owned. Reuse [[Ray GPU Sessions]]
and [[Kueue Scheduling]], publish progress through [[Observability]], and add
contract, failure, and performance coverage under [[Testing]].
