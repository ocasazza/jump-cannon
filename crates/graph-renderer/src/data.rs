//! App-level data state: bootstrap result + lazy node-meta cache.
//!
//! The fetch task on App::new populates `LoadState::Ready` with everything
//! the GPU pipelines need to upload buffers. The next App::update sees
//! `Ready` and hands the data to GraphPipelines::load.

use crate::proto;
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub struct Bootstrap {
    pub init: Option<proto::Init>,
    pub ids: Vec<String>,
    /// 3D positions, [x,y,z, ...]. Length = 3 * n_nodes.
    pub positions: Vec<f32>,
    /// Edge index pairs, [s,t, ...]. Length = 2 * n_edges.
    pub edges: Vec<u32>,
    /// Per-node metric f32 buffers, keyed by metric name.
    pub metrics: std::collections::HashMap<String, Vec<f32>>,
}

pub enum LoadState {
    Pending,
    Loading(String), // status text
    Ready(Bootstrap),
    /// Permanent error after retries — render falls through to placeholder.
    Failed(String),
}

impl Default for LoadState {
    fn default() -> Self {
        LoadState::Pending
    }
}

pub type SharedLoad = Arc<Mutex<LoadState>>;

/// Promote the server's flat 2D positions ([x,y,x,y,...]) into 3D with a
/// small random Z spread so the force sim doesn't trap nodes on a ring.
/// Returns [x,y,z, x,y,z, ...] of length 3 * n_nodes.
pub fn promote_2d_to_3d(positions_2d: &[f32], seed: u64) -> Vec<f32> {
    // xorshift64* — tiny deterministic PRNG, no rand dep.
    let mut s = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut next = move || {
        s ^= s >> 12;
        s ^= s << 25;
        s ^= s >> 27;
        let v = s.wrapping_mul(0x2545_F491_4F6C_DD1D);
        // map to [-1, 1)
        ((v >> 11) as f64 / (1u64 << 53) as f64) as f32 * 2.0 - 1.0
    };
    let n = positions_2d.len() / 2;
    let mut out = Vec::with_capacity(n * 3);
    for i in 0..n {
        out.push(positions_2d[i * 2]);
        out.push(positions_2d[i * 2 + 1]);
        // Spread across a few hundred world units in Z.
        out.push(next() * 200.0);
    }
    out
}

/// Default colour for every node before we apply metric-driven palette
/// choices (Phase D will introduce metric → palette wiring).
pub fn default_colors(n: usize) -> Vec<f32> {
    let mut out = Vec::with_capacity(n * 4);
    for _ in 0..n {
        // Pale white-blue with full alpha — readable on the dark egui panel.
        out.extend_from_slice(&[0.85, 0.90, 1.00, 1.0]);
    }
    out
}

/// Per-node screen-space radius in pixels.
pub fn default_sizes(n: usize) -> Vec<f32> {
    vec![3.0; n]
}
