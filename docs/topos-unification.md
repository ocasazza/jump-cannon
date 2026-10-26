# Unified Topos Concept

## Overview
The **topos** in Jump‑Cannon is a single, coherent abstraction that bridges

* **Category‑theoretic topoi** – the abstract sheaf‑topos framework defined in
  `lean‑topos.md`, and
* **UI/UX‑level topoi** – the concrete canvas‑to‑regime transformation defined in
  `OMP Auto‑Loop Topos.md`.

By treating both as instances of a *topos* we obtain a systematic way to map
different UI/UX regimes (e.g., *Session view*, *Stream view*, *Cycle detail*) onto
one another. The mapping is realized by the **geometric morphism** `f*` (and its
adjoint `f_*`) that translates objects, morphisms, and logical properties from
one regime’s topos to another’s while preserving structure.

## Core Principles
1. **Sheaf‑topos as a model of UI state** – every UI panel, node kind, colour
   scheme, and interaction can be seen as a *section* of a sheaf over a base
   topological space (the underlying graph).  The base space encodes constraints
   such as layout geometry, facet rules, and data flow.
2. **Translation functor `f*`** – a UI‑level transformation (the *topos* of a
   jump‑cannon view) is precisely the inverse‑image part of a geometric morphism.
   It maps UI elements from a *source* regime to a *target* regime, preserving
   the logical relationships expressed in the source sheaf.
3. **Adjoint `f_*`** – the direct‑image functor provides the reverse mapping,
   allowing us to propagate UI‑level changes (e.g., a user‑driven colour tweak)
   back into the abstract model and then forward to another regime.
4. **Regime‑agnostic API** – UI components (Style, Metrics, Inspector) request
   data via the abstract `NodeMeta` / `EdgeMeta` interfaces.  The concrete
   implementation in a given regime is automatically derived by applying the
   appropriate translation functor.

## Practical Implications
- **Consistent colour‑commuting** – colour assignments derived from
  `StabilityProfile` in the *Layout* regime are automatically transformed into
  the corresponding colour scheme for the *Session* regime via `f*`.
- **Cross‑regime panel reuse** – the *Stream* panel in the *Session* regime and the
  *Cycle detail* panel in the *Run* regime share the same underlying sheaf
  definitions; only the translation functor differs.
- **Formal verification** – Lean proofs in `lean‑topos.md` guarantee that any
  UI‑level transformation respecting the topos constraints preserves logical
  invariants (e.g., edge‑type consistency, facet ordering).

## References
- `charts/jump-cannon/knowledge/OMP Auto-Loop Topos.md` – concrete UI definition.
- `docs/research/water-energetic-docking/04-benchmarks/lean-topos/lean-topos.md`
  – abstract sheaf‑topos framework.
- `docs/implementation-plan.md` – implementation tasks that expose the
  unified concept to the UI.

---
*This document makes the unified topos concept explicit, enabling systematic
mapping between any two UI/UX regimes in Jump‑Cannon.*