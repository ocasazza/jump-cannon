//! Catalog of shareable EXAMPLE full-UI-state presets.
//!
//! Each example is a complete [`AppState`] that, when loaded, sets up an entire
//! Brownian self-assembly demo in one click:
//!
//!   * the validated bonding / self-assembly [`LensConfig`] regime — reused
//!     verbatim from [`SelfAssemblyPreset`] (the single source of truth shared
//!     with the geometric layout panel's preset buttons),
//!   * the **Geometric Engine** selected with `use_gpu = true` (the
//!     "Geometric (GPU)" backend),
//!   * a tvix GENERATOR expression that creates the initial particle soup
//!     (the embedded `soupGen` / `gridGen` combinators), staged in the Generate
//!     panel ready to evaluate,
//!   * a matching Initial-seed strategy (a sphere shell for spontaneous
//!     morphologies, a cubic grid disc-ish seed for the curvature-folding
//!     tube/vesicle regimes — see the honesty note on [`SelfAssemblyPreset`]),
//!   * sensible camera/style (centroid-follow, fit-to-window, a slightly
//!     heavier node size so the soup is legible).
//!
//! The catalog mirrors the `tvix_wasm::demos()` pattern: a static list the UI
//! offers in a picker. Loading an entry replaces the live `AppState` (preserving
//! the in-memory snapshot ring), exactly like the YAML / share-link import path.

use graph_layouts::geometric::LensConfig;

use crate::ui::layout::algorithms::geometric::SelfAssemblyPreset;
use crate::ui::state::{AppState, Section, SeedStrategy};

/// A named example UI-state preset for the Examples picker.
#[derive(Clone, Copy, Debug)]
pub struct Example {
    pub name: &'static str,
    pub description: &'static str,
    preset: SelfAssemblyPreset,
    /// Number of particles in the generated soup.
    soup_nodes: usize,
    /// Initial-seed `seed_demos()` index (Sphere = 0, Random = 1, Grid = 2).
    seed_index: usize,
}

/// The example catalog — the lipid → sheet → tube → vesicle ladder. Mirrors
/// `SelfAssemblyPreset::ALL`, one full-UI-state preset per morphology.
pub fn catalog() -> &'static [Example] {
    CATALOG
}

const CATALOG: &[Example] = &[
    Example {
        name: "Lipid chains (self-assembly)",
        description: "Valence-2 @180° bonding on a Brownian soup → spontaneous chains. \
                      Geometric (GPU). Sphere seed.",
        preset: SelfAssemblyPreset::LipidChain,
        soup_nodes: 200,
        seed_index: 0, // Sphere shell
    },
    Example {
        name: "Honeycomb sheet (self-assembly)",
        description: "Valence-3 @120° + membrane flattening → spontaneous honeycomb patches. \
                      Geometric (GPU). Sphere seed.",
        preset: SelfAssemblyPreset::HoneycombSheet,
        soup_nodes: 256,
        seed_index: 0,
    },
    Example {
        name: "Tube (curved sheet)",
        description: "Sheet regime + spontaneous curvature folds a patch into a tube. \
                      Geometric (GPU). Grid seed (start as a flat-ish disc).",
        preset: SelfAssemblyPreset::Tube,
        soup_nodes: 256,
        seed_index: 2, // Grid — a flat-ish starting patch the curvature can roll
    },
    Example {
        name: "Vesicle (rim seam + curvature)",
        description: "P3 rim line-tension (γ=4) + curvature (c₀=0.5) folds a seeded bonded \
                      disc toward a shell. Geometric (GPU). Grid seed.",
        preset: SelfAssemblyPreset::Vesicle,
        soup_nodes: 256,
        seed_index: 2,
    },
];

impl Example {
    /// The tvix soup-generator expression staged into the Generate panel.
    fn generator_expr(&self) -> String {
        format!(
            "# {} — initial particle soup for the dynamic-bonding engine.\n\
             # Evaluate to spawn {} unbonded particles; the Geometric (GPU)\n\
             # engine grows bonds at runtime into the target morphology.\n\
             let\n  \
             g  = import /jc/src/graph.nix {{}};\n  \
             gc = import /jc/src/graph-combinators.nix {{ graph = g; }};\n\
             in\n  \
             g.toGraphJSON (gc.soupGen {{ nodes = {}; prefix = \"s\"; }})\n",
            self.name, self.soup_nodes, self.soup_nodes
        )
    }

    /// The Custom-seed expression matching `seed_index`, so an agent that picks
    /// the Custom strategy still gets a runnable, regime-appropriate seed.
    fn seed_expr(&self) -> String {
        // Mirror the `tvix_wasm::seed_demos()` built-ins so the source is a
        // faithful, editable copy of the strategy this example selects.
        tvix_wasm::seed_demos()
            .get(self.seed_index)
            .map(|d| d.expr.to_string())
            .unwrap_or_default()
    }

    /// Build the full [`AppState`] for this example.
    pub fn build_state(&self) -> AppState {
        let mut state = AppState::default();

        // --- Engine: Geometric (GPU) with the validated self-assembly regime ---
        let mut cfg = LensConfig::default();
        self.preset.apply_to(&mut cfg); // single source of truth — sets bonding + knobs
        cfg.use_gpu = true; // the "Geometric (GPU)" backend
        cfg.use_multilevel = false;
        state.layout.active = "geometric".to_string();
        if let Ok(v) = serde_json::to_value(&cfg) {
            state.layout.settings.insert("geometric".to_string(), v);
        }

        // --- Generator: stage the soup expr in the Generate panel ---
        state.generate.editor.source = self.generator_expr();

        // --- Initial seed: a built-in strategy + matching Custom source ---
        state.seed.strategy = SeedStrategy::BuiltIn(self.seed_index);
        state.seed.editor.source = self.seed_expr();

        // --- Style: a slightly heavier node so the soup reads clearly ---
        state.style.size_mul = 0.8;

        // --- Camera: track the assembling cluster ---
        state.camera.follow_centroid = true;
        state.camera.fit_to_window = true;

        // --- Panels: open Generate + Layout so the demo is ready to drive ---
        state.set_section_open(Section::Generate, true);
        state.set_section_open(Section::Layout, true);

        state
    }

    /// Encode this example as a share hash (compact JSON → DEFLATE → base64url).
    pub fn share_hash(&self) -> Result<String, String> {
        crate::ui::share::encode(&self.build_state())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every example must deserialize into an `AppState` whose geometric lens
    /// has `bonding_enabled == true` and carries the morphology knobs the
    /// `SelfAssemblyPreset` validates — i.e. the example actually wires up the
    /// self-assembly demo it claims to.
    #[test]
    fn every_example_enables_bonding_and_sets_morphology() {
        for ex in catalog() {
            let state = ex.build_state();

            // Engine is the geometric bridge with GPU on.
            assert_eq!(state.layout.active, "geometric", "{}", ex.name);
            let v = state
                .layout
                .settings
                .get("geometric")
                .unwrap_or_else(|| panic!("{}: missing geometric settings", ex.name));
            let cfg: LensConfig = serde_json::from_value(v.clone())
                .unwrap_or_else(|e| panic!("{}: lens decode: {e}", ex.name));

            assert!(cfg.use_gpu, "{}: must select Geometric (GPU)", ex.name);
            assert!(cfg.bonding_enabled, "{}: must enable bonding", ex.name);

            // The morphology knobs must match the shared preset exactly.
            let mut want = LensConfig::default();
            ex.preset.apply_to(&mut want);
            assert_eq!(cfg.default_max_valence, want.default_max_valence, "{}", ex.name);
            assert_eq!(cfg.default_bond_angle, want.default_bond_angle, "{}", ex.name);
            assert_eq!(cfg.line_tension, want.line_tension, "{}", ex.name);
            assert_eq!(cfg.spont_curvature, want.spont_curvature, "{}", ex.name);

            // The generator + seed sources are populated (drive the demo).
            assert!(state.generate.editor.source.contains("soupGen"), "{}", ex.name);
            assert!(
                matches!(state.seed.strategy, SeedStrategy::BuiltIn(_)),
                "{}: seed strategy",
                ex.name
            );
            assert!(!state.seed.editor.source.is_empty(), "{}: seed source", ex.name);

            // Generate + Layout panels open so the demo is ready to drive.
            assert!(state.is_section_open(Section::Generate), "{}", ex.name);
            assert!(state.is_section_open(Section::Layout), "{}", ex.name);
        }
    }

    /// Each example must survive the full share-link codec (the format the
    /// Examples picker hands to the loader): encode → decode is identity on the
    /// persisted subset.
    #[test]
    fn examples_roundtrip_through_share_codec() {
        for ex in catalog() {
            let original = ex.build_state();
            let hash = ex.share_hash().unwrap_or_else(|e| panic!("{}: encode: {e}", ex.name));
            let back = crate::ui::share::decode(&hash)
                .unwrap_or_else(|e| panic!("{}: decode: {e}", ex.name));
            assert_eq!(
                crate::ui::state::to_json_sanitized(&original).unwrap(),
                crate::ui::state::to_json_sanitized(&back).unwrap(),
                "{}: share round-trip",
                ex.name
            );
        }
    }

    /// Each example's staged generator expression must actually evaluate to a
    /// valid (unbonded) soup graph through tvix.
    #[test]
    fn example_generators_evaluate() {
        for ex in catalog() {
            let g = tvix_wasm::eval_graph(&ex.generator_expr())
                .unwrap_or_else(|e| panic!("{}: generator eval: {e}", ex.name));
            assert_eq!(g.nodes.len(), ex.soup_nodes, "{}: node count", ex.name);
            assert!(g.edges.is_empty(), "{}: soup must be unbonded", ex.name);
        }
    }

    /// Each example's seed expression must evaluate to n positions for a
    /// representative node count.
    #[test]
    fn example_seeds_evaluate() {
        for ex in catalog() {
            let pts = tvix_wasm::eval_seed(&ex.seed_expr(), 24)
                .unwrap_or_else(|e| panic!("{}: seed eval: {e}", ex.name));
            assert_eq!(pts.len(), 24, "{}: seed must return n positions", ex.name);
        }
    }
}
