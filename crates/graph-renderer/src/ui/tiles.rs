//! Tileable workspace built on `egui_tiles`.
//!
//! Sections and the filter strip can each live as either a free-floating
//! `FloatingPanel` (the historical behaviour) or as a tile inside this
//! workspace tree. The user toggles between the two modes via a small
//! `⊟ / ⤢` button in the panel header. When at least one panel is in
//! `Placement::Tiled`, a side panel mounts on the right and renders the
//! tree; with zero tiled panels the side panel is hidden so the canvas
//! keeps all the screen real estate.
//!
//! ## Snap-to-fit rule
//!
//! When a floating panel is "snapped" or when the user clicks the tray
//! icon for a panel that is currently `Placement::Tiled` (auto-open):
//!
//! 1. If any `PaneKind::Empty` leaves exist, the panel is inserted into
//!    the **largest** (by laid-out rect area) Empty leaf — replacing it.
//! 2. If there are no Empty leaves, the **largest non-Empty leaf** is
//!    split vertically (left/right) in half; the existing pane stays on
//!    the left, a fresh Empty leaf is inserted on the right, and the
//!    inbound panel takes the Empty's slot.
//! 3. If there are no leaves at all (empty tree), the inbound panel
//!    becomes the root.
//!
//! "Largest" is measured against the **last frame's** laid-out rect
//! (`Tiles::rect`). On the very first auto-open after a fresh load the
//! tree has never been laid out and no rect data exists — in that case
//! we fall back to the first Empty leaf (or the first leaf, splitting
//! it) in iteration order. After one frame the rect-based path takes
//! over automatically.

use std::collections::BTreeSet;

use eframe::egui::{self, Rect};
use serde::{Deserialize, Serialize};

use crate::perf::PerfCollector;
use crate::ui::actions::ActionRegistry;
use crate::ui::layout::registry::LayoutRegistry;
use crate::ui::state::{AppState, Section};
use crate::ui::theme::{self, palette};

/// Placement mode for a toggleable panel. `Floating` keeps the panel in
/// the historical `FloatingPanel` chrome; `Tiled` puts it into the
/// workspace tree on the right.
///
/// Default is **Tiled** — opening a panel from the tray immediately
/// snaps it into the workspace so the tiling story is the discoverable
/// path. Users who want a free-roaming window flip the placement via
/// the ⤢ toggle in the panel header (FloatingPanel's `with_placement`
/// affordance).
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum Placement {
    Floating,
    #[default]
    Tiled,
}

/// Contents of one tile leaf.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum PaneKind {
    Section(Section),
    FilterStrip,
    /// Placeholder leaf — renders a dashed "drop here" rect. Reserved
    /// snap targets that the user (or auto-open) can fill.
    Empty,
}

impl PaneKind {
    pub fn title(&self) -> String {
        match self {
            PaneKind::Section(s) => s.title().to_string(),
            PaneKind::FilterStrip => "Filters".to_string(),
            PaneKind::Empty => "empty".to_string(),
        }
    }
}

/// The tile tree plus a sidecar of "things the workspace needs to
/// remember" (currently just the user-resizable side-panel width).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TileWorkspace {
    /// `egui_tiles::Tree` carries the actual layout. `Tree<Pane>: Serde`
    /// when `Pane: Serde` + the `serde` feature is on (it is).
    pub tree: egui_tiles::Tree<PaneKind>,
    /// Last-known side-panel width. Persisted so a reload keeps the
    /// user's chosen split between canvas and workspace.
    #[serde(default = "default_width")]
    pub width: f32,
}

fn default_width() -> f32 { 480.0 }

impl Default for TileWorkspace {
    fn default() -> Self {
        Self {
            tree: egui_tiles::Tree::empty("graph-renderer-tile-workspace"),
            width: default_width(),
        }
    }
}

impl TileWorkspace {
    /// Is the workspace currently hosting any panes? (Empty leaves count
    /// — they're reserved snap targets the user explicitly created via
    /// header splits.)
    pub fn is_empty(&self) -> bool {
        self.tree.is_empty() || self.tree.root().is_none()
    }

    /// Collect every leaf currently in the tree.
    pub fn leaf_ids(&self) -> Vec<egui_tiles::TileId> {
        self.tree
            .tiles
            .iter()
            .filter_map(|(id, tile)| match tile {
                egui_tiles::Tile::Pane(_) => Some(*id),
                _ => None,
            })
            .collect()
    }

    /// All leaves that currently host a *real* pane (not Empty).
    pub fn non_empty_panes(&self) -> Vec<(egui_tiles::TileId, PaneKind)> {
        self.tree
            .tiles
            .iter()
            .filter_map(|(id, tile)| match tile {
                egui_tiles::Tile::Pane(p) if !matches!(p, PaneKind::Empty) => {
                    Some((*id, p.clone()))
                }
                _ => None,
            })
            .collect()
    }

    /// True if any leaf currently holds `pane`.
    pub fn contains(&self, pane: &PaneKind) -> bool {
        self.tree.tiles.tiles().any(|t| matches!(t, egui_tiles::Tile::Pane(p) if p == pane))
    }

    /// Remove the first leaf that hosts `pane` (no-op if not found).
    pub fn remove_pane(&mut self, pane: &PaneKind) {
        let victim = self.tree.tiles.iter().find_map(|(id, tile)| match tile {
            egui_tiles::Tile::Pane(p) if p == pane => Some(*id),
            _ => None,
        });
        if let Some(id) = victim {
            self.tree.tiles.remove(id);
        }
    }

    /// Snap `pane` into the workspace per the rule documented at the
    /// module level: largest Empty leaf wins; falling back to splitting
    /// the largest non-Empty leaf; falling back to becoming the root.
    pub fn snap_insert(&mut self, pane: PaneKind) {
        // De-dup — if the pane is already mounted somewhere, leave it.
        if self.contains(&pane) {
            return;
        }

        // Root-less tree → pane becomes the root via a fresh container.
        if self.tree.root().is_none() {
            let new_id = self.tree.tiles.insert_pane(pane);
            self.tree.root = Some(new_id);
            return;
        }

        // Find the largest Empty leaf.
        let empty_target = self
            .tree
            .tiles
            .iter()
            .filter_map(|(id, tile)| match tile {
                egui_tiles::Tile::Pane(PaneKind::Empty) => {
                    let area = self
                        .tree
                        .tiles
                        .rect(*id)
                        .map(rect_area)
                        .unwrap_or(0.0);
                    Some((*id, area))
                }
                _ => None,
            })
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(id, _)| id);

        if let Some(id) = empty_target {
            // Replace the Empty pane in place.
            if let Some(tile) = self.tree.tiles.get_mut(id) {
                *tile = egui_tiles::Tile::Pane(pane);
            }
            return;
        }

        // No Empty leaves — split the largest non-Empty leaf.
        let split_target = self
            .tree
            .tiles
            .iter()
            .filter_map(|(id, tile)| match tile {
                egui_tiles::Tile::Pane(_) => {
                    let area = self
                        .tree
                        .tiles
                        .rect(*id)
                        .map(rect_area)
                        .unwrap_or(0.0);
                    Some((*id, area))
                }
                _ => None,
            })
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(id, _)| id);

        if let Some(target) = split_target {
            self.split_pane_with(target, pane, SplitDir::Horizontal);
            return;
        }

        // Defensive fallback — shouldn't reach (we already checked
        // root().is_none() above). Insert as the root anyway.
        let new_id = self.tree.tiles.insert_pane(pane);
        self.tree.root = Some(new_id);
    }

    /// Split `existing` into a binary container holding `existing` on
    /// the left/top and `new_pane` on the right/bottom.
    fn split_pane_with(
        &mut self,
        existing: egui_tiles::TileId,
        new_pane: PaneKind,
        dir: SplitDir,
    ) {
        let new_id = self.tree.tiles.insert_pane(new_pane);
        let parent_of_existing = self.tree.tiles.parent_of(existing);
        let container_id = match dir {
            SplitDir::Horizontal => self.tree.tiles.insert_horizontal_tile(vec![existing, new_id]),
            SplitDir::Vertical => self.tree.tiles.insert_vertical_tile(vec![existing, new_id]),
        };
        // Splice the new container into wherever `existing` used to live.
        if let Some(parent) = parent_of_existing {
            if let Some(egui_tiles::Tile::Container(c)) = self.tree.tiles.get_mut(parent) {
                // Replace `existing` in the parent's child list with
                // the new container.
                let kids = c.children_vec();
                let mut replaced = false;
                for (idx, &cid) in kids.iter().enumerate() {
                    if cid == existing {
                        c.remove_child(existing);
                        // Re-insert at the same slot by rebuilding the
                        // container's child list — egui_tiles::Container
                        // doesn't expose an `insert_child_at`, so we do
                        // a remove-all-then-rebuild dance.
                        let mut new_kids: Vec<egui_tiles::TileId> = Vec::with_capacity(kids.len());
                        for (j, &k) in kids.iter().enumerate() {
                            if j == idx {
                                new_kids.push(container_id);
                            } else if k != existing {
                                new_kids.push(k);
                            }
                        }
                        // Drop every child and re-add in order.
                        for k in c.children_vec() {
                            c.remove_child(k);
                        }
                        for k in new_kids {
                            c.add_child(k);
                        }
                        replaced = true;
                        break;
                    }
                }
                if !replaced {
                    // Couldn't find existing in parent — append the new
                    // container as a sibling and move on.
                    c.add_child(container_id);
                }
            }
        } else {
            // `existing` was the root — the new container takes over.
            self.tree.root = Some(container_id);
        }
    }

    /// User-driven split: split `target` (a leaf) with a fresh Empty
    /// leaf in the requested direction.
    pub fn split_leaf(&mut self, target: egui_tiles::TileId, dir: SplitDir) {
        self.split_pane_with(target, PaneKind::Empty, dir);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitDir {
    Horizontal,
    Vertical,
}

fn rect_area(r: Rect) -> f32 {
    r.width().max(0.0) * r.height().max(0.0)
}

/// Deferred per-pane operations queued during `pane_ui` so we don't
/// mutate the tree while iterating it. Applied after `tree.ui()`
/// returns.
#[derive(Debug)]
pub enum PaneOp {
    SplitH(egui_tiles::TileId),
    SplitV(egui_tiles::TileId),
    /// Detach this tile and switch the underlying pane back to
    /// `Placement::Floating`.
    Float(egui_tiles::TileId, PaneKind),
    /// Close this tile and mark the underlying pane closed.
    Close(egui_tiles::TileId, PaneKind),
}

/// The `Behavior` implementation that paints each tile's body. Holds
/// `&mut` access to the state shards a pane body might need — the
/// `tree` itself is removed from AppState before `tree.ui` is called so
/// the borrow checker is satisfied.
pub struct TileBehavior<'a> {
    pub state: &'a mut AppState,
    pub registry: &'a mut ActionRegistry,
    pub layout_registry: &'a LayoutRegistry,
    pub perf: &'a PerfCollector,
    pub ops: Vec<PaneOp>,
}

impl<'a> egui_tiles::Behavior<PaneKind> for TileBehavior<'a> {
    fn tab_title_for_pane(&mut self, pane: &PaneKind) -> egui::WidgetText {
        pane.title().into()
    }

    fn simplification_options(&self) -> egui_tiles::SimplificationOptions {
        // Keep single-child containers around so a user-created split
        // doesn't auto-collapse the moment they float one half away —
        // that would yank the snap-target geometry out from under them.
        // Empty *Tabs* still prune (they'd be invisible junk) but
        // single-pane linear splits survive.
        egui_tiles::SimplificationOptions {
            prune_empty_tabs: true,
            prune_single_child_tabs: true,
            prune_empty_containers: false,
            prune_single_child_containers: false,
            all_panes_must_have_tabs: false,
            join_nested_linear_containers: true,
        }
    }

    fn pane_ui(
        &mut self,
        ui: &mut egui::Ui,
        tile_id: egui_tiles::TileId,
        pane: &mut PaneKind,
    ) -> egui_tiles::UiResponse {
        // Header row: title + split-h / split-v / float / close.
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(pane.title())
                    .font(theme::mono(theme::font_size::HEADING))
                    .color(palette::TEXT),
            );
            ui.with_layout(
                egui::Layout::right_to_left(egui::Align::Center),
                |ui| {
                    if matches!(pane, PaneKind::Empty) {
                        // Empty placeholders only show split buttons; X
                        // removes the placeholder, float is meaningless.
                        if ui.small_button("X").on_hover_text("Remove placeholder").clicked() {
                            self.ops.push(PaneOp::Close(tile_id, pane.clone()));
                        }
                    } else {
                        if ui.small_button("X").on_hover_text("Close panel").clicked() {
                            self.ops.push(PaneOp::Close(tile_id, pane.clone()));
                        }
                        if ui.small_button("\u{2922}").on_hover_text("Float (un-tile)").clicked() {
                            self.ops.push(PaneOp::Float(tile_id, pane.clone()));
                        }
                    }
                    if ui.small_button("\u{229E}").on_hover_text("Split vertically").clicked() {
                        self.ops.push(PaneOp::SplitV(tile_id));
                    }
                    if ui.small_button("\u{229F}").on_hover_text("Split horizontally").clicked() {
                        self.ops.push(PaneOp::SplitH(tile_id));
                    }
                },
            );
        });
        ui.separator();

        // Body.
        match pane {
            PaneKind::Section(s) => {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    crate::ui::sections::show(
                        ui,
                        *s,
                        self.state,
                        self.registry,
                        self.layout_registry,
                        self.perf,
                    );
                });
            }
            PaneKind::FilterStrip => {
                crate::ui::filter_strip::render_tiled_body(ui, self.state);
            }
            PaneKind::Empty => {
                let avail = ui.available_rect_before_wrap();
                let painter = ui.painter();
                // Dashed-ish "drop here" rect (egui has no native dashed
                // stroke; we fake it with a thin border + label).
                painter.rect_stroke(
                    avail.shrink(8.0),
                    6.0,
                    egui::Stroke::new(1.0, palette::BORDER),
                );
                painter.text(
                    avail.center(),
                    egui::Align2::CENTER_CENTER,
                    "snap target",
                    theme::mono(theme::font_size::HEADING),
                    palette::GREY,
                );
            }
        }
        egui_tiles::UiResponse::None
    }
}

/// Apply queued pane ops back onto the tree + AppState. Call this after
/// `tree.ui()` returns.
pub fn apply_pane_ops(
    state: &mut AppState,
    tree: &mut TileWorkspace,
    ops: Vec<PaneOp>,
) {
    for op in ops {
        match op {
            PaneOp::SplitH(target) => tree.split_leaf(target, SplitDir::Horizontal),
            PaneOp::SplitV(target) => tree.split_leaf(target, SplitDir::Vertical),
            PaneOp::Float(target, pane) => {
                tree.tree.tiles.remove(target);
                set_placement(state, &pane, Placement::Floating);
                // Re-open the panel as a floating one so the user
                // actually sees it after the un-tile.
                match &pane {
                    PaneKind::Section(s) => state.set_section_open(*s, true),
                    PaneKind::FilterStrip => state.filter_strip_open = true,
                    PaneKind::Empty => {}
                }
            }
            PaneOp::Close(target, pane) => {
                tree.tree.tiles.remove(target);
                match &pane {
                    PaneKind::Section(s) => state.set_section_open(*s, false),
                    PaneKind::FilterStrip => state.filter_strip_open = false,
                    PaneKind::Empty => {}
                }
            }
        }
    }
}

/// Mutate the placement state for a pane back onto AppState.
pub fn set_placement(state: &mut AppState, pane: &PaneKind, p: Placement) {
    match pane {
        PaneKind::Section(s) => {
            state.section_placement.insert(*s, p);
        }
        PaneKind::FilterStrip => {
            state.filter_strip_placement = p;
        }
        PaneKind::Empty => {}
    }
}

/// Lookup helper — placement for a section (defaults to Floating).
pub fn section_placement(state: &AppState, s: Section) -> Placement {
    state.section_placement.get(&s).copied().unwrap_or_default()
}

/// Reconcile open/visible state with the tile tree:
/// - For each Section/FilterStrip currently open AND tiled, ensure it is
///   present in the tree (snap-insert if missing).
/// - For each pane currently in the tree but corresponding to a closed
///   panel, remove it from the tree.
pub fn sync_tree_with_open_state(state: &mut AppState) {
    // Pull the tree out via swap so we can mutate it independently of
    // the rest of AppState (which `snap_insert` doesn't need).
    let mut workspace = std::mem::take(&mut state.tiles);

    // 1. Remove panes whose owning panel is closed OR no longer tiled.
    let to_remove: Vec<PaneKind> = workspace
        .non_empty_panes()
        .into_iter()
        .filter_map(|(_, pane)| {
            let (open, placement) = match &pane {
                PaneKind::Section(s) => (
                    state.is_section_open(*s),
                    section_placement(state, *s),
                ),
                PaneKind::FilterStrip => (
                    state.filter_strip_open,
                    state.filter_strip_placement,
                ),
                PaneKind::Empty => return None,
            };
            if !open || placement != Placement::Tiled {
                Some(pane)
            } else {
                None
            }
        })
        .collect();
    for p in to_remove {
        workspace.remove_pane(&p);
    }

    // 2. Add panes that are open + tiled but not in the tree.
    let already: BTreeSet<PaneKind> = workspace
        .non_empty_panes()
        .into_iter()
        .map(|(_, p)| p)
        .collect();
    let mut wanted: Vec<PaneKind> = Vec::new();
    for &s in Section::ALL {
        if state.is_section_open(s) && section_placement(state, s) == Placement::Tiled {
            let p = PaneKind::Section(s);
            if !already.contains(&p) {
                wanted.push(p);
            }
        }
    }
    if state.filter_strip_open && state.filter_strip_placement == Placement::Tiled {
        let p = PaneKind::FilterStrip;
        if !already.contains(&p) {
            wanted.push(p);
        }
    }
    for p in wanted {
        workspace.snap_insert(p);
    }

    state.tiles = workspace;
}

/// Side-panel host. Renders the workspace if any pane is currently
/// tiled. Mount this *before* the CentralPanel in `App::update`.
pub fn show_workspace_panel(
    ctx: &egui::Context,
    state: &mut AppState,
    registry: &mut ActionRegistry,
    layout_registry: &LayoutRegistry,
    perf: &PerfCollector,
) {
    sync_tree_with_open_state(state);

    if state.tiles.is_empty() {
        return;
    }

    let initial_width = state.tiles.width.max(200.0);
    let response = egui::SidePanel::right("tile-workspace")
        .resizable(true)
        .default_width(initial_width)
        .min_width(280.0)
        .frame(
            egui::Frame::none()
                .fill(theme::FLOATING_BACKDROP)
                .stroke(egui::Stroke::new(1.0, palette::BORDER))
                .inner_margin(egui::Margin::same(4.0)),
        )
        .show(ctx, |ui| {
            // Pull the tree out so the Behavior can borrow the rest of
            // AppState mutably. Put it back unconditionally below.
            let mut workspace = std::mem::take(&mut state.tiles);
            let ops = {
                let mut behavior = TileBehavior {
                    state,
                    registry,
                    layout_registry,
                    perf,
                    ops: Vec::new(),
                };
                workspace.tree.ui(&mut behavior, ui);
                behavior.ops
            };
            apply_pane_ops(state, &mut workspace, ops);
            state.tiles = workspace;
            ui.min_rect()
        });

    // Stash the latest panel width so a reload (or YAML round-trip)
    // preserves the user's chosen split.
    let w = response.response.rect.width();
    if (w - state.tiles.width).abs() > 0.5 {
        state.tiles.width = w;
    }
}

/// Helper used by the tray icon dispatcher. Toggles `*open` and, when
/// the panel is tiled and being opened, snap-inserts it into the tree.
/// When closing, the next `sync_tree_with_open_state` removes it.
pub fn toggle_panel_with_snap(state: &mut AppState, pane: PaneKind, new_open: bool) {
    match &pane {
        PaneKind::Section(s) => state.set_section_open(*s, new_open),
        PaneKind::FilterStrip => state.filter_strip_open = new_open,
        PaneKind::Empty => return,
    }
    let placement = match &pane {
        PaneKind::Section(s) => section_placement(state, *s),
        PaneKind::FilterStrip => state.filter_strip_placement,
        PaneKind::Empty => Placement::Floating,
    };
    if new_open && placement == Placement::Tiled {
        let mut ws = std::mem::take(&mut state.tiles);
        ws.snap_insert(pane);
        state.tiles = ws;
    }
}

