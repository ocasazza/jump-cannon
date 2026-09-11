# Computational cameras — research notes

Consolidated 2026-09-11 from the feature-planning comments that lived in
`app/ui/src/panels/camera.rs`. This document is the back-reference for every
linked source and the synthesis of what a "computational camera" feature set
means for jump-cannon's wgpu graph renderer. The migration contract
(`docs/dioxus-migration.md`, "Future work — camera models") points here.

## Current pipeline baseline

Grounding for everything below — what the renderer actually does today.

- **Camera model**: 6DoF perspective (`app/ui/src/render/camera.rs`) —
  position + yaw/pitch, `fov_y` 60°, `znear` 0.1, `zfar` 200k. Ops: `pan`,
  `rotate_yaw/pitch`, `zoom` (move along forward), `look_at_point`,
  `fit_to_bounds`, `reset`, `raycast`. No orthographic mode, no clipping
  slabs, no named/serializable views beyond position+basis.
- **DoF uniforms** (`app/ui/src/render/pipelines.rs:106`): `EffectsUniform`
  carries `focus_plane_z`, `focus_thickness`, `blur_strength`, `max_coc`;
  staged via `set_focus_plane` / `set_dof_params`
  (`pipelines.rs:1347-1357`). DoF "off" is a sentinel thickness of 1e9.
- **Bokeh shader** (`app/ui/src/shaders/node.wgsl`): vertex stage computes
  view-space depth `view_dist`, `dz = |view_dist − focus_plane_z|`, depth
  error beyond the half-thickness band becomes
  `coc = min(blur_z · blur_strength, max_coc)` extra quad pixels
  (`node.wgsl:125-133`); fragment stage picks the sharp SDF path when DoF is
  off or `coc_ratio > 0.985`, else a soft disc with intensity ∝
  `coc_ratio²` for energy conservation (`node.wgsl:209-223`).

### Concretely broken in the current bokeh path

1. **World-z vs view-space depth mismatch.** `push_focus`
   (`panels/camera.rs`) sets `focus_plane_z = camera.position.z − distance`
   — an absolute *world* Z coordinate — but the shader compares it against
   radial view-space distance. The focal band is only correct while the
   camera looks straight down −Z; any yaw/pitch silently detunes focus.
2. **Non-perspective CoC mapping.** `coc = blur_z · blur_strength` grows
   linearly in *world* depth-error with a dimensionless slider, ignoring
   perspective (real CoC scales with aperture and inversely with subject
   distance). Near and far nodes blur identically per world-unit, so the
   effect reads wrong at every zoom level.
3. **Halo/depth interaction.** The CoC-inflated quad still rasterizes at
   the node's original depth; enlarged discs are occluded by nearer
   geometry instead of blooming over it, so bokeh clips in dense regions.
   *(INFERENCE from the shader — no depth-state change accompanies quad
   inflation; needs render-state confirmation.)*
4. **Aesthetic failure mode.** Energy conservation (`intensity ∝
   coc_ratio²`) makes out-of-focus regions of a dense graph go nearly
   invisible instead of glowing, so the feature feels absent rather than
   soft.

## Source set

### Cryo-EM and computational structural biology

The linked set is coherent once read together: it is about *recovering
signal from enormous numbers of noisy, partial observations* — which is
exactly the perceptual problem of rendering a large, dense graph.

- **sciadv.adv8257** — Ferguson, Raghavan, Alzua, Bhavsar, Huang,
  Rodriguez, Torres, Bottermann, Han, Krammer, Batista, Ward,
  *"Functional and epitope specific monoclonal antibody discovery directly
  from immune sera using cryo-EM"*, Science Advances 11(33), eadv8257,
  2025-08-15 (<https://www.science.org/doi/10.1126/sciadv.adv8257>,
  paywalled; catalog record:
  <https://researchprofiles.ku.dk/en/publications/functional-and-epitope-specific-monoclonal-antibody-discovery-dir/>).
  Cryo-EM used not just for structure but for *selection*: finding a rare
  signal (one antibody species) inside a complex mixture by computational
  classification of projection images.
- **phonchi/Computational-CryoEM**
  (<https://github.com/phonchi/Computational-CryoEM>) — curated map of the
  single-particle-analysis pipeline: motion correction → CTF estimation →
  denoising → particle picking → 2D classification → ab-initio model → 3D
  refinement → 3D variability analysis → postprocessing. Notable for this
  project:
  - **CTF / defocus**: defocus is an estimated *measurement parameter*
    (CTFFIND5, gCTF, patch-based estimation), not an aesthetic. Contrast
    varies with defocus and must be modeled and corrected.
  - **Denoising as a first-class stage** (Topaz-Denoise, noise2noise/JANNI,
    Restore): extremely low SNR inputs are made legible before analysis.
  - **Classification** (ISAC, Relion Bayesian 2D classes): millions of
    noisy projections collapse into a few dozen representative views.
  - **3D variability** (cryoDRGN, 3DFlex, ManifoldEM): heterogeneity is
    modeled as a continuous landscape, not averaged away.
- **snijderlab/stitch** (<https://github.com/snijderlab/stitch>) —
  template-based assembly of proteomics short reads for de novo antibody
  sequencing and repertoire profiling (C#, MIT). The proteomics analog of
  image stitching: many short, overlapping, error-prone fragments are
  assembled into a consensus against templates, with explicit error
  alphabets (`alphabets/common_errors_alphabet.csv`) and benchmarked
  ground truths (`benchmark/`, DOI 10.1021/acs.jproteome.1c00913).
- **Schrödinger org code search** (`org:schrodinger cryo` /
  `imaging OR microscopy OR tomography`): no public or internal cryo-EM /
  computational-imaging repos as of 2026-09-11 — only container-image
  infrastructure. The org's relevant public rendering codebase is PyMOL
  (below).

### Molecular / scientific visualization cameras

- **schrodinger/pymol-open-source**
  (<https://github.com/schrodinger/pymol-open-source>) — the reference for
  what camera features a serious scientific viz tool ships:
  - `get_view` / `set_view`: the full camera state as an 18-float tuple —
    3×3 rotation, rotation origin in camera *and* model space, front/rear
    clip distances, orthoscopic flag
    (<https://pymolwiki.org/index.php/Get_View>). Views are scriptable,
    saveable, and restorable — scenes are reproducible.
  - **Orthoscopic/orthographic projection** as a first-class toggle.
  - **Depth cueing + fog**: distance attenuation of contrast for depth
    legibility in dense scenes.
  - **Clipping slabs** (front/rear plane control) to cut sections through
    dense structure.
  - **Stereo modes** (cross-eye/wall-eye/quad-buffer with angle/shift).

### Real-time point-set rendering

- **3D Gaussian Splatting** — Kerbl, Kopanas, Leimkühler, Drettakis,
  *"3D Gaussian Splatting for Real-Time Radiance Field Rendering"*, ACM
  Transactions on Graphics (SIGGRAPH 2023),
  <https://arxiv.org/abs/2308.04079>; reference implementation:
  <https://github.com/graphdeco-inria/gaussian-splatting>. Scenes are
  rendered as anisotropic 3D gaussians (position, covariance, opacity,
  spherical-harmonic color) with adaptive density control
  (clone/split/prune) and a tile-based, depth-sorted alpha-blending
  rasterizer — real-time (≥30 fps at 1080p) at millions of primitives.
  Relevant twice over: (a) the *level-of-detail* answer for massive node
  sets — far nodes become splats whose size/opacity encode local density
  instead of individually drawn glyphs; (b) its per-tile depth-sorted
  alpha blending is a proven solution to exactly the halo-occlusion
  problem flagged in the bokeh baseline above.

## Synthesis — feature set

Ordered cheap-and-correct → researchy. Item 1 is a bug fix, not research.

1. **Fix the DoF model** (bug; small). Compute the focal plane in view
   space at slider-change time (or push camera-space depth per frame from
   the 30 Hz loop); replace the linear `blur_z · strength` CoC with a
   perspective-correct mapping parameterized as aperture + focal distance;
   address halo clipping (draw inflated quads at the focal plane's depth,
   or sort/additive-blend the bokeh pass).
2. **Typed camera models** (small; started). `DofParams` is now a grouped
   struct (`panels/camera.rs`, serde-flattened, wire-compatible). Next:
   `enum CameraModel { Perspective { .. }, Orthographic { .. } }` where
   each variant carries its own parameter struct; the panel swaps
   parameter sets as a unit. PyMOL's orthoscopic flag proves the UX.
3. **Depth cueing / fog** (small, high value). Distance-based contrast
   attenuation in `node.wgsl`/`edge.wgsl` — the cheap 80% of DoF's
   depth-legibility benefit, no bokeh artifacts.
4. **View state serialization** (small). A `get_view`/`set_view` analog:
   position + yaw/pitch + fov + focal plane as one serializable struct;
   named saved views in the Camera panel; stable restore across graph
   reloads. This is the "camera as data" foundation for everything below.
5. **Clipping slabs / section views** (medium). Front/rear clip controls
   (PyMOL slab model) to slice dense graphs; shader-side discard against
   view-space depth bounds.
6. **Defocus as a data channel** (medium; CTF-inspired). Cryo-EM treats
   defocus as information. Analog: deliberately drive the DoF band from a
   *data attribute* (staleness, distance-from-selection, confidence) so
   focus itself encodes signal — contrast transfer as a legibility tool,
   not damage.
7. **Density-aware LOD via splat-style rendering** (large). Beyond a node
   budget, render far regions as density splats (size/opacity from local
   node count and attribute aggregates) instead of glyphs; gaussian-
   splatting rasterization is the proven real-time technique. Interacts
   with the scale concerns in the original TODO: node/edge counts,
   sparsity, and pipeline settings (density/width, shader intensity) must
   feed the LOD threshold.
8. **Consensus / stitching of views** (research; stitch-inspired).
   Treat overlapping partial observations of a large graph (filtered
   views, per-community layouts, time slices) as fragments to be assembled
   into one navigable consensus view, with explicit mismatch surfacing —
   the proteomics error-alphabet idea applied to graph mutations between
   snapshots.
9. **Layout-variability landscapes** (research; 3D-variability-inspired).
   Run layout ensembles (seeds/algorithms), render positional variance as
   an uncertainty halo or variability map — cryoDRGN-style heterogeneity
   modeling for graph layout stability.

## Scale interplay (from the original TODO, preserved)

Camera-model usefulness depends on simulation scale: node/edge counts,
sparsity, and current visual pipeline settings (node/edge density and
width, shader choice, `shader_intensity`). Camera-mode selection and LOD
thresholds should react to those signals, not only to user sliders.
