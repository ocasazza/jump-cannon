//! Graph view: data bootstrap + the wgpu canvas component.
//!
//! Second-generation renderer for the Dioxus shell — the Canvas2D
//! placeholder is gone. The actual pixels come from `crate::render`
//! (the wgpu pipeline port from `crates/graph-renderer`): same WGSL
//! shaders, same 6DoF camera, same in-process GPU force layout. This
//! module owns (a) the one-shot fetch that turns graph-api responses
//! into the renderer's buffer seed, and (b) the `<canvas>` element with
//! its interaction handlers (drag-rotate / wheel-zoom / click-pick).

use std::collections::HashMap;

use dioxus::events::{MouseEvent, WheelEvent};
use dioxus::html::geometry::WheelDelta;
use dioxus::prelude::*;
use graph_layouts::GpuForceOptions;

use crate::api;
use crate::render;

/// Everything the app needs, fetched once from graph-api.
#[derive(Clone, PartialEq)]
pub struct GraphData {
    pub n_nodes: u32,
    pub n_edges: u32,
    pub num_communities: u32,
    pub num_wcc: u32,
    /// Node ids, same order as the renderer's buffers.
    pub ids: Vec<String>,
    pub id_to_idx: HashMap<String, u32>,
    /// Seed buffers for `render::mount_canvas` (positions / edges /
    /// colors / sizes in renderer wire format).
    pub scene: render::Scene,
}

/// Fetch the full graph bundle and derive the GPU buffer seed. Any piece
/// failing fails the load (the caller retries — the server may still be
/// indexing the vault).
///
/// Mirrors the egui app's bootstrap (`app.rs::spawn_fetch_task` +
/// `try_promote_bootstrap_to_gpu`):
///   - the server's 2D positions are ignored — nodes seed on a hollow
///     sphere shell (radius 800 wu), then the multilevel coarsening
///     warm-up (`graph_layouts::warmup_positions`) replaces that with a
///     coarsened-cascade seed so the GPU sim converges in a handful of
///     frames instead of hundreds;
///   - colors come from the community metric through the Tableau20
///     palette (egui default `ColorBy::Community`);
///   - sizes come from pagerank with the default 0.5 multiplier
///     (egui default `SizeBy::PageRank`, `size_mul = 0.5`).
pub async fn load() -> Result<GraphData, String> {
    let init = api::init().await?;
    let ids = api::ids().await?;
    let edges = api::edges().await?;

    let n = init.n_nodes as usize;
    let mut metrics: HashMap<String, Vec<f32>> = HashMap::new();
    for name in ["community", "pagerank"] {
        match api::metric(name).await {
            Ok(v) => {
                metrics.insert(name.to_string(), v);
            }
            Err(e) => tracing::warn!("[graph] metric {name}: {e}"),
        }
    }

    // Sphere shell seed, then the coarsening warm-up (which always
    // returns a full position set, so it effectively rules; the sphere
    // remains as the fallback should warmup ever come back short).
    let mut positions = render::data::spawn_on_unit_sphere(n, 800.0);
    let spring_len = GpuForceOptions::default().spring_len.max(1.0);
    let warmed = graph_layouts::warmup_positions(n, &edges, spring_len, 0xC0A75E);
    if warmed.len() == positions.len() {
        positions = warmed;
    }

    let colors = render::data::colors_from_metric("community", &metrics, n);
    let sizes = render::data::sizes_from_metric("pagerank", &metrics, n, 0.5);

    let id_to_idx = ids
        .iter()
        .enumerate()
        .map(|(i, id)| (id.clone(), i as u32))
        .collect();

    Ok(GraphData {
        n_nodes: init.n_nodes,
        n_edges: init.n_edges,
        num_communities: init.num_communities,
        num_wcc: init.num_wcc,
        ids,
        id_to_idx,
        scene: render::Scene {
            positions,
            edges,
            colors,
            sizes,
        },
    })
}

/// In-flight pointer drag (camera rotate). A press that never travels
/// more than the slop is a click — and clicks pick nodes.
#[derive(Clone, Copy, PartialEq)]
struct Drag {
    last_mx: f64,
    last_my: f64,
    moved: bool,
}

/// The canvas element + interaction handlers. Pixels come from the rAF
/// loop in `crate::render`; handlers only steer the camera and selection.
///
/// Input map (matching the egui app's semantics):
///   - mouse-drag rotates pitch + yaw (any button; sensitivity + curve
///     from workspace.rs)
///   - wheel zooms along the camera forward axis (distance-aware)
///   - click (no travel) picks the nearest node within tolerance and sets
///     the app `selected` signal
///   - plain mousemove updates hover (white rim / edge brighten)
///   - WASDQE pan is handled at the workspace root (see main.rs) and
///     gated on the pointer being over this canvas
#[component]
pub fn GraphCanvas(graph: Signal<Option<GraphData>>, selected: Signal<Option<String>>) -> Element {
    let mut drag = use_signal(|| Option::<Drag>::None);
    let mut sim_on = use_signal(|| true);

    rsx! {
        div { class: "graph-wrap",
            canvas {
                id: render::CANVAS_ID,
                class: "graph-canvas",
                onmounted: move |_| {
                    if let Some(g) = graph.read().as_ref() {
                        render::mount_canvas(g.scene.clone());
                    }
                },
                onmousedown: move |e: MouseEvent| {
                    let c = e.element_coordinates();
                    drag.set(Some(Drag { last_mx: c.x, last_my: c.y, moved: false }));
                },
                onmousemove: move |e: MouseEvent| {
                    render::set_pointer_over(true);
                    let c = e.element_coordinates();
                    // A drag is only live while a button is held — without
                    // this check a press whose release happened off-canvas
                    // leaves a stale drag that spins the camera on re-entry.
                    if drag.read().is_some() && e.held_buttons().is_empty() {
                        drag.set(None);
                    }
                    let cur = *drag.read();
                    if let Some(mut d) = cur {
                        let (dx, dy) = (c.x - d.last_mx, c.y - d.last_my);
                        if d.moved || dx.abs() + dy.abs() > 3.0 {
                            d.moved = true;
                            render::pointer_rotate(dx as f32, dy as f32);
                        }
                        d.last_mx = c.x;
                        d.last_my = c.y;
                        drag.set(Some(d));
                    } else {
                        // Hover pipeline (anchored cards + shader rim +
                        // focus dim) — throttle/hold policy lives there.
                        crate::anchored::hover_at(c.x as f32, c.y as f32);
                    }
                },
                onmouseup: move |e: MouseEvent| {
                    let was = *drag.read();
                    drag.set(None);
                    // A press that never travelled is a click. The anchored
                    // module owns the egui click semantics: node hit →
                    // sticky focus + promoted card (and we mirror the hit
                    // into `selected` for the Inspector/Document panels,
                    // like the egui `selected_node_idx`); empty canvas →
                    // clear sticky focus, `selected` untouched.
                    if let Some(d) = was {
                        if !d.moved {
                            let c = e.element_coordinates();
                            let hit_id = graph.read().as_ref().and_then(|g| {
                                crate::anchored::canvas_click(c.x as f32, c.y as f32, g)
                                    .and_then(|i| g.ids.get(i as usize).cloned())
                            });
                            if hit_id.is_some() {
                                selected.set(hit_id);
                            }
                        }
                    }
                },
                onmouseenter: move |_| render::set_pointer_over(true),
                onmouseleave: move |_| {
                    drag.set(None);
                    render::set_pointer_over(false);
                    // Edge hover clears immediately; node hover holds for
                    // 250 ms (egui update_hover_focus's pointer-None arm).
                    crate::anchored::canvas_leave();
                },
                // RMB must stay available as a rotate button (egui app
                // rotates on RMB/MMB drag) — suppress the context menu.
                oncontextmenu: move |e| e.prevent_default(),
                onwheel: move |e: WheelEvent| {
                    e.prevent_default();
                    let dy = match e.delta() {
                        WheelDelta::Pixels(p) => p.y,
                        WheelDelta::Lines(l) => l.y * 40.0,
                        WheelDelta::Pages(p) => p.y * 400.0,
                    };
                    // Browser wheel-down is +y; egui zoom-in is positive.
                    render::wheel_zoom(-dy as f32);
                },
            }
            // Minimal force-layout transport. The full Layout panel
            // (sliders, presets, backend picker) comes later.
            div { class: "graph-hud",
                button {
                    class: "btn",
                    title: "play/pause the GPU force layout",
                    onclick: move |_| {
                        let next = !*sim_on.read();
                        sim_on.set(next);
                        render::set_sim_running(next);
                    },
                    { if *sim_on.read() { "⏸ layout" } else { "▶ layout" } }
                }
                button {
                    class: "btn",
                    title: "fit camera to graph (F)",
                    onclick: move |_| render::fit_camera(),
                    "fit"
                }
            }
        }
    }
}
