//! UI factory + widgets for the `geometric` constraint engine.

use std::sync::{Arc, Mutex};
use eframe::egui;
use graph_layouts::{
    BoxedPhysics, DynPhysicsLayout, Graph, LayoutDescriptor, LayoutId, LayoutKind,
    LayoutRequirements, PhysicsLayout,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ui::layout::registry::LayoutFactory;
use crate::ui::sections::{reset_row, row, subgroup_label, subgroup_separator};

const LAYOUT_ID: LayoutId = "geometric";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", content = "value")]
pub enum ClassLens {
    Uniform,
    DegreeBuckets,
    Louvain,
    Field(String),
    Tag(String),
    NodeType,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", content = "value")]
pub enum CoordinationLens {
    Degree,
    Uniform(u32),
    Field(String),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", content = "value")]
pub enum MassLens {
    Uniform,
    Degree,
    PageRank,
    Field(String),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", content = "value")]
pub enum EdgeLengthLens {
    Uniform,
    Weight,
    EdgeType,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct LensConfig {
    pub url: String,
    pub reconnect_backoff_ms: u32,
    
    pub class: ClassLens,
    pub coordination: CoordinationLens,
    pub mass: MassLens,
    pub edge_length: EdgeLengthLens,

    pub edge_stiffness: f32,
    pub angle_stiffness: f32,
    pub exclusion_strength: f32,
    pub affinity_strength: f32,
    pub gravity: f32,
    pub coordination_angles: Vec<f32>,
    pub class_radius: Vec<f32>,
    pub class_affinity: Vec<f32>,
}

impl Default for LensConfig {
    fn default() -> Self {
        Self {
            url: "ws://127.0.0.1:8080/graph/layout/stream".to_string(),
            reconnect_backoff_ms: 1000,
            class: ClassLens::Uniform,
            coordination: CoordinationLens::Uniform(0),
            mass: MassLens::Uniform,
            edge_length: EdgeLengthLens::Uniform,
            edge_stiffness: 0.1,
            angle_stiffness: 0.05,
            exclusion_strength: 100.0,
            affinity_strength: 0.0,
            gravity: 0.005,
            coordination_angles: vec![],
            class_radius: vec![],
            class_affinity: vec![],
        }
    }
}

pub fn factory() -> LayoutFactory {
    LayoutFactory::Physics {
        descriptor: <RemoteGeometricLayout as PhysicsLayout>::descriptor(),
        build: build_layout,
        default_settings: default_settings_json,
        ui: render_ui,
    }
}

fn default_settings_json() -> Value {
    serde_json::to_value(LensConfig::default()).unwrap_or(Value::Null)
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

impl PhysicsLayout for RemoteGeometricLayout {
    type Settings = LensConfig;

    fn descriptor() -> LayoutDescriptor {
        LayoutDescriptor {
            id: LAYOUT_ID,
            kind: LayoutKind::Physics,
            display_name: "Geometric Engine",
            description: "Remote geometric constraint solver via graph-api.",
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
        let url = self.settings.url.clone();
        if self.spawned_url.as_deref() == Some(url.as_str()) {
            return Ok(());
        }
        self.spawned_url = Some(url.clone());
        let backoff_ms = self.settings.reconnect_backoff_ms.max(100);
        let latch = Arc::clone(&self.latch);
        // We reuse the remote_fa2 WS consumer logic here since it's identical
        // Phase D will customize the connection to send the layout config.
        crate::ui::layout::algorithms::remote_fa2::spawn_ws_consumer(url, backoff_ms, latch);
        Ok(())
    }

    fn step_with_encoder(
        &mut self,
        _device: &wgpu::Device,
        queue: &wgpu::Queue,
        _encoder: &mut wgpu::CommandEncoder,
        positions_buf: &wgpu::Buffer,
    ) {
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

fn render_ui(ui: &mut egui::Ui, json: &mut Value) {
    let mut opts: LensConfig = serde_json::from_value(json.clone()).unwrap_or_default();
    let mut changed = false;

    if reset_row(ui) {
        opts = LensConfig::default();
        changed = true;
    }

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
            });
    });

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

    if changed {
        if let Ok(v) = serde_json::to_value(&opts) {
            *json = v;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lens_config_serde_roundtrip() {
        let mut config = LensConfig::default();
        config.class = ClassLens::Louvain;
        config.exclusion_strength = 1337.0;

        let json = serde_json::to_string(&config).unwrap();
        let decoded: LensConfig = serde_json::from_str(&json).unwrap();
        
        assert_eq!(decoded.class, ClassLens::Louvain);
        assert_eq!(decoded.exclusion_strength, 1337.0);
    }
}
