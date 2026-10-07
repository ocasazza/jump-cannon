//! The `<canvas>` the renderer draws into, with camera interaction and the
//! WebGPU status overlays. Pixels come from the rAF loop in [`crate::render`];
//! handlers steer the camera and report hover/click to the host.

use dioxus::events::{MouseEvent, WheelEvent};
use dioxus::html::geometry::WheelDelta;
use dioxus::prelude::*;

use crate::render;

/// In-flight pointer drag (camera rotate). A press that never travels
/// more than the slop is a click.
#[derive(Clone, Copy, PartialEq)]
struct Drag {
    last_mx: f64,
    last_my: f64,
    moved: bool,
}

/// The graph canvas. Input map:
///   - mouse-drag rotates pitch + yaw (any button; RMB keeps rotating, so the
///     context menu is suppressed)
///   - wheel zooms along the camera forward axis (distance-aware)
///   - plain mousemove reports `on_hover` with element coordinates
///   - click (no travel) reports `on_click` with element coordinates
///   - leaving the canvas reports `on_leave`
///
/// `children` render over the canvas after the renderer's own status overlay.
#[component]
pub fn GraphView(
    node_count: u32,
    on_hover: EventHandler<(f32, f32)>,
    on_click: EventHandler<(f32, f32)>,
    on_leave: EventHandler<()>,
    children: Element,
) -> Element {
    let mut drag = use_signal(|| Option::<Drag>::None);
    let render_status = render::RENDER_STATUS.read().clone();
    let render_state = render_status.as_attr();

    rsx! {
        div {
            class: "graph-wrap",
            "data-render-state": "{render_state}",
            canvas {
                id: render::CANVAS_ID,
                class: "graph-canvas",
                "data-node-count": "{node_count}",
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
                        on_hover.call((c.x as f32, c.y as f32));
                    }
                },
                onmouseup: move |e: MouseEvent| {
                    let was = *drag.read();
                    drag.set(None);
                    if let Some(d) = was {
                        if !d.moved {
                            let c = e.element_coordinates();
                            on_click.call((c.x as f32, c.y as f32));
                        }
                    }
                },
                onmouseenter: move |_| render::set_pointer_over(true),
                onmouseleave: move |_| {
                    drag.set(None);
                    render::set_pointer_over(false);
                    on_leave.call(());
                },
                oncontextmenu: move |e| e.prevent_default(),
                onwheel: move |e: WheelEvent| {
                    e.prevent_default();
                    let dy = match e.delta() {
                        WheelDelta::Pixels(p) => p.y,
                        WheelDelta::Lines(l) => l.y * 40.0,
                        WheelDelta::Pages(p) => p.y * 400.0,
                    };
                    // Browser wheel-down is +y; zoom-in is positive.
                    render::wheel_zoom(-dy as f32);
                },
            }
            if render_status == render::RenderStatus::Initializing {
                div {
                    class: "graph-render-status initializing",
                    "data-testid": "graph-render-status",
                    "Preparing WebGPU renderer…"
                }
            }
            if let render::RenderStatus::Unavailable { title, detail } = render_status {
                div {
                    class: "graph-render-status unavailable",
                    role: "alert",
                    "data-testid": "graph-render-status",
                    h2 { "{title}" }
                    p { "{detail}" }
                }
            }
            {children}
        }
    }
}
