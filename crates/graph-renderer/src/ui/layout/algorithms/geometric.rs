//! UI factory + widgets for the `geometric` constraint engine.

use std::sync::{Arc, Mutex};
use graph_layouts::{
    BoxedPhysics, DynPhysicsLayout, Graph, LayoutDescriptor, LayoutId, LayoutKind,
    LayoutRequirements, PhysicsLayout,
};
use serde_json::Value;

use crate::ui::layout::registry::LayoutFactory;
use crate::ui::sections::{reset_row, row, subgroup_label, subgroup_separator};

use graph_layouts::geometric::{ClassLens, CoordinationLens, EdgeLengthLens, LensConfig, MassLens};

const LAYOUT_ID: LayoutId = "geometric";

pub fn factory() -> LayoutFactory {
    LayoutFactory::Physics {
        descriptor: <RemoteGeometricLayout as PhysicsLayout>::descriptor(),
        build: build_layout,
        default_settings: default_settings_json,
        ui: render_ui,
    }
}

fn default_settings_json() -> Value {
    // `mut` is only needed on the wasm32 branch below (which overrides
    // `config.url`); on native nothing mutates it.
    #[cfg_attr(not(target_arch = "wasm32"), allow(unused_mut))]
    let mut config = LensConfig::default();
    // On WASM, default to the same host we were loaded from.
    #[cfg(target_arch = "wasm32")]
    {
        if let Some(window) = web_sys::window() {
            if let Ok(location) = window.location().origin() {
                let ws_protocol = if location.starts_with("https") { "wss" } else { "ws" };
                let host = location.strip_prefix("http://").or_else(|| location.strip_prefix("https://")).unwrap_or(&location);
                config.url = format!("{}://{}/graph/layout/stream", ws_protocol, host);
            }
        }
    }
    serde_json::to_value(config).unwrap_or(Value::Null)
}

fn build_layout(json: &Value) -> Box<dyn DynPhysicsLayout> {
    let s: LensConfig = serde_json::from_value(json.clone()).unwrap_or_default();
    Box::new(BoxedPhysics::new(RemoteGeometricLayout::create(s)))
}

type Latch = Arc<Mutex<Option<Vec<f32>>>>;

pub struct RemoteGeometricLayout {
    settings: LensConfig,
    latch: Latch,
    n_nodes: u32,
    spawned_url: Option<String>,
}

impl RemoteGeometricLayout {
    fn create(settings: LensConfig) -> Self {
        Self {
            settings,
            latch: Arc::new(Mutex::new(None)),
            n_nodes: 0,
            spawned_url: None,
        }
    }
}

impl RemoteGeometricLayout {
    /// The stream URL implied by the *current* settings. The worker engine is
    /// self-selected from `?layout_id=` (`geometric` vs `geometric-gpu`), and
    /// the resolved lens rides in `?lens=`, so this string fully determines
    /// which backend + parameters the stream produces.
    fn stream_url(&self) -> String {
        let backend_id = if self.settings.use_gpu { "geometric-gpu" } else { "geometric" };
        let lens_json = serde_json::to_string(&self.settings).unwrap_or_default();
        let encoded_lens = urlencoding::encode(&lens_json);
        format!("{}?layout_id={}&lens={}", self.settings.url, backend_id, encoded_lens)
    }

    /// Spawn the WS consumer for `url` unless it already matches the one we
    /// last spawned. Returns immediately on a no-op. This is the single place
    /// the bridge (re)opens a stream, so both `init_with_device` and a
    /// settings-only change (CPU↔GPU toggle, lens/preset edits) route through
    /// it and correctly respawn against the new `?layout_id=`/`?lens=`.
    fn ensure_stream(&mut self) {
        if self.n_nodes == 0 {
            return; // not initialised against a graph yet
        }
        let url = self.stream_url();
        if self.spawned_url.as_deref() == Some(url.as_str()) {
            return;
        }
        self.spawned_url = Some(url.clone());
        let backoff_ms = self.settings.reconnect_backoff_ms.max(100);
        let latch = Arc::clone(&self.latch);
        crate::ui::layout::algorithms::remote_fa2::spawn_ws_consumer(url, backoff_ms, latch);
    }
}

impl PhysicsLayout for RemoteGeometricLayout {
    type Settings = LensConfig;

    fn descriptor() -> LayoutDescriptor {
        LayoutDescriptor {
            id: LAYOUT_ID,
            kind: LayoutKind::Physics,
            display_name: "Geometric Engine",
            description: "Remote geometric constraint solver via graph-api. Supports both CPU \
                          and GPU-accelerated backends.",
            requirements: LayoutRequirements {
                needs_edges: false,
                needs_cpu_positions: false,
                needs_gpu_positions_buffer: true,
            },
        }
    }

    fn new(settings: Self::Settings) -> Self { Self::create(settings) }

    fn init_with_device(
        &mut self,
        _device: &wgpu::Device,
        _queue: &wgpu::Queue,
        graph: &Graph,
        _positions_buf: &wgpu::Buffer,
    ) -> Result<(), String> {
        self.n_nodes = graph.nodes.len() as u32;
        self.ensure_stream();
        Ok(())
    }

    fn step_with_encoder(
        &mut self,
        _device: &wgpu::Device,
        queue: &wgpu::Queue,
        _encoder: &mut wgpu::CommandEncoder,
        positions_buf: &wgpu::Buffer,
    ) {
        // Respawn the stream if the effective URL changed since we last spawned.
        // The renderer pushes a settings-only update (no layout re-init) when the
        // active layout id is unchanged — which is exactly the case when toggling
        // CPU↔GPU (both are the `geometric` bridge) or editing the lens. Without
        // this, `use_gpu`/lens edits would never reach a new `?layout_id=`/`?lens=`
        // stream and the backend swap would be silently ignored.
        self.ensure_stream();

        let positions = match self.latch.lock() {
            Ok(mut g) => g.take(),
            Err(_) => return,
        };
        let Some(positions) = positions else { return };
        if positions.len() == 3 * (self.n_nodes as usize) && self.n_nodes > 0 {
            queue.write_buffer(positions_buf, 0, bytemuck::cast_slice(&positions));
        }
    }

    fn set_settings(&mut self, settings: Self::Settings) { self.settings = settings; }
    fn settings(&self) -> &Self::Settings { &self.settings }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LensPreset {
    CrystallizeMotifs,
    SeparateCommunities,
    CorePeriphery,
    Molecular,
}

impl LensPreset {
    pub fn label(self) -> &'static str {
        match self {
            LensPreset::CrystallizeMotifs => "Crystallize motifs",
            LensPreset::SeparateCommunities => "Separate communities",
            LensPreset::CorePeriphery => "Core–periphery",
            LensPreset::Molecular => "Molecular",
        }
    }

    pub fn apply_to(self, c: &mut LensConfig) {
        match self {
            LensPreset::CrystallizeMotifs => {
                c.class = ClassLens::Uniform;
                c.coordination = CoordinationLens::Degree;
                c.angle_stiffness = 0.5;
            }
            LensPreset::SeparateCommunities => {
                c.class = ClassLens::Louvain;
                c.affinity_strength = -50.0;
                c.angle_stiffness = 0.01;
                // De-hairball: let global shortcut edges stretch so communities
                // can pull apart (small-world layout fix).
                c.edge_length = EdgeLengthLens::JaccardStrength;
                c.edge_strength_spread = 3.0;
            }
            LensPreset::CorePeriphery => {
                c.mass = MassLens::PageRank;
                c.gravity = 0.05;
            }
            LensPreset::Molecular => {
                c.angle_stiffness = 1.0;
            }
        }
    }
}

/// Self-assembly example presets — each sets `bonding_enabled` plus the
/// PARAMETER REGIME validated in `crates/graph-compute/tests/geometric_solver.rs`
/// (`soup_settings` / `p2_*` / `p3_*`) for a morphology on the dynamic-edge
/// engine. These map onto the lipid → sheet → tube → sphere demo ladder
/// (`docs/dynamic-edge-bonding-plan.md` §5).
///
/// HONESTY: chains (valence-2) and honeycomb sheets (valence-3 @120°) form
/// SPONTANEOUSLY from a soup in the validated budget. Full tube/vesicle closure
/// is a kinetic trap in a single-leaflet point model — the tube/vesicle presets
/// dial in the validated curvature / rim-line-tension regime (P3
/// `p3_line_tension_closes_a_seeded_disk`, γ=4, c₀=0.5) that FOLDS a seeded
/// bonded patch toward closure; spontaneous soup→vesicle closure is logged as
/// not reached. Pair the tube/vesicle preset with a sheet/disk Initial-seed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SelfAssemblyPreset {
    LipidChain,
    HoneycombSheet,
    Tube,
    Vesicle,
}

impl SelfAssemblyPreset {
    pub const ALL: [SelfAssemblyPreset; 4] = [
        SelfAssemblyPreset::LipidChain,
        SelfAssemblyPreset::HoneycombSheet,
        SelfAssemblyPreset::Tube,
        SelfAssemblyPreset::Vesicle,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SelfAssemblyPreset::LipidChain => "Lipid chains (valence 2)",
            SelfAssemblyPreset::HoneycombSheet => "Honeycomb sheet (valence 3 @120°)",
            SelfAssemblyPreset::Tube => "Tube (curved sheet)",
            SelfAssemblyPreset::Vesicle => "Vesicle (rim seam + curvature)",
        }
    }

    pub fn tooltip(self) -> &'static str {
        match self {
            SelfAssemblyPreset::LipidChain => {
                "P2 valence-2 @180°. Spontaneous from a cohering Brownian soup."
            }
            SelfAssemblyPreset::HoneycombSheet => {
                "P2 valence-3 @120° + membrane flattening (anisotropy + GB-side + \
                 tilt). Honeycomb patches form spontaneously."
            }
            SelfAssemblyPreset::Tube => {
                "Sheet regime + spontaneous curvature → a rolled tube. Seed a \
                 disk/sheet; full closure is a kinetic trap (logged)."
            }
            SelfAssemblyPreset::Vesicle => {
                "P3 rim line-tension (γ=4) + curvature (c₀=0.5): folds a seeded \
                 bonded disk toward a shell. Spontaneous soup→vesicle not reached \
                 (logged) — seed a disk."
            }
        }
    }

    /// Apply the validated self-assembly regime to a [`LensConfig`]. All values
    /// are the ones the `graph-compute` solver canaries validate for the
    /// morphology (see the doc comment on the enum).
    pub fn apply_to(self, c: &mut LensConfig) {
        // Common dynamic-bond + Brownian-soup base (matches the validated
        // `soup_settings` regime: σ=1.0 contact, cohesion well, thermostat).
        c.bonding_enabled = true;
        c.exclusion_strength = 1.0;
        c.gravity = 0.1;
        c.r_bond = 1.1; // just past contact σ=1.0
        c.r_break = 1.5; // ≈1.36·r_bond hysteresis band
        c.bond_stiffness = 0.4;
        c.bond_every = 4;
        c.well_depth = 2.0;
        c.well_width = 1.0;
        c.temperature = 0.2;
        // Default the membrane terms OFF; the sheet/tube/vesicle regimes turn
        // them on below.
        c.anisotropy_strength = 0.0;
        c.gb_side_strength = 0.0;
        c.tilt_coupling_strength = 0.0;
        c.spont_curvature = 0.0;
        c.line_tension = 0.0;

        match self {
            // P2 valence-2 @180° (soup_settings + cap 2, angle 0.15, 180°).
            SelfAssemblyPreset::LipidChain => {
                c.default_max_valence = 2;
                c.default_bond_angle = 180.0;
                c.angle_stiffness = 0.15;
            }
            // P2 valence-3 @120° honeycomb (p2_valence_three_120deg...).
            SelfAssemblyPreset::HoneycombSheet => {
                c.default_max_valence = 3;
                c.default_bond_angle = 120.0;
                c.angle_stiffness = 0.3;
                c.well_depth = 2.5;
                c.anisotropy_strength = 1.0;
                c.gb_side_strength = 1.5;
                c.tilt_coupling_strength = 1.0;
                c.spont_curvature = 0.0; // flat target
                c.gravity = 0.05;
            }
            // Sheet regime + spontaneous curvature (rolls a sheet into a tube).
            SelfAssemblyPreset::Tube => {
                c.default_max_valence = 3;
                c.default_bond_angle = 120.0;
                c.angle_stiffness = 0.3;
                c.well_depth = 2.5;
                c.anisotropy_strength = 1.0;
                c.gb_side_strength = 1.5;
                c.tilt_coupling_strength = 1.0;
                c.spont_curvature = 0.25; // intermediate c₀ → tube curvature
                c.gravity = 0.05;
            }
            // P3 rim line-tension + curvature (p3_line_tension_closes_a_seeded_disk
            // validated γ=4, c₀=0.5: folds a seeded bonded disk toward a shell).
            SelfAssemblyPreset::Vesicle => {
                c.default_max_valence = 3;
                c.default_bond_angle = 120.0;
                c.angle_stiffness = 0.3;
                c.well_depth = 2.5;
                c.anisotropy_strength = 1.0;
                c.gb_side_strength = 1.5;
                c.tilt_coupling_strength = 1.0;
                c.spont_curvature = 0.5; // validated closure curvature
                c.line_tension = 4.0; // validated rim seam strength
                c.gravity = 0.05;
            }
        }
    }
}

fn render_ui(ui: &mut egui::Ui, json: &mut Value) {
    let mut opts: LensConfig = serde_json::from_value(json.clone()).unwrap_or_default();
    let mut changed = false;

    if reset_row(ui) {
        opts = LensConfig::default();
        changed = true;
    }

    subgroup_label(ui, "Connection");
    row(ui, "URL", |ui| {
        if ui.add(egui::TextEdit::singleline(&mut opts.url).desired_width(f32::INFINITY)).changed() { changed = true; }
    });
    row(ui, "Reconnect", |ui| {
        if ui.add(egui::DragValue::new(&mut opts.reconnect_backoff_ms).range(100..=30000).suffix("ms")).changed() { changed = true; }
    });
    // NOTE: GPU / Multilevel are no longer checkboxes here — they are now
    // distinct entries in the unified Engine picker (geometric-gpu, etc.).
    // The fields are still set on `LensConfig` by that picker.

    subgroup_separator(ui);

    subgroup_label(ui, "Presets");
    ui.horizontal_wrapped(|ui| {
        for preset in [
            LensPreset::CrystallizeMotifs,
            LensPreset::SeparateCommunities,
            LensPreset::CorePeriphery,
            LensPreset::Molecular,
        ] {
            if ui.button(preset.label()).clicked() {
                preset.apply_to(&mut opts);
                changed = true;
            }
        }
    });

    subgroup_separator(ui);

    subgroup_label(ui, "Roles");
    row(ui, "Class", |ui| {
        egui::ComboBox::from_id_salt("class-lens")
            .selected_text(format!("{:?}", opts.class))
            .show_ui(ui, |ui| {
                if ui.selectable_value(&mut opts.class, ClassLens::Uniform, "Uniform").clicked() { changed = true; }
                if ui.selectable_value(&mut opts.class, ClassLens::DegreeBuckets, "DegreeBuckets").clicked() { changed = true; }
                if ui.selectable_value(&mut opts.class, ClassLens::Louvain, "Louvain").clicked() { changed = true; }
            });
    });
    row(ui, "Coordination", |ui| {
        egui::ComboBox::from_id_salt("coord-lens")
            .selected_text(format!("{:?}", opts.coordination))
            .show_ui(ui, |ui| {
                if ui.selectable_value(&mut opts.coordination, CoordinationLens::Degree, "Degree").clicked() { changed = true; }
                if ui.selectable_value(&mut opts.coordination, CoordinationLens::Uniform(0), "Uniform").clicked() { changed = true; }
            });
    });
    row(ui, "Mass", |ui| {
        egui::ComboBox::from_id_salt("mass-lens")
            .selected_text(format!("{:?}", opts.mass))
            .show_ui(ui, |ui| {
                if ui.selectable_value(&mut opts.mass, MassLens::Uniform, "Uniform").clicked() { changed = true; }
                if ui.selectable_value(&mut opts.mass, MassLens::Degree, "Degree").clicked() { changed = true; }
                if ui.selectable_value(&mut opts.mass, MassLens::PageRank, "PageRank").clicked() { changed = true; }
            });
    });
    row(ui, "Edge Length", |ui| {
        egui::ComboBox::from_id_salt("edge-len-lens")
            .selected_text(format!("{:?}", opts.edge_length))
            .show_ui(ui, |ui| {
                if ui.selectable_value(&mut opts.edge_length, EdgeLengthLens::Uniform, "Uniform").clicked() { changed = true; }
                if ui.selectable_value(&mut opts.edge_length, EdgeLengthLens::Weight, "Weight").clicked() { changed = true; }
                if ui.selectable_value(&mut opts.edge_length, EdgeLengthLens::EdgeType, "EdgeType").clicked() { changed = true; }
                if ui.selectable_value(&mut opts.edge_length, EdgeLengthLens::JaccardStrength, "Strength (Jaccard)").on_hover_text("De-hairball: short rest length for intra-cluster edges, long for global shortcuts.").clicked() { changed = true; }
                if ui.selectable_value(&mut opts.edge_length, EdgeLengthLens::CorrectedOverlapStrength, "Strength (corrected)").on_hover_text("Edge strength via Batagelj corrected overlap; damps tiny-dense-subgraph over-emphasis.").clicked() { changed = true; }
            });
    });
    // Stretch factor only matters for the structural-strength edge-length lenses.
    if matches!(
        opts.edge_length,
        EdgeLengthLens::JaccardStrength | EdgeLengthLens::CorrectedOverlapStrength
    ) {
        row(ui, "Strength Spread", |ui| {
            if ui.add(egui::Slider::new(&mut opts.edge_strength_spread, 0.0..=8.0)).on_hover_text("Shortcut edges target rest_len·(1+spread); 0 disables the de-hairball effect.").changed() { changed = true; }
        });
    }

    subgroup_separator(ui);

    subgroup_label(ui, "Physics");
    row(ui, "Exclusion", |ui| {
        if ui.add(egui::Slider::new(&mut opts.exclusion_strength, 0.1..=10000.0).logarithmic(true)).changed() { changed = true; }
    });
    row(ui, "Affinity", |ui| {
        if ui.add(egui::Slider::new(&mut opts.affinity_strength, -100.0..=100.0)).changed() { changed = true; }
    });
    row(ui, "Edge Stiffness", |ui| {
        if ui.add(egui::Slider::new(&mut opts.edge_stiffness, 0.0..=1.0)).changed() { changed = true; }
    });
    row(ui, "Angle Stiffness", |ui| {
        if ui.add(egui::Slider::new(&mut opts.angle_stiffness, 0.0..=1.0)).changed() { changed = true; }
    });
    row(ui, "Gravity", |ui| {
        if ui.add(egui::Slider::new(&mut opts.gravity, 0.0..=0.1)).changed() { changed = true; }
    });

    subgroup_separator(ui);

    // ── Self-assembly (dynamic bonding) ──────────────────────────────────
    subgroup_label(ui, "Self-assembly");
    row(ui, "Bonding", |ui| {
        if ui
            .checkbox(&mut opts.bonding_enabled, "enabled")
            .on_hover_text(
                "Add/remove edges (bonds) each step under a proximity + class \
                 + valence + angle constraint, so chains → sheets → tubes → \
                 vesicles emerge on an evolving graph. OFF = byte-identical \
                 default engine behaviour.",
            )
            .changed()
        {
            changed = true;
        }
    });

    // Example presets: lipid → sheet → tube → sphere. Each sets the validated
    // parameter regime (and enables bonding).
    ui.horizontal_wrapped(|ui| {
        for preset in SelfAssemblyPreset::ALL {
            if ui.button(preset.label()).on_hover_text(preset.tooltip()).clicked() {
                preset.apply_to(&mut opts);
                changed = true;
            }
        }
    });

    if opts.bonding_enabled {
        row(ui, "r_bond", |ui| {
            if ui.add(egui::Slider::new(&mut opts.r_bond, 0.5..=3.0).suffix("σ")).on_hover_text("Bond creation cutoff: a compatible pair closer than this bonds.").changed() { changed = true; }
        });
        row(ui, "r_break", |ui| {
            if ui.add(egui::Slider::new(&mut opts.r_break, 0.5..=4.0)).on_hover_text("Bond break cutoff (hysteresis ≈1.2–1.5·r_bond, prevents flicker).").changed() { changed = true; }
        });
        row(ui, "Bond stiffness", |ui| {
            if ui.add(egui::Slider::new(&mut opts.bond_stiffness, 0.0..=1.0)).changed() { changed = true; }
        });
        row(ui, "Bond every", |ui| {
            if ui.add(egui::DragValue::new(&mut opts.bond_every).range(1..=64)).on_hover_text("Rebuild the bond set every N steps (Verlet amortisation).").changed() { changed = true; }
        });
        row(ui, "Max valence", |ui| {
            if ui.add(egui::DragValue::new(&mut opts.default_max_valence).range(0..=8)).on_hover_text("Per-node bond cap: 0=uncapped, 2=chain, 3=honeycomb, 4=square net.").changed() { changed = true; }
        });
        row(ui, "Bond angle", |ui| {
            if ui.add(egui::Slider::new(&mut opts.default_bond_angle, 60.0..=180.0).suffix("°")).on_hover_text("Target angle between a node's bonds: 180=chain, 120=honeycomb, 90=square.").changed() { changed = true; }
        });
        row(ui, "Line tension", |ui| {
            if ui.add(egui::Slider::new(&mut opts.line_tension, 0.0..=8.0)).on_hover_text("Rim seam force on under-coordinated boundary nodes — closes an open sheet (needs a valence cap). 0=OFF.").changed() { changed = true; }
        });
        row(ui, "Spont. curvature", |ui| {
            if ui.add(egui::Slider::new(&mut opts.spont_curvature, 0.0..=1.0)).on_hover_text("Preferred tilt across each bond (radians): 0=flat, intermediate=tube, higher=vesicle.").changed() { changed = true; }
        });

        subgroup_label(ui, "Membrane");
        row(ui, "Well depth", |ui| {
            if ui.add(egui::Slider::new(&mut opts.well_depth, 0.0..=5.0)).on_hover_text("Cooke–Deserno cohesion-well depth ε — condenses the soup so bonds can form. 0=no cohesion.").changed() { changed = true; }
        });
        row(ui, "Well width", |ui| {
            if ui.add(egui::Slider::new(&mut opts.well_width, 0.5..=2.5)).changed() { changed = true; }
        });
        row(ui, "Temperature", |ui| {
            if ui.add(egui::Slider::new(&mut opts.temperature, 0.0..=1.0)).on_hover_text("Langevin kT — the Brownian drive self-assembly emerges from. 0=deterministic minimizer.").changed() { changed = true; }
        });
        row(ui, "Anisotropy", |ui| {
            if ui.add(egui::Slider::new(&mut opts.anisotropy_strength, 0.0..=3.0)).on_hover_text("Patchy-well orientation anisotropy — drives nematic/membrane order.").changed() { changed = true; }
        });
        row(ui, "GB side bias", |ui| {
            if ui.add(egui::Slider::new(&mut opts.gb_side_strength, 0.0..=3.0)).on_hover_text("Gay–Berne side-by-side packing bias — flat lamella over a droplet.").changed() { changed = true; }
        });
        row(ui, "Tilt coupling", |ui| {
            if ui.add(egui::Slider::new(&mut opts.tilt_coupling_strength, 0.0..=3.0)).on_hover_text("Director→position coupling — turns the orientational preference into real (flat/curved) geometry.").changed() { changed = true; }
        });
    }

    if changed {
        if let Ok(v) = serde_json::to_value(&opts) {
            *json = v;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every self-assembly preset must enable bonding and round-trip through the
    /// JSON `Value` form `AppState.layout.settings` stores — i.e. a preset
    /// deserializes back into a `LensConfig` with `bonding_enabled == true` and
    /// its knobs intact. This is the unit-test stand-in for the (headless-
    /// unverifiable) UI click-through.
    #[test]
    fn presets_enable_bonding_and_roundtrip() {
        for preset in SelfAssemblyPreset::ALL {
            let mut cfg = LensConfig::default();
            assert!(!cfg.bonding_enabled, "default lens must start OFF");
            preset.apply_to(&mut cfg);
            assert!(
                cfg.bonding_enabled,
                "{:?} must enable bonding",
                preset
            );

            // Round-trip through the Value form the UI persists.
            let v = serde_json::to_value(&cfg).unwrap();
            let back: LensConfig = serde_json::from_value(v).unwrap();
            assert_eq!(back, cfg, "{:?} must survive the JSON round-trip", preset);
        }
    }

    /// The presets carry the PARAMETER REGIMES validated by the graph-compute
    /// solver canaries (`p2_*` / `p3_*` in `tests/geometric_solver.rs`). Pinning
    /// the load-bearing knobs here guards against silent drift away from the
    /// validated values.
    #[test]
    fn preset_values_match_validated_regimes() {
        // P2 valence-2 chain (soup_settings + cap 2, angle 0.15 @180°).
        let mut chain = LensConfig::default();
        SelfAssemblyPreset::LipidChain.apply_to(&mut chain);
        assert_eq!(chain.default_max_valence, 2);
        assert_eq!(chain.default_bond_angle, 180.0);
        assert_eq!(chain.angle_stiffness, 0.15);
        assert_eq!(chain.r_bond, 1.1);
        assert_eq!(chain.r_break, 1.5);
        assert_eq!(chain.bond_stiffness, 0.4);
        assert_eq!(chain.bond_every, 4);
        assert_eq!(chain.well_depth, 2.0);
        assert_eq!(chain.temperature, 0.2);
        assert_eq!(chain.line_tension, 0.0);
        assert_eq!(chain.spont_curvature, 0.0);

        // P2 valence-3 @120° honeycomb sheet (membrane regime, flat).
        let mut sheet = LensConfig::default();
        SelfAssemblyPreset::HoneycombSheet.apply_to(&mut sheet);
        assert_eq!(sheet.default_max_valence, 3);
        assert_eq!(sheet.default_bond_angle, 120.0);
        assert_eq!(sheet.angle_stiffness, 0.3);
        assert_eq!(sheet.well_depth, 2.5);
        assert_eq!(sheet.anisotropy_strength, 1.0);
        assert_eq!(sheet.gb_side_strength, 1.5);
        assert_eq!(sheet.tilt_coupling_strength, 1.0);
        assert_eq!(sheet.spont_curvature, 0.0); // flat
        assert_eq!(sheet.line_tension, 0.0);

        // Tube: sheet regime + intermediate spontaneous curvature, no seam.
        let mut tube = LensConfig::default();
        SelfAssemblyPreset::Tube.apply_to(&mut tube);
        assert_eq!(tube.default_max_valence, 3);
        assert!(
            tube.spont_curvature > 0.0 && tube.spont_curvature < 0.5,
            "tube curvature should be intermediate, got {}",
            tube.spont_curvature
        );
        assert_eq!(tube.line_tension, 0.0, "a tube rolls, it does not seam");

        // Vesicle: P3 validated rim line-tension (γ=4) + curvature (c₀=0.5).
        let mut vesicle = LensConfig::default();
        SelfAssemblyPreset::Vesicle.apply_to(&mut vesicle);
        assert_eq!(vesicle.default_max_valence, 3);
        assert_eq!(vesicle.line_tension, 4.0);
        assert_eq!(vesicle.spont_curvature, 0.5);
        assert_eq!(vesicle.well_depth, 2.5);
        assert_eq!(vesicle.tilt_coupling_strength, 1.0);
    }
}
