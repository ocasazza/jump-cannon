//! 2D canvas graph view.
//!
//! First-generation renderer for the Dioxus shell: draws the whole vault graph
//! (positions/edges from graph-api's binary endpoints, community colors from
//! the init palette) to a `<canvas>` with pan/zoom/click-select. Deliberately
//! Canvas2D, not wgpu — it keeps the migration end-to-end simple; the wgpu
//! pipeline port from `crates/graph-renderer` is the planned follow-up for
//! large-vault performance.

use std::collections::{HashMap, HashSet};

use dioxus::events::{MouseEvent, WheelEvent};
use dioxus::html::geometry::WheelDelta;
use dioxus::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{CanvasRenderingContext2d, HtmlCanvasElement};

use crate::api;

const CANVAS_ID: &str = "graph-canvas";

/// Everything the canvas needs, fetched once from graph-api.
#[derive(Clone, PartialEq)]
pub struct GraphData {
    pub n_nodes: u32,
    pub n_edges: u32,
    pub num_communities: u32,
    pub num_wcc: u32,
    /// CSS-ready rgb per palette slot.
    pub palette: Vec<(u8, u8, u8)>,
    /// Node ids, same order as `pos` / metric buffers.
    pub ids: Vec<String>,
    pub id_to_idx: HashMap<String, u32>,
    pub pos: Vec<(f32, f32)>,
    pub edges: Vec<(u32, u32)>,
    pub community: Vec<f32>,
    /// (min_x, min_y, max_x, max_y) over node positions.
    pub bounds: (f32, f32, f32, f32),
}

/// Fetch the full graph bundle. Any piece failing fails the load (the caller
/// retries — the server may still be indexing the vault).
pub async fn load() -> Result<GraphData, String> {
    let init = api::init().await?;
    let ids = api::ids().await?;
    let raw_pos = api::positions().await?;
    let raw_edges = api::edges().await?;
    let community = api::metric("community").await.unwrap_or_default();

    let pos: Vec<(f32, f32)> = raw_pos.chunks_exact(2).map(|c| (c[0], c[1])).collect();
    let edges: Vec<(u32, u32)> = raw_edges.chunks_exact(2).map(|c| (c[0], c[1])).collect();

    let mut bounds = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for &(x, y) in &pos {
        bounds.0 = bounds.0.min(x);
        bounds.1 = bounds.1.min(y);
        bounds.2 = bounds.2.max(x);
        bounds.3 = bounds.3.max(y);
    }
    if pos.is_empty() {
        bounds = (-1.0, -1.0, 1.0, 1.0);
    }

    // Palette arrives as flat r,g,b floats; tolerate either 0..1 or 0..255.
    let palette: Vec<(u8, u8, u8)> = init
        .palette
        .chunks_exact(3)
        .map(|c| {
            let conv = |v: f32| -> u8 {
                let v = if v > 1.001 { v } else { v * 255.0 };
                v.round().clamp(0.0, 255.0) as u8
            };
            (conv(c[0]), conv(c[1]), conv(c[2]))
        })
        .collect();

    let id_to_idx = ids.iter().enumerate().map(|(i, id)| (id.clone(), i as u32)).collect();

    Ok(GraphData {
        n_nodes: init.n_nodes,
        n_edges: init.n_edges,
        num_communities: init.num_communities,
        num_wcc: init.num_wcc,
        palette,
        ids,
        id_to_idx,
        pos,
        edges,
        community,
        bounds,
    })
}

/// In-flight canvas pan (distinct from panel-kit's window drag).
#[derive(Clone, Copy, PartialEq)]
pub struct CanvasDrag {
    pub start_mx: f64,
    pub start_my: f64,
    pub start_pan: (f64, f64),
    pub moved: bool,
}

/// The canvas view state — `Copy` signal bundle like panel-kit's `Workspace`.
#[derive(Clone, Copy)]
pub struct View {
    pub zoom: Signal<f64>,
    pub pan: Signal<(f64, f64)>,
    pub drag: Signal<Option<CanvasDrag>>,
}

fn canvas_el() -> Option<HtmlCanvasElement> {
    web_sys::window()?
        .document()?
        .get_element_by_id(CANVAS_ID)?
        .dyn_into::<HtmlCanvasElement>()
        .ok()
}

/// screen = offset + world * scale, in CSS pixels.
fn view_params(g: &GraphData, cw: f64, ch: f64, zoom: f64, pan: (f64, f64)) -> (f64, f64, f64) {
    let (minx, miny, maxx, maxy) = g.bounds;
    let w = (maxx - minx).max(1e-6) as f64;
    let h = (maxy - miny).max(1e-6) as f64;
    let base = ((cw - 60.0) / w).min((ch - 60.0) / h).max(1e-9);
    let s = base * zoom;
    let wcx = (minx + maxx) as f64 / 2.0;
    let wcy = (miny + maxy) as f64 / 2.0;
    let ox = cw / 2.0 + pan.0 - wcx * s;
    let oy = ch / 2.0 + pan.1 - wcy * s;
    (s, ox, oy)
}

/// Redraw the whole scene. No-ops when the canvas isn't mounted (panel
/// minimized or another panel maximized).
pub fn draw(g: &GraphData, selected: Option<u32>, highlight: &HashSet<u32>, zoom: f64, pan: (f64, f64)) {
    let Some(canvas) = canvas_el() else { return };
    let dpr = web_sys::window().map(|w| w.device_pixel_ratio()).unwrap_or(1.0).max(0.5);
    let cw = canvas.client_width() as f64;
    let ch = canvas.client_height() as f64;
    if cw < 4.0 || ch < 4.0 {
        return;
    }
    let pw = (cw * dpr) as u32;
    let ph = (ch * dpr) as u32;
    if canvas.width() != pw {
        canvas.set_width(pw);
    }
    if canvas.height() != ph {
        canvas.set_height(ph);
    }
    let Some(ctx) = canvas
        .get_context("2d")
        .ok()
        .flatten()
        .and_then(|c| c.dyn_into::<CanvasRenderingContext2d>().ok())
    else {
        return;
    };

    let _ = ctx.set_transform(dpr, 0.0, 0.0, dpr, 0.0, 0.0);
    ctx.set_fill_style_str("#0a0a0a");
    ctx.fill_rect(0.0, 0.0, cw, ch);

    let (s, ox, oy) = view_params(g, cw, ch, zoom, pan);
    let sx = |x: f32| ox + x as f64 * s;
    let sy = |y: f32| oy + y as f64 * s;

    // Edges first, one path: faint so dense vaults read as structure, not soup.
    ctx.set_stroke_style_str("rgba(122,122,122,0.20)");
    ctx.set_line_width(0.6);
    ctx.begin_path();
    for &(a, b) in &g.edges {
        let (ai, bi) = (a as usize, b as usize);
        if ai >= g.pos.len() || bi >= g.pos.len() {
            continue;
        }
        let (ax, ay) = g.pos[ai];
        let (bx, by) = g.pos[bi];
        ctx.move_to(sx(ax), sy(ay));
        ctx.line_to(sx(bx), sy(by));
    }
    ctx.stroke();

    // Nodes, batched per community color to minimise style switches.
    let n_pal = g.palette.len().max(1);
    let r = 2.5_f64;
    let tau = std::f64::consts::TAU;
    if g.palette.is_empty() || g.community.len() < g.pos.len() {
        ctx.set_fill_style_str("#ededed");
        ctx.begin_path();
        for &(x, y) in &g.pos {
            ctx.move_to(sx(x) + r, sy(y));
            let _ = ctx.arc(sx(x), sy(y), r, 0.0, tau);
        }
        ctx.fill();
    } else {
        let mut buckets: HashMap<usize, Vec<usize>> = HashMap::new();
        for (i, c) in g.community.iter().enumerate() {
            buckets.entry((*c as usize) % n_pal).or_default().push(i);
        }
        for (slot, nodes) in buckets {
            let (cr, cg, cb) = g.palette[slot];
            ctx.set_fill_style_str(&format!("rgb({cr},{cg},{cb})"));
            ctx.begin_path();
            for &i in &nodes {
                let (x, y) = g.pos[i];
                ctx.move_to(sx(x) + r, sy(y));
                let _ = ctx.arc(sx(x), sy(y), r, 0.0, tau);
            }
            ctx.fill();
        }
    }

    // Search highlights: accent rings.
    if !highlight.is_empty() {
        ctx.set_stroke_style_str("#5ef38c");
        ctx.set_line_width(1.2);
        ctx.begin_path();
        for &i in highlight {
            let i = i as usize;
            if i >= g.pos.len() {
                continue;
            }
            let (x, y) = g.pos[i];
            ctx.move_to(sx(x) + 5.0, sy(y));
            let _ = ctx.arc(sx(x), sy(y), 5.0, 0.0, tau);
        }
        ctx.stroke();
    }

    // Selection: white ring on top.
    if let Some(i) = selected {
        let i = i as usize;
        if i < g.pos.len() {
            let (x, y) = g.pos[i];
            ctx.set_stroke_style_str("#ededed");
            ctx.set_line_width(1.6);
            ctx.begin_path();
            let _ = ctx.arc(sx(x), sy(y), 7.0, 0.0, tau);
            ctx.stroke();
        }
    }
}

/// Nearest node within 9 CSS px of the cursor, if any.
fn pick(g: &GraphData, zoom: f64, pan: (f64, f64), mx: f64, my: f64) -> Option<u32> {
    let canvas = canvas_el()?;
    let cw = canvas.client_width() as f64;
    let ch = canvas.client_height() as f64;
    let (s, ox, oy) = view_params(g, cw, ch, zoom, pan);
    let mut best: Option<(u32, f64)> = None;
    for (i, &(x, y)) in g.pos.iter().enumerate() {
        let dx = ox + x as f64 * s - mx;
        let dy = oy + y as f64 * s - my;
        let d2 = dx * dx + dy * dy;
        if d2 < 81.0 && best.map(|(_, b)| d2 < b).unwrap_or(true) {
            best = Some((i as u32, d2));
        }
    }
    best.map(|(i, _)| i)
}

/// The canvas element + its interaction handlers (pan / zoom-to-cursor /
/// click-select). The actual pixels come from [`draw`], driven by the app's
/// effect + ticker.
pub fn graph_canvas(
    graph: Signal<Option<GraphData>>,
    mut selected: Signal<Option<String>>,
    view: View,
) -> Element {
    let View { mut zoom, mut pan, mut drag } = view;
    rsx! {
        canvas {
            id: CANVAS_ID,
            class: "graph-canvas",
            onmousedown: move |e: MouseEvent| {
                let c = e.element_coordinates();
                drag.set(Some(CanvasDrag {
                    start_mx: c.x, start_my: c.y, start_pan: *pan.read(), moved: false,
                }));
            },
            onmousemove: move |e: MouseEvent| {
                let cur = *drag.read();
                if let Some(mut d) = cur {
                    let c = e.element_coordinates();
                    let (dx, dy) = (c.x - d.start_mx, c.y - d.start_my);
                    if d.moved || dx.abs() + dy.abs() > 3.0 {
                        d.moved = true;
                        pan.set((d.start_pan.0 + dx, d.start_pan.1 + dy));
                        drag.set(Some(d));
                    }
                }
            },
            onmouseup: move |e: MouseEvent| {
                let was = *drag.read();
                drag.set(None);
                // A press that never travelled is a click — select the node.
                if let Some(d) = was {
                    if !d.moved {
                        let c = e.element_coordinates();
                        if let Some(g) = graph.read().as_ref() {
                            if let Some(i) = pick(g, *zoom.read(), *pan.read(), c.x, c.y) {
                                selected.set(g.ids.get(i as usize).cloned());
                            }
                        }
                    }
                }
            },
            onmouseleave: move |_| drag.set(None),
            onwheel: move |e: WheelEvent| {
                e.prevent_default();
                let dy = match e.delta() {
                    WheelDelta::Pixels(p) => p.y,
                    WheelDelta::Lines(l) => l.y * 40.0,
                    WheelDelta::Pages(p) => p.y * 400.0,
                };
                let old = *zoom.read();
                let new = (old * (-dy * 0.0015).exp()).clamp(0.05, 80.0);
                // Zoom about the cursor: keep the world point under it fixed.
                let c = e.element_coordinates();
                if let Some(canvas) = canvas_el() {
                    let cw = canvas.client_width() as f64 / 2.0;
                    let ch = canvas.client_height() as f64 / 2.0;
                    let k = new / old;
                    let p = *pan.read();
                    pan.set((
                        (c.x - cw) - ((c.x - cw) - p.0) * k,
                        (c.y - ch) - ((c.y - ch) - p.1) * k,
                    ));
                }
                zoom.set(new);
            },
        }
    }
}
