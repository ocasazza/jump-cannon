# Hybrid floating + tiling panel layout — design & plan

Research-backed plan (deep-research, all findings 3-0 / high confidence; APIs verified
against the repo's pinned versions: egui 0.30.0, egui-wgpu 0.30.0, egui_tiles 0.11.0,
egui_dock 0.15.0) for letting every panel be **either free-floating OR tiled
(snap-to-fit)**, toggled per-panel, using ONE shared component — keep floating, merge
tiling into the same logic.

> Status: the expanded-preview → `FloatingPanel` unification + the `Placement`
> float⇄tile toggle (green traffic-light dot) are landing in parallel. This doc is the
> target architecture those increments build toward; sections marked ✅ are done.

---

## 1. UX model — AeroSpace/yabai hybrid + Niri stability

- **Hybrid (AeroSpace/yabai):** floating panels are *excluded from the tiling tree for
  layout*, but a single shared component owns both states and the float⇄tile toggle just
  **re-parents** the panel's data. Floating stays reachable for focus/navigation.
  (`github.com/nikitabobko/AeroSpace`.)
- **Niri stability principles** for the tiled region: opening/closing a panel should not
  resize existing panels, and the focused panel should not move on its own. These are
  *"should"* guidance — egui_tiles' default drag/split can still reflow, so enforce them
  by inserting into a specific container rather than auto-splitting the whole tree.
  (`niri-wm.github.io/.../Design-Principles`.)
- **When to float:** transient/overlay panels (the node-preview popover, hover cards)
  default to floating; persistent settings panels (Layout, Filters, Camera, Style…) can
  be tiled into a workspace.

## 2. Architecture

- **wgpu graph = `CentralPanel` background, added LAST** (matches the repo's existing
  "CentralPanel must be added last" rule). It's a `Rect`-bound paint callback
  (`egui_wgpu::Callback::new_paint_callback(rect, cb)`) drawing into egui's main pass, so
  z-order is submission order: tiled region (SidePanel / inner tiles) → canvas → floating
  `egui::Window`s overlay on top.
- **One shared component** (today's `FloatingPanel`) renders a panel in EITHER mode:
  - `Placement::Floating` → an `egui::Window` overlaying the canvas. ✅ (exists)
  - `Placement::Tiled` → a pane inside one `egui_tiles::Tree`.
- **egui_tiles 0.11 is the tiling engine** (already a dep): drag-and-drop docking,
  resize, `Tabs`/`Linear`(`LinearDir`)/`Grid` containers, all customized via the
  `Behavior` trait (`pane_ui`, `tab_title_for_pane`, + optional `gap_width`, `min_size`,
  `paint_drag_preview`, `is_tile_draggable`, `on_tab_close`, …).

## 3. The float⇄tile transition (verified against vendored 0.11 source)

- **float → tile:** `Tiles::insert_pane(pane) -> TileId` (tiles.rs:213-258), then
  optionally `Tree::move_tile_to_container(tile, dest_container, index, reflow_grid)`
  (tree.rs:535) to place it precisely. Remove it from the floating list.
- **tile → float:** remove the `TileId` from the `Tree`, push the pane *data* onto a
  floating-`Window` list.
- **State preservation (critical):** keep ALL panel state in your own **serde-able `Pane`
  type**, NOT in egui `Window`/widget geometry. egui_tiles is generic over `Pane`, so the
  transition just moves that data and state survives. Window position/size/scroll and
  other transient egui state will NOT carry across the toggle unless explicitly persisted
  keyed by a stable `PanelId`.

## 4. Drag-to-snap

- Build on **egui 0.30's native type-safe DnD**: `dnd_drag_source(id, payload)` on a
  panel's grab handle, `dnd_drop_zone::<Payload, R>(frame)` on tiling drop targets, and
  `dnd_hover_payload::<T>()` during a drag to render a live **drop-preview overlay**.
- Model the overlay UX on **egui_dock 0.15's `OverlayType`**: `HighlightedAreas`
  (highlight predicted landing area) vs `Widgets` (drop-position icons). egui_dock 0.15
  already targets egui 0.30, so it's a usable reference even though we lead with egui_tiles.

## 5. Persistence

- `egui_tiles::Tree` and `Tiles` derive `Serialize`/`Deserialize` (serde is a default
  feature) **provided the `Pane` type also does**. Serialize the **floating-panel list
  alongside the `Tree`**.
- Transient geometry (floating window pos/size, per-panel scroll) is NOT auto-serialized —
  persist it separately, keyed by `PanelId`, and re-apply on load / on transition.
- Fits the repo's `AppState` serde persistence (sessionStorage on WASM, `eframe::Storage`
  native). Avoid bumping the persist version: add new fields `#[serde(default)]`.

## 6. Engine decision (the one real fork)

- **Recommended: egui_tiles 0.11** — maximal control (Behavior trait, plain
  `egui::Window` float side, manual drag-to-snap via egui DnD), at the cost of writing the
  tear-off / float⇄tile glue ourselves. Best when the floating node-preview panel needs to
  be custom (it does).
- **Alternative: egui_dock 0.15** — built-in undock-to-window (`DockState` with `Main` +
  `Window` surfaces), but less control over the float surface.
- **Rejected: `egui_docking`** (the egui_tiles↔multi-viewport bridge) — experimental,
  requires *forks* of egui and egui_tiles, incompatible with the stock egui 0.30 stack.

## 7. Phased plan

1. ✅ **Shared component + per-panel `Placement`** with the green traffic-light
   Floating⇄Tiled toggle (`FloatingPanel.with_placement`).
2. ✅ **Expanded preview → `FloatingPanel`** (component-based; no bespoke anchored modal
   for the promoted card; hover preview stays anchored).
3. **Serde-able `Pane` type** holding each panel's state (PanelId + kind + settings), so
   the toggle moves data, not widgets. Put transient geometry in a `PanelId`-keyed map.
4. **Single `egui_tiles::Tree`** for the Tiled region behind a `Behavior` impl whose
   `pane_ui` dispatches to the same per-panel body the floating path uses.
5. **Wire the toggle to re-parent**: float→`insert_pane`(+`move_tile_to_container`);
   tile→remove + push to floating list. Honour Niri no-reflow on insert.
6. **Drag-to-snap** via egui DnD + a drop-preview overlay (HighlightedAreas style).
7. **Persistence**: serialize Tree + floating list + geometry map in `AppState`.

## 8. Validation

- **Serde round-trip** tests: `Tree` + floating list + geometry map survive
  serialize→deserialize unchanged.
- **Transition state-preservation** tests: float→tile→float keeps each panel's `Pane`
  state (and re-applies geometry) intact.
- **No-reflow** test: inserting a tiled pane doesn't change sibling sizes (Niri principle).
- **Manual / browser** (pending a real display — headless WebGPU renders blank here):
  z-order (Window over canvas; tiled pane inset), input routing between the wgpu canvas and
  panels, drag-to-snap overlay correctness.

## 9. Open questions

- Drop-zone scope: only canvas edges (snap into a SidePanel-like slot) or also interior
  tiles regions (split/insert into the Tree)? Which `OverlayType` (HighlightedAreas vs
  Widgets) fits the app's visual language?
- How aggressively to enforce Niri no-reflow vs egui_tiles' default split behavior.
- Exact transient-geometry persistence policy across toggle + sessions.

### Sources
egui_tiles 0.11 (`github.com/rerun-io/egui_tiles`, `docs.rs/egui_tiles` `Behavior`/`Tiles`/`Tree`),
egui_dock 0.15 (`OverlayType`, `DockState`), egui 0.30 DnD (`dnd_drag_source`/`dnd_drop_zone`/`dnd_hover_payload`),
AeroSpace, Niri design principles, Hyprland Dwindle.
