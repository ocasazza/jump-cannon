//! Generic Dioxus panel-workspace library.
//!
//! Factored out of apple-notes-ocr-flow's reviewer UI so any app can get the
//! same shell: every view is a panel you can move/resize/minimize/maximize,
//! with floating (free placement) and tiling (auto grid) workspace modes,
//! macOS-style traffic lights, a minimized-panel dock strip, and layout
//! persistence to localStorage.
//!
//! The app supplies two things: a `PanelKind` impl (an enum of its panels)
//! and a body-render callback. Everything else — geometry, z-order, drag
//! state, viewport clamping, the mobile breakpoint, persistence — lives here.
//!
//! ```ignore
//! #[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
//! enum Panel { Graph, Inspector }
//! impl panel_kit::PanelKind for Panel {
//!     fn title(self) -> &'static str { /* … */ }
//! }
//!
//! let ws = panel_kit::use_workspace("myapp_layout", default_layout);
//! rsx! {
//!     style { {panel_kit::CSS} }
//!     div { class: ws.root_class(),
//!         onmousemove: move |e| ws.handle_mouse_move(&e),
//!         onmouseup: move |_| ws.handle_mouse_up(),
//!         header { class: "topbar", /* app-specific */ }
//!         {ws.render(|kind, maximized| rsx! { /* panel body for `kind` */ })}
//!         {ws.dock()}
//!     }
//! }
//! ```

use dioxus::events::MouseEvent;
use dioxus::prelude::*;
use gloo_storage::{LocalStorage, Storage};
use serde::{Deserialize, Serialize};

/// Base stylesheet for the workspace chrome (panels, lights, dock, spinner,
/// tooltip overlay, mobile breakpoint). Inject once at the app root with
/// `style { {panel_kit::CSS} }`, then layer app-specific styles after it.
pub const CSS: &str = include_str!("../assets/panel-kit.css");

/// The app's panel identifier — typically a fieldless enum.
pub trait PanelKind:
    Copy + PartialEq + Eq + std::hash::Hash + Serialize + serde::de::DeserializeOwned + 'static
{
    fn title(self) -> &'static str;
}

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum WinState {
    Floating,
    Minimized,
    Maximized,
}

/// One panel's geometry + window state. `z` is the floating-mode stacking order.
#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PanelWin<K> {
    pub kind: K,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    pub state: WinState,
    pub z: i32,
}

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Mode {
    Floating,
    Tiling,
}

#[derive(Clone, Copy, PartialEq)]
pub enum DragKind {
    Move,
    Resize,
}

#[derive(Clone, Copy, PartialEq)]
pub struct Drag {
    pub idx: usize,
    pub kind: DragKind,
    pub start_mx: f64,
    pub start_my: f64,
    pub start_x: f64,
    pub start_y: f64,
    pub start_w: f64,
    pub start_h: f64,
}

/// Convenience builder for default layouts: hands out incrementing z values.
pub struct LayoutBuilder {
    z: i32,
}

impl LayoutBuilder {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self { z: 0 }
    }
    pub fn at<K>(&mut self, kind: K, x: f64, y: f64, w: f64, h: f64) -> PanelWin<K> {
        self.z += 1;
        PanelWin { kind, x, y, w, h, state: WinState::Floating, z: self.z }
    }
}

fn front_z<K>(ps: &[PanelWin<K>]) -> i32 {
    ps.iter().map(|p| p.z).max().unwrap_or(0) + 1
}

/// Keep every floating panel inside the workspace: shrink it if it's larger
/// than the viewport, then pull its position back so the whole panel stays
/// on-screen (e.g. a layout saved on a wider screen).
fn clamp_to_viewport<K>(panels: &mut [PanelWin<K>]) {
    let win = web_sys::window();
    let vw = win.as_ref().and_then(|w| w.inner_width().ok()).and_then(|v| v.as_f64()).unwrap_or(1280.0);
    let vh = win.and_then(|w| w.inner_height().ok()).and_then(|v| v.as_f64()).unwrap_or(800.0);
    let ws_w = (vw - 4.0).max(220.0);
    let ws_h = (vh - 66.0).max(180.0); // minus topbar (~36) + dock (~30)
    for p in panels.iter_mut() {
        p.w = p.w.min(ws_w - 12.0).max(180.0);
        p.h = p.h.min(ws_h - 12.0).max(110.0);
        p.x = p.x.min(ws_w - p.w - 6.0).max(0.0);
        p.y = p.y.min(ws_h - p.h - 6.0).max(0.0);
    }
}

/// Narrow viewport -> mobile shell (static stacked tiling instead of the
/// floating panel workspace).
pub fn viewport_is_mobile() -> bool {
    web_sys::window()
        .and_then(|w| w.inner_width().ok())
        .and_then(|v| v.as_f64())
        .map(|w| w < 760.0)
        .unwrap_or(false)
}

/// True while an input/textarea has focus — apps use this to suppress
/// single-key shortcuts while the user is typing.
pub fn is_editing() -> bool {
    web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.active_element())
        .map(|el| {
            let tag = el.tag_name();
            tag.eq_ignore_ascii_case("input") || tag.eq_ignore_ascii_case("textarea")
        })
        .unwrap_or(false)
}

#[derive(Serialize, Deserialize)]
struct SavedLayout<K> {
    panels: Vec<PanelWin<K>>,
    tiling: bool,
}

fn save_layout<K: PanelKind>(key: &str, panels: &[PanelWin<K>], mode: Mode) {
    let _ = LocalStorage::set(key, SavedLayout { panels: panels.to_vec(), tiling: mode == Mode::Tiling });
}

/// Load the saved layout, reconciling against the current panel set: panels
/// added since the layout was saved are appended with their default placement,
/// so new features still show up for existing users.
fn load_layout<K: PanelKind>(key: &str, defaults: &[PanelWin<K>]) -> Option<(Vec<PanelWin<K>>, Mode)> {
    let saved: SavedLayout<K> = LocalStorage::get(key).ok()?;
    let mut panels = saved.panels;
    for d in defaults {
        if !panels.iter().any(|p| p.kind == d.kind) {
            panels.push(*d);
        }
    }
    Some((panels, if saved.tiling { Mode::Tiling } else { Mode::Floating }))
}

/// The workspace handle: a bundle of `Copy` signals, safe to pass around and
/// capture in event handlers. Create one per app root with [`use_workspace`].
pub struct Workspace<K: PanelKind> {
    pub panels: Signal<Vec<PanelWin<K>>>,
    pub mode: Signal<Mode>,
    pub drag: Signal<Option<Drag>>,
    pub is_mobile: Signal<bool>,
}

impl<K: PanelKind> Clone for Workspace<K> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<K: PanelKind> Copy for Workspace<K> {}

/// Set up workspace state: restores the persisted layout (merging in any new
/// panel kinds), re-clamps + re-evaluates the mobile breakpoint on window
/// resize, and persists the layout whenever it settles (not mid-drag).
pub fn use_workspace<K: PanelKind>(
    storage_key: &'static str,
    defaults: fn() -> Vec<PanelWin<K>>,
) -> Workspace<K> {
    let saved = load_layout(storage_key, &defaults());
    let panels = use_signal(|| {
        let mut p = saved.as_ref().map(|(p, _)| p.clone()).unwrap_or_else(defaults);
        clamp_to_viewport(&mut p);
        p
    });
    let mode = use_signal(|| saved.as_ref().map(|(_, m)| *m).unwrap_or(Mode::Floating));
    let drag = use_signal(|| Option::<Drag>::None);
    let is_mobile = use_signal(viewport_is_mobile);

    use_hook(|| {
        use wasm_bindgen::closure::Closure;
        use wasm_bindgen::JsCast;
        let mut panels = panels;
        let mut is_mobile = is_mobile;
        let cb = Closure::wrap(Box::new(move |_e: web_sys::Event| {
            {
                let mut ps = panels.write();
                clamp_to_viewport(&mut ps);
            }
            is_mobile.set(viewport_is_mobile());
        }) as Box<dyn FnMut(web_sys::Event)>);
        if let Some(w) = web_sys::window() {
            let _ = w.add_event_listener_with_callback("resize", cb.as_ref().unchecked_ref());
        }
        cb.forget();
    });

    use_effect(move || {
        let ps = panels.read().clone();
        let md = *mode.read();
        if drag.read().is_none() {
            save_layout(storage_key, &ps, md);
        }
    });

    Workspace { panels, mode, drag, is_mobile }
}

impl<K: PanelKind> Workspace<K> {
    /// Effective mode: a narrow viewport forces the static stacked (tiling)
    /// layout — the floating workspace metaphor doesn't fit a phone.
    pub fn effective_mode(&self) -> Mode {
        if *self.is_mobile.read() {
            Mode::Tiling
        } else {
            *self.mode.read()
        }
    }

    /// Class for the app root div (adds the `mobile` shell modifier).
    pub fn root_class(&self) -> &'static str {
        if *self.is_mobile.read() {
            "ws-root mobile"
        } else {
            "ws-root"
        }
    }

    /// Start a move/resize drag from a mousedown, capturing panel geometry.
    pub fn begin_drag(&self, idx: usize, kind: DragKind, e: &MouseEvent) {
        let c = e.client_coordinates();
        let p = self.panels.read()[idx];
        let mut drag = self.drag;
        drag.set(Some(Drag {
            idx,
            kind,
            start_mx: c.x,
            start_my: c.y,
            start_x: p.x,
            start_y: p.y,
            start_w: p.w,
            start_h: p.h,
        }));
    }

    /// Attach to the app root's `onmousemove` — applies the in-flight drag.
    pub fn handle_mouse_move(&self, e: &MouseEvent) {
        if let Some(d) = *self.drag.read() {
            let c = e.client_coordinates();
            let mut panels = self.panels;
            let mut ps = panels.write();
            if let Some(p) = ps.get_mut(d.idx) {
                match d.kind {
                    DragKind::Move => {
                        p.x = (d.start_x + (c.x - d.start_mx)).max(0.0);
                        p.y = (d.start_y + (c.y - d.start_my)).max(0.0);
                    }
                    DragKind::Resize => {
                        p.w = (d.start_w + (c.x - d.start_mx)).max(180.0);
                        p.h = (d.start_h + (c.y - d.start_my)).max(110.0);
                    }
                }
            }
        }
    }

    /// Attach to the app root's `onmouseup` — ends the in-flight drag.
    pub fn handle_mouse_up(&self) {
        let mut drag = self.drag;
        drag.set(None);
    }

    /// Render the workspace area. `body` renders one panel's content given its
    /// kind and whether that panel is currently maximized.
    pub fn render(&self, body: impl Fn(K, bool) -> Element) -> Element {
        let ws = *self;
        let mode_now = self.effective_mode();
        let ps = self.panels.read().clone();
        let maximized = ps.iter().position(|p| p.state == WinState::Maximized);
        let visible: Vec<usize> = match maximized {
            Some(mi) => vec![mi],
            None => ps
                .iter()
                .enumerate()
                .filter(|(_, p)| p.state != WinState::Minimized)
                .map(|(i, _)| i)
                .collect(),
        };
        let ws_class = if maximized.is_some() {
            "ws maxed"
        } else if mode_now == Mode::Tiling {
            "ws tiling"
        } else {
            "ws floating"
        };

        rsx! {
            div { class: "{ws_class}",
                for i in visible.iter().copied() {
                    {
                        let p = ps[i];
                        let floating = maximized.is_none() && mode_now == Mode::Floating;
                        let style = if maximized.is_some() {
                            "position:absolute; inset:0;".to_string()
                        } else if floating {
                            format!("position:absolute; left:{}px; top:{}px; width:{}px; height:{}px; z-index:{};",
                                p.x, p.y, p.w, p.h, p.z)
                        } else {
                            String::new()
                        };
                        rsx! {
                            section {
                                key: "{i}",
                                class: "panel",
                                style: "{style}",
                                onmousedown: move |_| {
                                    // z-order only matters when panels can overlap (floating).
                                    // In tiling, mutating panels here re-renders and can swallow
                                    // clicks on panel content.
                                    if floating {
                                        let mut panels = ws.panels;
                                        let z = front_z(&panels.read());
                                        if let Some(pp) = panels.write().get_mut(i) { pp.z = z; };
                                    }
                                },
                                {ws.header(i, p.kind.title(), floating)}
                                div { class: "panel-body",
                                    {body(p.kind, maximized == Some(i))}
                                }
                                if floating {
                                    div {
                                        class: "resize",
                                        onmousedown: move |e: MouseEvent| {
                                            ws.begin_drag(i, DragKind::Resize, &e);
                                        },
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// Panel chrome: traffic lights (red = floating⇄tiling, yellow = minimize,
    /// green = maximize⇄restore) + the drag-to-move title strip.
    fn header(&self, idx: usize, title: &str, draggable: bool) -> Element {
        let ws = *self;
        let is_max = self.panels.read().get(idx).map(|p| p.state) == Some(WinState::Maximized);
        rsx! {
            header {
                class: "panel-head",
                onmousedown: move |e: MouseEvent| {
                    if !draggable { return; }
                    ws.begin_drag(idx, DragKind::Move, &e);
                },
                div { class: "lights",
                    button { class: "light red", title: "tiling / floating",
                        onmousedown: move |e: MouseEvent| e.stop_propagation(),
                        onclick: move |_| {
                            let mut mode = ws.mode;
                            let next = if *mode.read() == Mode::Tiling { Mode::Floating } else { Mode::Tiling };
                            mode.set(next);
                        } }
                    button { class: "light yellow", title: "minimize",
                        onmousedown: move |e: MouseEvent| e.stop_propagation(),
                        onclick: move |_| {
                            let mut panels = ws.panels;
                            if let Some(p) = panels.write().get_mut(idx) { p.state = WinState::Minimized; };
                        } }
                    button { class: "light green", title: "maximize / restore",
                        onmousedown: move |e: MouseEvent| e.stop_propagation(),
                        onclick: move |_| {
                            let mut panels = ws.panels;
                            if let Some(p) = panels.write().get_mut(idx) {
                                p.state = if p.state == WinState::Maximized { WinState::Floating } else { WinState::Maximized };
                            };
                        } }
                }
                span { class: "panel-title", "{title}" }
                if is_max { span { class: "max-hint", "maximized" } }
            }
        }
    }

    /// The footer dock: minimized panels collapse to chips; click restores.
    pub fn dock(&self) -> Element {
        let ws = *self;
        let minimized: Vec<(usize, K)> = self
            .panels
            .read()
            .iter()
            .enumerate()
            .filter(|(_, p)| p.state == WinState::Minimized)
            .map(|(i, p)| (i, p.kind))
            .collect();
        rsx! {
            footer { class: "dock",
                span { class: "dock-label", "dock:" }
                if minimized.is_empty() {
                    span { class: "dock-empty", "— nothing minimized —" }
                }
                for (i, kind) in minimized.iter().copied() {
                    button {
                        key: "{i}",
                        class: "dock-chip",
                        onclick: move |_| {
                            let mut panels = ws.panels;
                            let z = front_z(&panels.read());
                            if let Some(p) = panels.write().get_mut(i) { p.state = WinState::Floating; p.z = z; };
                        },
                        "{kind.title()}"
                    }
                }
            }
        }
    }
}

/// Reusable spinner — a small rotating ring with an optional label.
#[component]
pub fn Spinner(#[props(default = String::new())] label: String) -> Element {
    rsx! {
        span { class: "spinner",
            span { class: "spin-ring" }
            if !label.is_empty() {
                span { class: "spin-label", "{label}" }
            }
        }
    }
}

/// Viewport-aware tooltip placement: prefer left of the cursor, flip right if
/// there's no room, and clamp inside the window. (CSS anchor-positioning would
/// do this natively but WebKit doesn't support it yet.)
pub fn tip_pos(cx: f64, cy: f64, tw: f64, th: f64) -> (f64, f64) {
    let win = web_sys::window();
    let vw = win.as_ref().and_then(|w| w.inner_width().ok()).and_then(|v| v.as_f64()).unwrap_or(1280.0);
    let vh = win.and_then(|w| w.inner_height().ok()).and_then(|v| v.as_f64()).unwrap_or(800.0);
    let mut x = cx - tw - 14.0;
    if x < 8.0 {
        x = cx + 14.0;
    }
    if x + tw > vw - 8.0 {
        x = vw - tw - 8.0;
    }
    let mut y = cy - 12.0;
    if y + th > vh - 8.0 {
        y = vh - th - 8.0;
    }
    (x.max(8.0), y.max(8.0))
}
