//! Layout abstraction.
//!
//! Object-safe `DynStaticLayout` / `DynPhysicsLayout` trait objects are the
//! integration surface for the renderer-side registry. Algorithm crates
//! implement the typed `StaticLayout` / `PhysicsLayout` traits and wrap
//! themselves in `BoxedStatic` / `BoxedPhysics` to land in a `Box<dyn …>`.

use std::marker::PhantomData;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::types::Graph;

/// Stable string identifier for a registered layout. Used as the registry
/// key and persisted in `LayoutState::active`.
pub type LayoutId = &'static str;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LayoutKind {
    /// One-shot solver — runs once on the CPU and writes positions.
    Static,
    /// Continuous physics sim — driven per-frame from the GPU compute queue.
    Physics,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct LayoutRequirements {
    pub needs_edges: bool,
    pub needs_cpu_positions: bool,
    pub needs_gpu_positions_buffer: bool,
}

#[derive(Clone, Debug)]
pub struct LayoutDescriptor {
    pub id: LayoutId,
    pub kind: LayoutKind,
    pub display_name: &'static str,
    pub description: &'static str,
    pub requirements: LayoutRequirements,
}

/// Marker for serde-roundtrippable layout settings types. Blanket impl —
/// any `Serialize + DeserializeOwned + Default + Clone + Send + Sync +
/// 'static` type qualifies.
pub trait LayoutSettings:
    Serialize + DeserializeOwned + Default + Clone + Send + Sync + 'static
{
}
impl<T> LayoutSettings for T where
    T: Serialize + DeserializeOwned + Default + Clone + Send + Sync + 'static
{
}

/// Typed static layout (one-shot CPU solver).
pub trait StaticLayout: Send + Sync {
    type Settings: LayoutSettings;

    fn descriptor() -> LayoutDescriptor
    where
        Self: Sized;

    /// Returns `[x0, y0, z0, x1, y1, z1, ...]` packed positions in the
    /// graph's id-sorted order.
    fn solve(settings: &Self::Settings, graph: &Graph) -> Result<Vec<f32>, String>
    where
        Self: Sized;
}

/// Typed physics layout (continuous GPU sim).
pub trait PhysicsLayout: Send + Sync {
    type Settings: LayoutSettings;

    fn descriptor() -> LayoutDescriptor
    where
        Self: Sized;

    fn new(settings: Self::Settings) -> Self
    where
        Self: Sized;

    fn init_with_device(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        graph: &Graph,
        positions_buf: &wgpu::Buffer,
    ) -> Result<(), String>;

    fn step_with_encoder(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        positions_buf: &wgpu::Buffer,
    );

    fn set_settings(&mut self, settings: Self::Settings);
    fn settings(&self) -> &Self::Settings;

    fn is_halted(&self) -> bool { false }
    fn last_max_ke(&self) -> f32 { 0.0 }
    fn wake(&mut self) {}
}

// -- Object-safe dyn dispatch ------------------------------------------------

pub trait DynStaticLayout: Send + Sync {
    fn descriptor(&self) -> &LayoutDescriptor;
    fn solve_dyn(&self, settings: &Value, graph: &Graph) -> Result<Vec<f32>, String>;
    fn default_settings_json(&self) -> Value;
}

pub trait DynPhysicsLayout: Send + Sync {
    fn descriptor(&self) -> &LayoutDescriptor;

    fn init_with_device(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        graph: &Graph,
        positions_buf: &wgpu::Buffer,
    ) -> Result<(), String>;

    fn step_with_encoder(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        positions_buf: &wgpu::Buffer,
    );

    fn set_settings_json(&mut self, settings: &Value) -> Result<(), String>;
    fn settings_json(&self) -> Value;
    fn default_settings_json(&self) -> Value;

    fn is_halted(&self) -> bool;
    fn last_max_ke(&self) -> f32;
    fn wake(&mut self);
}

// -- Boxed adapters ----------------------------------------------------------

pub struct BoxedStatic<T: StaticLayout> {
    descriptor: LayoutDescriptor,
    _phantom: PhantomData<fn() -> T>,
}

impl<T: StaticLayout + 'static> BoxedStatic<T> {
    pub fn new() -> Self {
        Self {
            descriptor: T::descriptor(),
            _phantom: PhantomData,
        }
    }
}

impl<T: StaticLayout + 'static> Default for BoxedStatic<T> {
    fn default() -> Self { Self::new() }
}

impl<T: StaticLayout + 'static> DynStaticLayout for BoxedStatic<T> {
    fn descriptor(&self) -> &LayoutDescriptor { &self.descriptor }

    fn solve_dyn(&self, settings: &Value, graph: &Graph) -> Result<Vec<f32>, String> {
        let typed: T::Settings = serde_json::from_value(settings.clone())
            .map_err(|e| format!("decode settings: {e}"))?;
        T::solve(&typed, graph)
    }

    fn default_settings_json(&self) -> Value {
        serde_json::to_value(T::Settings::default()).unwrap_or(Value::Null)
    }
}

pub struct BoxedPhysics<T: PhysicsLayout> {
    inner: T,
    descriptor: LayoutDescriptor,
}

impl<T: PhysicsLayout + 'static> BoxedPhysics<T> {
    pub fn new(inner: T) -> Self {
        Self {
            descriptor: T::descriptor(),
            inner,
        }
    }
}

impl<T: PhysicsLayout + 'static> DynPhysicsLayout for BoxedPhysics<T> {
    fn descriptor(&self) -> &LayoutDescriptor { &self.descriptor }

    fn init_with_device(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        graph: &Graph,
        positions_buf: &wgpu::Buffer,
    ) -> Result<(), String> {
        self.inner.init_with_device(device, queue, graph, positions_buf)
    }

    fn step_with_encoder(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        positions_buf: &wgpu::Buffer,
    ) {
        self.inner.step_with_encoder(device, queue, encoder, positions_buf)
    }

    fn set_settings_json(&mut self, settings: &Value) -> Result<(), String> {
        let typed: T::Settings = serde_json::from_value(settings.clone())
            .map_err(|e| format!("decode settings: {e}"))?;
        self.inner.set_settings(typed);
        Ok(())
    }

    fn settings_json(&self) -> Value {
        serde_json::to_value(self.inner.settings()).unwrap_or(Value::Null)
    }

    fn default_settings_json(&self) -> Value {
        serde_json::to_value(T::Settings::default()).unwrap_or(Value::Null)
    }

    fn is_halted(&self) -> bool { self.inner.is_halted() }
    fn last_max_ke(&self) -> f32 { self.inner.last_max_ke() }
    fn wake(&mut self) { self.inner.wake() }
}
