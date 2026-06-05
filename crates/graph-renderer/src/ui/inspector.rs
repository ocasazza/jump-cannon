//! Inspector body renderer.
//!
//! Shows the currently-selected node's metadata (id, degree, pagerank,
//! community, kcore — whatever lives in the per-node `metrics` map),
//! plus a clickable list of community siblings and a clickable list of
//! direct neighbors derived from the raw edge list.
//!
//! **There is no longer a free-standing inspector surface.** The
//! right-side `SidePanel` / floating `egui::Window` / "reopen pill"
//! UX was collapsed into the unified anchored panel (see
//! `app.rs::render_anchored_panel`). What remains here is the *body*
//! renderer — `render_body` — which the anchored panel calls in its
//! `expanded` mode. The `InspectorData` struct is the stable input
//! contract.
//!
//! Communication back to `App` flows through `InspectorData::requested_selection`:
//! clicking a row in either list writes that node's idx; `App::update`
//! drains it on the next frame and applies the same selection-change
//! path the canvas click uses.

use eframe::egui;
use std::collections::HashMap;

use super::frontmatter_chip::{render_frontmatter_chips, ChipOutcome};
use super::frontmatter_grid::show_frontmatter_grid;
use super::page_viewer::{self, PageViewerActions, PageViewerState};
use super::query::ActiveFieldFilters;
use super::state::ColorBy;
use super::theme::palette;
/// Read-only context the inspector uses to resolve node info, plus a
/// single mutable out-channel for click-to-select.
pub struct InspectorData<'a> {
    pub ids: &'a [String],
    pub metrics: &'a HashMap<String, Vec<f32>>,
    pub edges: &'a [u32], // packed [src, tgt, src, tgt, ...]
    pub selected_idx: Option<u32>,
    pub requested_selection: &'a mut Option<u32>,
    /// Mutable hook for badge clicks: when Some, the next badge toggle
    /// writes (field, value) here for the App to forward into the
    /// active filter set.
    pub requested_filter_toggle: &'a mut Option<(String, String)>,
    /// Active `StyleState::color_by` — drives metadata-badge tinting so
    /// chips read with the same swatch the canvas paints the node with.
    pub color_by: ColorBy,
    /// Active `StyleState::palette` — selects the categorical palette
    /// the inspector resolves community badge tints against.
    pub palette: crate::data::PaletteId,
    /// NodeMeta for the currently-selected node, when available. The
    /// inspector renders frontmatter-derived chips (wikilinks, urls,
    /// status pills, dates, ticket ids, plain values) when present.
    pub current_meta: Option<&'a crate::proto::NodeMeta>,
    /// Read-only view of the active (field, value) filter set so the
    /// inspector can paint a removable chip strip at the top of the
    /// panel and mark badge halos for already-active filters.
    pub active_filters: &'a ActiveFieldFilters,
    /// Wikilink chip click → page id to navigate to (App refetches the
    /// node and updates the modal / selection).
    pub requested_navigate: &'a mut Option<String>,
    /// URL chip click → href to open in a new tab.
    pub requested_open_url: &'a mut Option<String>,
    /// Badge body-click → node id to camera-focus + sidebar-update. For
    /// non-link badges this is the currently-selected node (so the click
    /// just slides the viewport over the node you're already reading
    /// about); for wikilink/ticket badges it's the link target. The App
    /// drains this into `focus_node_by_id`.
    pub requested_focus_node: &'a mut Option<String>,
    /// Vault-wide `(field, value, node_idxs)` inverse index, populated
    /// by `/graph/meta_summary` at boot. Used by the empty-state tag
    /// panel that renders when no node is selected — chips for the
    /// top-N vault tags, sorted by frequency. `None` while the fetch
    /// is in flight.
    pub field_index: Option<&'a crate::ui::field_index::FieldIndex>,
    /// Per-node editor state for the embedded page viewer. Shared with
    /// `app.rs::render_anchored_panel` — both surfaces mutate the same
    /// `HashMap<NodeId, PageViewerState>` so an unsaved edit started in
    /// one panel survives a hop to the other. `None` disables the page
    /// viewer entirely (used in tests that don't wire the cache).
    pub page_viewer_states: Option<&'a mut HashMap<String, PageViewerState>>,
    /// Shared CommonMark cache so the markdown renderer keeps its parsed
    /// AST + galley layout between frames. Same instance the anchored
    /// panel uses.
    pub markdown_cache: Option<&'a mut egui_commonmark::CommonMarkCache>,
    /// Save request channel. Inspector writes `Some((node_id, path, body))`
    /// when the user clicks Save (or Cmd+S). App drains and forwards into
    /// `kick_off_page_save` — same plumbing the anchored panel uses, just
    /// surfaced through a channel instead of a closure to keep the borrow
    /// graph simple.
    pub requested_page_save: &'a mut Option<(String, String, String)>,
}

/// Render the inspector body content. The unified anchored panel in
/// `app.rs::render_anchored_panel` calls this when its `expanded` flag
/// is set; the anchored panel's header owns the chrome (title, close,
/// re-snap, expand/contract toggle) so this function emits only the
/// active-filter chip strip + metadata/badges/frontmatter/page-content/
/// community/neighbours sections.
///
/// `max_height` bounds the inner `ScrollArea` so content that exceeds the
/// host panel's height scrolls *within* the panel instead of overflowing
/// its bounds. Inside an auto-sizing `egui::Area` (the anchored card),
/// `ui.set_max_height` alone doesn't give the ScrollArea a finite
/// `available_height` to clip against — the Area grows to fit content, so
/// the ScrollArea never actually scrolls. Passing an explicit `Some(h)`
/// here pins the ScrollArea's `max_height` so it clips + scrolls. `None`
/// keeps the legacy auto-sizing behaviour for any unbounded host.
pub(crate) fn render_body(
    ui: &mut egui::Ui,
    tag_browser_query: &mut String,
    data: &mut InspectorData,
    max_height: Option<f32>,
) {
    log::info!(
        "[graph-renderer] inspector body rendered: idx={}",
        data.selected_idx.unwrap_or(u32::MAX),
    );

    // Active filter chip-strip — visible whenever any filter is
    // active, regardless of whether a node is currently focused.
    // Click any chip's ✕ to remove that single (field, value)
    // pair; click a field-name lozenge to clear every value
    // bound to that field.
    show_active_filter_bar(ui, data);

    let valid_idx = data
        .selected_idx
        .filter(|i| (*i as usize) < data.ids.len());

    let mut scroll = egui::ScrollArea::vertical().auto_shrink([false; 2]);
    if let Some(h) = max_height {
        // Bound the viewport so overflowing content scrolls within the
        // panel instead of growing the host Area past its bounds.
        scroll = scroll.max_height(h);
    }
    scroll
        .show(ui, |ui| {
            let Some(idx) = valid_idx else {
                // Empty-state: render the vault-wide tag panel so the
                // sidebar has a useful default surface ("browse tags")
                // when nothing's selected.
                show_browse_tags(ui, tag_browser_query, data);
                return;
            };
            show_metadata(ui, idx, data);
            show_badges(ui, idx, data);
            show_frontmatter_section(ui, idx, data);
            show_page_content(ui, idx, data);
            ui.add_space(8.0);
            // Compute neighbours once so `show_community` can
            // decide whether to fold them into the Community
            // section or surface them as a separate dropdown
            // (only when the labelled community ≠ neighbour
            // set — see the function's docstring).
            let neighbors = neighbor_set(idx, data.edges);
            let community_handled_neighbors = show_community(ui, idx, data, &neighbors);
            if !community_handled_neighbors {
                ui.add_space(8.0);
                show_neighbors_section(ui, idx, &neighbors, data);
            }
            // Bottom breathing room so the last neighbour row
            // never sits flush against the panel border.
            ui.add_space(4.0);
        });
}

/// Render the active-filter chip strip at the top of the inspector.
///
/// Hidden when no filters are active. Each `(field, value)` chip has a
/// ✕ glyph; clicking it routes a toggle through `requested_filter_toggle`
/// — the same channel the badge clicks below use, so the App folds both
/// into `QueryModel::toggle_field_filter` on the next frame.
fn show_active_filter_bar(ui: &mut egui::Ui, data: &mut InspectorData) {
    use crate::ui::badge::{Badge, BadgeAction};
    if data.active_filters.by_field.is_empty() {
        return;
    }
    let order: Vec<String> = data
        .active_filters
        .insertion_order
        .iter()
        .filter(|f| data.active_filters.by_field.contains_key(*f))
        .cloned()
        .collect();
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        ui.spacing_mut().item_spacing.y = 4.0;
        for field in &order {
            let Some(values) = data.active_filters.by_field.get(field) else {
                continue;
            };
            for v in values {
                let kind = badge_kind_for(field);
                let b = Badge::new(field, v, kind)
                    .active(true)
                    .with_x(true)
                    .small(true);
                if let BadgeAction::Toggle { field, value } = b.show(ui) {
                    *data.requested_filter_toggle = Some((field, value));
                }
            }
        }
    });
    ui.add_space(4.0);
    let sep_rect = ui
        .allocate_exact_size(egui::vec2(ui.available_width(), 1.0), egui::Sense::hover())
        .0;
    ui.painter().rect_filled(sep_rect, 0.0, palette::BORDER);
    ui.add_space(4.0);
}

fn badge_kind_for(field: &str) -> crate::ui::badge::BadgeKind {
    use crate::ui::badge::BadgeKind;
    match field {
        "tags" | "tag" => BadgeKind::Tag,
        "doctype" => BadgeKind::Doctype,
        "folder" => BadgeKind::Folder,
        "authors" => BadgeKind::Author,
        "entities" => BadgeKind::Entity { ty: None },
        "status" => BadgeKind::Status,
        _ => BadgeKind::Generic,
    }
}

fn show_metadata(ui: &mut egui::Ui, idx: u32, data: &InspectorData) {
    let id = data.ids.get(idx as usize).cloned().unwrap_or_default();
    ui.add(
        egui::Label::new(
            egui::RichText::new(&id)
                .color(palette::TEXT)
                .strong()
                .monospace(),
        )
        .wrap(),
    );
    ui.add_space(4.0);

    // Hand-rolled two-column rows. egui::Grid was inferring weird
    // column widths once the value column held mixed monospace +
    // proportional content, and the right column wouldn't expand to
    // `available_width()` when the user dragged the panel wider.
    let label_width: f32 = 72.0;
    let row = |ui: &mut egui::Ui, key: &str, value: String, mono: bool| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let (rect, _) = ui.allocate_exact_size(
                egui::vec2(label_width, ui.spacing().interact_size.y.min(18.0)),
                egui::Sense::hover(),
            );
            ui.painter().text(
                egui::pos2(rect.left(), rect.center().y),
                egui::Align2::LEFT_CENTER,
                key,
                crate::ui::theme::mono(crate::ui::theme::font_size::BODY),
                egui::Color32::from_gray(170),
            );
            let mut rt = egui::RichText::new(value).color(palette::TEXT);
            if mono {
                rt = rt.monospace();
            }
            ui.add(egui::Label::new(rt).wrap());
        });
    };

    row(ui, "idx", format!("{}", idx), true);
    for key in ["degree", "pagerank", "community", "kcore", "recency"] {
        if let Some(vec) = data.metrics.get(key) {
            if let Some(&v) = vec.get(idx as usize) {
                let text = if key == "community" || key == "degree" || key == "kcore" {
                    format!("{}", v as i64)
                } else {
                    format!("{:.4}", v)
                };
                row(ui, key, text, true);
            }
        }
    }
}

/// Route a single badge outcome into the inspector's out-channels.
/// Mirrors the modal's `dispatch_badge` so call sites stay tiny.
fn dispatch_inspector_badge(
    action: crate::ui::badge::BadgeAction,
    body_target: &str,
    data: &mut InspectorData,
) {
    use crate::ui::badge::BadgeAction;
    match action {
        BadgeAction::Toggle { field, value } | BadgeAction::AddFilter { field, value } => {
            *data.requested_filter_toggle = Some((field, value));
        }
        BadgeAction::Clicked { .. } => {
            *data.requested_focus_node = Some(body_target.to_string());
        }
        BadgeAction::Navigate { target } => {
            *data.requested_focus_node = Some(target.clone());
            *data.requested_navigate = Some(target);
        }
        BadgeAction::OpenUrl { href } => {
            *data.requested_open_url = Some(href);
        }
        BadgeAction::Hovered { .. } | BadgeAction::None => {}
    }
}

/// Empty-state tag browser. Renders when no node is selected so the
/// inspector has a useful default surface — top-N vault tags as filter
/// chips, click toggles the filter, active chips show the purple halo.
///
/// Hidden if `field_index` hasn't returned yet or has no tags. The
/// caller already gates inspector mount on `has_browse_tags`, so we
/// only get here when there's something to render.
/// Empty-state vault-wide tag browser. Renders when no node is selected
/// so the right sidebar has a useful default. Top of the panel is a
/// fuzzy search input — on a 7k-tag vault the unfiltered top-50 list
/// can't surface a specific tag, so the search is the only practical
/// affordance. Empty query falls back to "top N by frequency."
///
/// Persistence: the query lives on `AppState::tag_browser_query` so it
/// survives between renders and across reloads.
fn show_browse_tags(
    ui: &mut egui::Ui,
    query: &mut String,
    data: &mut InspectorData,
) {
    use crate::ui::badge::{Badge, BadgeAction, BadgeKind};
    use fuzzy_matcher::skim::SkimMatcherV2;
    use fuzzy_matcher::FuzzyMatcher;

    const MAX_CHIPS: usize = 80;

    let Some(fi) = data.field_index else {
        ui.add_space(8.0);
        ui.label(
            egui::RichText::new("(no node selected — loading tags…)")
                .color(egui::Color32::from_gray(140))
                .italics(),
        );
        return;
    };
    let Some(tag_buckets) = fi.by_field.get("tags") else {
        ui.add_space(8.0);
        ui.label(
            egui::RichText::new("(no node selected)")
                .color(egui::Color32::from_gray(140))
                .italics(),
        );
        return;
    };
    if tag_buckets.is_empty() {
        ui.add_space(8.0);
        ui.label(
            egui::RichText::new("(no node selected — vault has no tags)")
                .color(egui::Color32::from_gray(140))
                .italics(),
        );
        return;
    }

    let total = tag_buckets.len();
    let trimmed = query.trim().to_string();

    // Rank tags. Empty query → top-N by frequency desc, alpha asc as
    // stable tiebreak. Non-empty query → SkimMatcherV2 fuzzy score with
    // frequency as a secondary key, capped at MAX_CHIPS hits.
    let ranked: Vec<(&String, usize)> = if trimmed.is_empty() {
        let mut v: Vec<(&String, usize)> = tag_buckets
            .iter()
            .map(|(v, idxs)| (v, idxs.len()))
            .collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
        v.truncate(MAX_CHIPS);
        v
    } else {
        let matcher = SkimMatcherV2::default().ignore_case();
        let mut scored: Vec<(i64, &String, usize)> = tag_buckets
            .iter()
            .filter_map(|(value, idxs)| {
                matcher
                    .fuzzy_match(value, &trimmed)
                    .map(|score| (score, value, idxs.len()))
            })
            .collect();
        // Sort by fuzzy score desc, then frequency desc as tiebreak.
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.2.cmp(&a.2)));
        scored.truncate(MAX_CHIPS);
        scored.into_iter().map(|(_, v, c)| (v, c)).collect()
    };

    ui.add_space(6.0);
    ui.label(
        egui::RichText::new(format!("Tags ({} total)", total))
            .color(palette::TEXT)
            .strong(),
    );

    // Fuzzy search input. Use `desired_width` so the field stretches
    // to the panel column instead of egui's default narrow look.
    let avail = ui.available_width();
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(query)
                .desired_width(avail - 60.0)
                .hint_text("filter tags…"),
        );
        if !query.is_empty() && ui.small_button("clear").clicked() {
            query.clear();
        }
    });

    if ranked.is_empty() {
        ui.add_space(6.0);
        ui.label(
            egui::RichText::new(format!("(no tags match {:?})", trimmed))
                .color(egui::Color32::from_gray(140))
                .italics(),
        );
        return;
    }

    if !trimmed.is_empty() {
        ui.label(
            egui::RichText::new(format!(
                "{} match{}",
                ranked.len(),
                if ranked.len() == 1 { "" } else { "es" }
            ))
            .small()
            .color(egui::Color32::from_gray(140)),
        );
    }

    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        ui.spacing_mut().item_spacing.y = 4.0;
        for (value, count) in &ranked {
            let active = data
                .active_filters
                .by_field
                .get("tags")
                .map(|s| s.contains(*value))
                .unwrap_or(false);
            let label = format!("{} ({})", value, count);
            let b = Badge::new("tags", &label, BadgeKind::Tag)
                .active(active)
                .small(true);
            if let BadgeAction::Toggle { .. } = b.show(ui) {
                // The badge's `value` carries the composite "name (count)"
                // label; route the raw tag value through the filter
                // toggle instead so subsequent renders re-match.
                *data.requested_filter_toggle =
                    Some(("tags".to_string(), (*value).clone()));
            }
        }
    });
}

fn show_badges(ui: &mut egui::Ui, idx: u32, data: &mut InspectorData) {
    use crate::ui::badge::{Badge, BadgeClickKind, BadgeKind};
    // Single source of truth: the modal's NodeMeta cache for the
    // focused node (populated by `/node/:id` fetches). The inspector
    // and modal share this same record so the chip set stays in sync
    // across both surfaces. When the cached meta's id has drifted from
    // the selected node's id (mid-fetch / stale click), bail rather
    // than render badges for the wrong page.
    let id = match data.ids.get(idx as usize) {
        Some(s) => s.as_str(),
        None => return,
    };
    let Some(meta) = data.current_meta else {
        return;
    };
    if !meta.id.is_empty() && meta.id != id {
        return;
    }
    let has_frontmatter = !meta.frontmatter_json.is_empty()
        && meta.frontmatter_json != "{}"
        && meta.frontmatter_json != "null";
    if meta.tags.is_empty()
        && meta.folder.is_empty()
        && meta.doctype.is_none()
        && !has_frontmatter
    {
        return;
    }
    // Tint every badge with the focused node's community swatch so
    // the chip strip reads as part of the same colour cohort the
    // canvas paints the node with. Falls back to the per-kind
    // palette when no community metric is available.
    let community_tint: Option<egui::Color32> = crate::data::node_color_for_key(
        data.color_by.metric_key(),
        idx,
        data.metrics,
        data.palette,
    );
    // Thin BORDER-coloured rule sets the badges row apart from the
    // meta grid above; `add_space` book-ends it for breathing room.
    ui.add_space(6.0);
    let sep_rect = ui
        .allocate_exact_size(egui::vec2(ui.available_width(), 1.0), egui::Sense::hover())
        .0;
    ui.painter().rect_filled(sep_rect, 0.0, palette::BORDER);
    ui.add_space(6.0);
    let is_active = |field: &str, value: &str| -> bool {
        data.active_filters
            .by_field
            .get(field)
            .map(|s| s.contains(value))
            .unwrap_or(false)
    };
    ui.horizontal_wrapped(|ui| {
        // Pack chips tighter than the default item_spacing — a 240px
        // panel with default 6px gaps wastes a chip's worth of width
        // per row. 4px keeps chips readable while squeezing one more
        // chip on most rows.
        ui.spacing_mut().item_spacing.x = 4.0;
        ui.spacing_mut().item_spacing.y = 4.0;
        // Body-click on a content badge focuses the node the inspector
        // is currently showing (camera + sticky highlight + sidebar
        // refresh). The explicit `+` routes to the filter set.
        let body_target = id.to_string();
        for tag in &meta.tags {
            let active = is_active("tags", tag);
            let b = maybe_tint(
                Badge::new("tags", tag, BadgeKind::Tag).active(active),
                community_tint,
            )
            .with_plus(true)
            .click_kind(BadgeClickKind::Clicked);
            dispatch_inspector_badge(b.show(ui), &body_target, data);
        }
        if let Some(dt) = &meta.doctype {
            let active = is_active("doctype", dt);
            let b = maybe_tint(
                Badge::new("doctype", dt, BadgeKind::Doctype).active(active),
                community_tint,
            )
            .with_plus(true)
            .click_kind(BadgeClickKind::Clicked);
            dispatch_inspector_badge(b.show(ui), &body_target, data);
        }
        if !meta.folder.is_empty() {
            let active = is_active("folder", &meta.folder);
            let b = maybe_tint(
                Badge::new("folder", &meta.folder, BadgeKind::Folder).active(active),
                community_tint,
            )
            .with_plus(true)
            .click_kind(BadgeClickKind::Clicked);
            dispatch_inspector_badge(b.show(ui), &body_target, data);
        }
        // Frontmatter chips — same detection rules the modal uses
        // (wikilinks, urls, status pills, dates, ticket ids, plain
        // values). Skips long text, nested arrays/objects, nulls.
        if has_frontmatter {
            if let Ok(map) = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(
                &meta.frontmatter_json,
            ) {
                let outcome: ChipOutcome = render_frontmatter_chips(
                    ui,
                    &map,
                    data.active_filters,
                    community_tint,
                );
                if let Some(p) = outcome.toggle_filter {
                    *data.requested_filter_toggle = Some(p);
                }
                if let Some(t) = outcome.navigate_to {
                    *data.requested_navigate = Some(t);
                }
                if let Some(h) = outcome.open_url {
                    *data.requested_open_url = Some(h);
                }
            }
        }
    });
}

/// Render the "Frontmatter" collapsing section that surfaces every
/// frontmatter field which the chip walker did NOT render — long-form
/// strings, nested objects, mixed arrays, nulls. Keeps the inspector
/// honest about the full page metadata, not just the chip-shaped subset.
fn show_frontmatter_section(ui: &mut egui::Ui, idx: u32, data: &InspectorData) {
    let id = match data.ids.get(idx as usize) {
        Some(s) => s.as_str(),
        None => return,
    };
    let Some(meta) = data.current_meta else {
        return;
    };
    if !meta.id.is_empty() && meta.id != id {
        return;
    }
    if meta.frontmatter_json.is_empty()
        || meta.frontmatter_json == "{}"
        || meta.frontmatter_json == "null"
    {
        return;
    }
    let Ok(map) = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(
        &meta.frontmatter_json,
    ) else {
        return;
    };
    // Cheap pre-check: if the helper would emit zero rows, don't even
    // draw the collapsing header — keeps the panel tidy when every
    // frontmatter value is already a chip.
    let has_leftover = map.iter().any(|(k, v)| {
        let skip = matches!(
            k.to_ascii_lowercase().as_str(),
            "tags" | "tag" | "doctype" | "folder" | "title" | "id" | "path"
        );
        if skip {
            return false;
        }
        match v {
            serde_json::Value::String(s) => {
                let t = s.trim();
                t.is_empty() || t.chars().count() > 120
            }
            serde_json::Value::Array(arr) => !arr.iter().any(|x| {
                matches!(
                    x,
                    serde_json::Value::String(_)
                        | serde_json::Value::Number(_)
                        | serde_json::Value::Bool(_)
                )
            }),
            serde_json::Value::Object(_) | serde_json::Value::Null => true,
            _ => false,
        }
    });
    if !has_leftover {
        return;
    }
    ui.add_space(6.0);
    egui::CollapsingHeader::new(egui::RichText::new("Frontmatter").color(palette::TEXT))
        .default_open(false)
        .id_salt("inspector-frontmatter")
        .show(ui, |ui| {
            show_frontmatter_grid(ui, &map, "inspector-frontmatter-grid");
        });
}

/// Render the editable Obsidian page-viewer for the focused node.
///
/// Mirrors the surface `app.rs::render_anchored_panel` paints for the
/// click-promoted anchored card so the user gets the same editor whether
/// they read the inspector or the anchored panel. Hidden when:
///   * the node isn't a vault page (`page_viewer::is_obsidian_page`
///     returns false — verified against `graph-api::server.rs`: real
///     vault pages carry the on-disk doctype or `None`, and the
///     stub-fallback for unknown ids carries `doctype = Some("external")`,
///     which we want to skip),
///   * `current_meta` is stale (its id has drifted from the selected
///     node's id — same guard `show_badges` / `show_frontmatter_section`
///     use to avoid painting against the wrong page),
///   * the caller didn't wire `page_viewer_states` + `markdown_cache`
///     (e.g. headless tests construct the inspector without the App's
///     editor scaffolding).
fn show_page_content(ui: &mut egui::Ui, idx: u32, data: &mut InspectorData) {
    let id = match data.ids.get(idx as usize) {
        Some(s) => s.as_str(),
        None => {
            log::debug!(
                "[graph-renderer] show_page_content skip: ids.get({idx}) is None"
            );
            return;
        }
    };
    let Some(meta) = data.current_meta else {
        // Fetch is in flight — surface a tiny "loading" hint so the
        // editor area isn't silently empty.
        log::debug!(
            "[graph-renderer] show_page_content skip: current_meta is None (id={id})"
        );
        ui.add_space(8.0);
        egui::CollapsingHeader::new(
            egui::RichText::new("Page content").color(palette::TEXT),
        )
        .default_open(true)
        .id_salt(("inspector-page-content", id))
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new("Loading page metadata…")
                    .color(palette::GREY)
                    .italics(),
            );
        });
        return;
    };
    if !meta.id.is_empty() && meta.id != id {
        log::debug!(
            "[graph-renderer] show_page_content skip: meta.id={:?} != selected id={:?}",
            meta.id, id
        );
        return;
    }
    if !page_viewer::is_obsidian_page(meta) {
        log::debug!(
            "[graph-renderer] show_page_content skip: not obsidian page (id={id}, path={:?}, doctype={:?}, body_len={})",
            meta.path, meta.doctype, meta.body.len()
        );
        return;
    }
    // We need ALL three editor handles to render anything useful. If any
    // is absent (test harness, missing wiring), bail silently — the
    // inspector still shows metadata / badges / frontmatter above.
    let (Some(states), Some(cache)) = (
        data.page_viewer_states.as_deref_mut(),
        data.markdown_cache.as_deref_mut(),
    ) else {
        log::debug!(
            "[graph-renderer] show_page_content skip: page_viewer_states or markdown_cache is None (id={id})"
        );
        return;
    };

    ui.add_space(8.0);
    egui::CollapsingHeader::new(
        egui::RichText::new("Page content").color(palette::TEXT),
    )
    .default_open(true)
    .id_salt(("inspector-page-content", id))
    .show(ui, |ui| {
        let state = states.entry(meta.id.clone()).or_default();
        let mut save_request: Option<(String, String)> = None;
        {
            let mut on_save = |path: &str, body: &str| {
                save_request = Some((path.to_string(), body.to_string()));
            };
            let mut actions = PageViewerActions {
                markdown_cache: cache,
                on_save: &mut on_save,
            };
            page_viewer::show_in_panel(ui, state, meta, &mut actions);
        }
        if let Some((path, body)) = save_request {
            *data.requested_page_save = Some((meta.id.clone(), path, body));
        }
    });
}

/// Apply the community tint to a badge if a tint is available,
/// otherwise pass it through. Free function (not a closure) so the
/// returned `Badge<'a>` keeps a single named lifetime — the closure
/// version produces a borrow-checker `'_`-mismatch.
fn maybe_tint<'a>(
    b: crate::ui::badge::Badge<'a>,
    tint: Option<egui::Color32>,
) -> crate::ui::badge::Badge<'a> {
    match tint {
        Some(c) => b.override_color(c),
        None => b,
    }
}

/// Walk the packed `[src, tgt]` edge list and collect the focused
/// node's unique neighbours, sorted ascending. Pulled out of the
/// rendering site so the inspector can compare neighbour-set against
/// the labelled-community sibling set before deciding whether to
/// surface the Neighbours dropdown.
fn neighbor_set(idx: u32, edges: &[u32]) -> Vec<u32> {
    use std::collections::HashSet;
    let mut set: HashSet<u32> = HashSet::new();
    for chunk in edges.chunks_exact(2) {
        let (s, t) = (chunk[0], chunk[1]);
        if s == idx {
            set.insert(t);
        } else if t == idx {
            set.insert(s);
        }
    }
    set.remove(&idx);
    let mut v: Vec<u32> = set.into_iter().collect();
    v.sort_unstable();
    v
}

/// Render the Community section.
///
/// Returns `true` when the section already represents the neighbour
/// set — either because the labelled community is identical to the
/// neighbour set, or because no community metric exists and the
/// neighbours ARE the de-facto community. The caller skips the
/// separate Neighbours dropdown in that case (it would just show the
/// same list under a different header).
///
/// Returns `false` when the labelled community diverges from the
/// neighbour set and the caller should also show a separate
/// Neighbours dropdown for the supplemental view.
fn show_community(
    ui: &mut egui::Ui,
    idx: u32,
    data: &mut InspectorData,
    neighbors: &[u32],
) -> bool {
    let Some(comm_vec) = data.metrics.get("community") else {
        // No labelled community → neighbours are the de-facto community.
        // Surface them as the Community section so the panel keeps a
        // single canonical "who's connected to this node" list.
        let header = format!("Community ({} members, neighbours)", neighbors.len());
        egui::CollapsingHeader::new(egui::RichText::new(header).color(palette::TEXT))
            .default_open(true)
            .id_salt(("inspector-community-neighbors", idx))
            .show(ui, |ui| {
                if neighbors.is_empty() {
                    ui.label(
                        egui::RichText::new("(no neighbours)")
                            .color(egui::Color32::from_gray(140))
                            .italics(),
                    );
                    return;
                }
                clickable_list(ui, "comm-list", neighbors, data);
            });
        return true;
    };
    let Some(&my_comm_f) = comm_vec.get(idx as usize) else {
        return true;
    };
    let my_comm = my_comm_f as i64;

    // Collect siblings (same community id, excluding self).
    let mut siblings: Vec<u32> = comm_vec
        .iter()
        .enumerate()
        .filter_map(|(i, &v)| {
            if v as i64 == my_comm && i as u32 != idx {
                Some(i as u32)
            } else {
                None
            }
        })
        .collect();
    // Stable order, capped to keep the UI snappy on huge communities.
    siblings.sort_unstable();

    let header = format!("Community {} ({} members)", my_comm, siblings.len());
    egui::CollapsingHeader::new(egui::RichText::new(header).color(palette::TEXT))
        .default_open(true)
        .id_salt(("inspector-community", idx, my_comm))
        .show(ui, |ui| {
            if siblings.is_empty() {
                ui.label(
                    egui::RichText::new("(no siblings)")
                        .color(egui::Color32::from_gray(140))
                        .italics(),
                );
                return;
            }
            clickable_list(ui, "comm-list", &siblings, data);
        });

    // The Neighbours dropdown only adds signal when its set diverges
    // from the labelled community siblings. When they're identical,
    // showing both is redundant chrome — return true so the caller
    // skips the second section.
    siblings == *neighbors
}

fn show_neighbors_section(
    ui: &mut egui::Ui,
    idx: u32,
    neighbors: &[u32],
    data: &mut InspectorData,
) {
    let header = format!("Neighbours ({})", neighbors.len());
    egui::CollapsingHeader::new(egui::RichText::new(header).color(palette::TEXT))
        .default_open(true)
        .id_salt(("inspector-neighbors", idx))
        .show(ui, |ui| {
            if neighbors.is_empty() {
                ui.label(
                    egui::RichText::new("(no neighbours)")
                        .color(egui::Color32::from_gray(140))
                        .italics(),
                );
                return;
            }
            clickable_list(ui, "neighbor-list", neighbors, data);
        });
}

fn clickable_list(
    ui: &mut egui::Ui,
    _id_source: &str,
    items: &[u32],
    data: &mut InspectorData,
) {
    use crate::ui::badge::{Badge, BadgeAction, BadgeClickKind, BadgeKind};
    // No nested ScrollArea — the outer panel ScrollArea handles vertical
    // scroll, so two scrollables don't fight for wheel events. The MAX
    // cap below prevents per-frame widget blow-up on huge communities.
    const MAX: usize = 200;
    let truncated = items.len() > MAX;
    // Render each neighbour / community sibling as a community-tinted
    // pill (Badge). Body-click fires `requested_selection`, which the
    // App folds into `focus_node_by_id` — camera slides, sticky-focus
    // flips, modal refreshes. Wrap horizontally so a long community
    // grid into a paragraph of chips instead of a column of one-per-row.
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        ui.spacing_mut().item_spacing.y = 4.0;
        for &i in items.iter().take(MAX) {
            let label = data
                .ids
                .get(i as usize)
                .map(|s| s.as_str())
                .unwrap_or("?");
            let short = short_id_for_pill(label);
            let tint = crate::data::node_color_for_key(
                data.color_by.metric_key(),
                i,
                data.metrics,
                data.palette,
            );
            let mut b = Badge::new("node", &short, BadgeKind::Generic)
                .small(true)
                .click_kind(BadgeClickKind::Clicked);
            if let Some(c) = tint {
                b = b.override_color(c);
            }
            match b.show(ui) {
                BadgeAction::Clicked { .. } => {
                    *data.requested_selection = Some(i);
                }
                // No filter affordance here — the pill represents a node,
                // not a (field, value) attribute. Other variants are not
                // emitted by the configuration above.
                _ => {}
            }
        }
    });
    if truncated {
        ui.label(
            egui::RichText::new(format!("… {} more not shown", items.len() - MAX))
                .color(egui::Color32::from_gray(140))
                .italics(),
        );
    }
}

/// Truncate a node id to a single-line pill label. Long path-like ids
/// ("notes/2025/projects/alpha.md") get the file-name tail; the
/// directory prefix is implied by community colour anyway.
fn short_id_for_pill(id: &str) -> String {
    // Prefer the basename if the id looks path-like.
    let basename = id.rsplit('/').next().unwrap_or(id);
    const MAX: usize = 24;
    if basename.chars().count() <= MAX {
        basename.to_string()
    } else {
        let head: String = basename.chars().take(MAX - 1).collect();
        format!("{head}…")
    }
}
