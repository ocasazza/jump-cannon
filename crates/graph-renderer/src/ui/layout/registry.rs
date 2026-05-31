//! Registry of layout algorithms exposed to the sidebar.
//!
//! Step 1 only registers the gpu-force physics layout — but the registry
//! shape (Static + Physics factory variants, JSON-keyed settings, per-id
//! UI fn) is the seam Steps 2/3 plug additional layouts into.

use std::collections::BTreeMap;

use eframe::egui;
use graph_layouts::{
    DynPhysicsLayout, DynStaticLayout, LayoutDescriptor, LayoutId, LayoutKind,
};
use serde_json::Value;

/// Factory entry for one registered layout. Holds both the descriptor and
/// the closures the renderer needs to construct / mutate it.
pub enum LayoutFactory {
    Static {
        descriptor: LayoutDescriptor,
        build: fn() -> Box<dyn DynStaticLayout>,
        default_settings: fn() -> Value,
        ui: fn(&mut egui::Ui, &mut Value),
    },
    Physics {
        descriptor: LayoutDescriptor,
        build: fn(&Value) -> Box<dyn DynPhysicsLayout>,
        default_settings: fn() -> Value,
        ui: fn(&mut egui::Ui, &mut Value),
    },
}

impl LayoutFactory {
    pub fn id(&self) -> LayoutId { self.descriptor().id }

    pub fn descriptor(&self) -> &LayoutDescriptor {
        match self {
            LayoutFactory::Static { descriptor, .. } => descriptor,
            LayoutFactory::Physics { descriptor, .. } => descriptor,
        }
    }

    pub fn kind(&self) -> LayoutKind { self.descriptor().kind }

    pub fn default_settings(&self) -> Value {
        match self {
            LayoutFactory::Static { default_settings, .. } => default_settings(),
            LayoutFactory::Physics { default_settings, .. } => default_settings(),
        }
    }

    pub fn ui(&self, ui: &mut egui::Ui, json: &mut Value) {
        match self {
            LayoutFactory::Static { ui: f, .. } => f(ui, json),
            LayoutFactory::Physics { ui: f, .. } => f(ui, json),
        }
    }
}

/// In-memory registry of layout factories. Insertion order is preserved
/// for the sidebar ComboBox.
pub struct LayoutRegistry {
    by_id: BTreeMap<LayoutId, LayoutFactory>,
    order: Vec<LayoutId>,
}

impl LayoutRegistry {
    pub fn new() -> Self {
        Self {
            by_id: BTreeMap::new(),
            order: Vec::new(),
        }
    }

    pub fn register(&mut self, factory: LayoutFactory) {
        let id = factory.id();
        if self.by_id.insert(id, factory).is_none() {
            self.order.push(id);
        }
    }

    pub fn get(&self, id: &str) -> Option<&LayoutFactory> {
        self.by_id.get(id)
    }

    pub fn iter(&self) -> impl Iterator<Item = &LayoutFactory> {
        self.order.iter().filter_map(move |id| self.by_id.get(id))
    }

    /// Seed the registry with the default set: the gpu-force physics
    /// backend plus the Step-3 stub static layouts (Random, Circle).
    pub fn seed_default() -> Self {
        let mut r = Self::new();
        r.register(super::algorithms::gpu_force::factory());
        r.register(super::algorithms::random::factory());
        r.register(super::algorithms::circle::factory());
        r.register(super::algorithms::grid::factory());
        r.register(super::algorithms::sphere::factory());
        r.register(super::algorithms::concentric::factory());
        r.register(super::algorithms::hilbert::factory());
        r.register(super::algorithms::force_atlas2::factory());
        r.register(super::algorithms::fcose::factory());
        r.register(super::algorithms::cose_bilkent::factory());
        r.register(super::algorithms::cise::factory());
        r.register(super::algorithms::dagre::factory());
        r.register(super::algorithms::klay::factory());
        r.register(super::algorithms::remote_fa2::factory());
        r.register(super::algorithms::geometric::factory());
        r
    }
}

impl Default for LayoutRegistry {
    fn default() -> Self { Self::seed_default() }
}
