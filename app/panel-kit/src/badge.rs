//! Reusable pill-shaped badge widget for clickable metadata — Dioxus port
//! of crates/graph-renderer/src/ui/badge.rs.
//!
//! Renders a (field, value) pair as a rounded chip whose click navigates,
//! opens a URL, or toggles a filter depending on [`BadgeKind`]. The egui
//! widget returns a `BadgeAction` from its immediate-mode `show()`; here
//! the action arrives through the `on_action` callback instead, so there
//! is no `None` variant — the callback only fires when something happens.
//! The host app maps fields/values onto kinds and routes the action.

use dioxus::prelude::*;

/// RGB triple for the community-tint override. Kept numeric (not a CSS
/// string) so the widget can derive the contrast fg + brightened border
/// in Rust, the same way the egui widget derives them from a `Color32`.
pub type Rgb = (u8, u8, u8);

#[derive(Debug, Clone, PartialEq)]
pub enum BadgeKind {
    Tag,
    Doctype,
    Folder,
    Author,
    Entity { ty: Option<String> },
    Wikilink { resolved: bool, target: String },
    Url { href: String, host: String },
    Date,
    Status,
    Generic,
}

#[derive(Debug, Clone, PartialEq)]
pub enum BadgeAction {
    /// Body click under [`BadgeClickKind::Toggle`], and every `×` click
    /// (the egui widget treats `×` as a toggle, which removes when the
    /// badge is active).
    Toggle { field: String, value: String },
    /// Body click when the caller opted into raw-click semantics via
    /// [`BadgeClickKind::Clicked`]. Default body-click stays `Toggle` so
    /// filter-toggle call sites need no extra match arms.
    Clicked { field: String, value: String },
    /// The explicit `+` affordance (`with_plus`). Distinct from `Toggle`
    /// so call sites can route body-clicks to "focus the node this badge
    /// belongs to" and reserve `+` for "add this attribute to the
    /// filter set".
    AddFilter { field: String, value: String },
    /// Body click on a [`BadgeKind::Wikilink`] badge.
    Navigate { target: String },
    /// Body click on a [`BadgeKind::Url`] badge.
    OpenUrl { href: String },
    /// Pointer entered the badge and the caller opted in via
    /// `emit_hover`. The egui widget emits this once per hovered frame;
    /// the DOM equivalent is the enter edge.
    Hovered { field: String, value: String },
}

/// Selects the click semantics for a non-Wikilink/Url body click.
/// Default is `Toggle` (back-compat with filter-toggle call sites).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum BadgeClickKind {
    #[default]
    Toggle,
    Clicked,
}

/// Stable hue derivation (0.0..1.0) for tag-like values. FNV-1a 32-bit —
/// small, deterministic, no extra deps — so hosts share one colour
/// mapping and tests can assert determinism without rendering.
pub fn tag_hue(value: &str) -> f32 {
    let mut h: u32 = 0x811C_9DC5;
    for b in value.as_bytes() {
        h ^= *b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    (h % 360) as f32 / 360.0
}

/// Rec. 709 luma in [0, 1]. Picks a foreground (light vs dark) that stays
/// readable across the full categorical palette.
fn perceived_brightness((r, g, b): Rgb) -> f32 {
    0.2126 * (r as f32 / 255.0) + 0.7152 * (g as f32 / 255.0) + 0.0722 * (b as f32 / 255.0)
}

/// Opaquely blend `over` (at strength `a`) on top of `base` — the border
/// of a community-tinted badge is its bg pushed 30% toward white.
fn tint_over(base: Rgb, over: Rgb, a: f32) -> Rgb {
    let a = a.clamp(0.0, 1.0);
    let mix = |b: u8, o: u8| -> u8 { (b as f32 * (1.0 - a) + o as f32 * a).round() as u8 };
    (
        mix(base.0, over.0),
        mix(base.1, over.1),
        mix(base.2, over.2),
    )
}

fn kind_class(kind: &BadgeKind) -> &'static str {
    match kind {
        BadgeKind::Tag => "badge-tag",
        BadgeKind::Doctype => "badge-doctype",
        BadgeKind::Folder => "badge-folder",
        BadgeKind::Author => "badge-author",
        BadgeKind::Entity { .. } => "badge-entity",
        BadgeKind::Wikilink { resolved: true, .. } => "badge-wikilink",
        BadgeKind::Wikilink { resolved: false, .. } => "badge-wikilink badge-unresolved",
        BadgeKind::Url { .. } => "badge-url",
        BadgeKind::Date => "badge-date",
        BadgeKind::Status => "badge-status",
        BadgeKind::Generic => "badge-generic",
    }
}

fn display_label(kind: &BadgeKind, value: &str) -> String {
    match kind {
        BadgeKind::Wikilink { .. } => format!("\u{27F6} {value}"),
        BadgeKind::Url { host, .. } => {
            if host.is_empty() {
                value.to_string()
            } else {
                host.clone()
            }
        }
        _ => value.to_string(),
    }
}

/// The badge component. Mirrors the egui builder surface: `active` (halo),
/// `with_x` (trailing `×` → `Toggle`), `with_plus` (trailing `+` →
/// `AddFilter`, drawn left of `×` when both are on), `small` (cramped
/// chip-strip geometry), `override_color` (community tint: bg = colour,
/// border/fg derived for contrast), `click_kind`, `emit_hover`.
///
/// `accent_color` is the DOM port of the egui modal's one-shot
/// `status_pill` / `ticket_badge` stroke colour: it recolours border +
/// text (any CSS colour, `var(--…)` included) while keeping the dark fill.
///
/// Long values truncate with an ellipsis (the chip carries the full value
/// in `title`); the egui widget sizes to content instead, but unbounded
/// chips don't survive a DOM flex row.
#[component]
#[allow(clippy::too_many_arguments)]
pub fn Badge(
    field: String,
    value: String,
    #[props(default = BadgeKind::Generic)] kind: BadgeKind,
    #[props(default)] active: bool,
    #[props(default)] with_x: bool,
    #[props(default)] with_plus: bool,
    #[props(default)] small: bool,
    #[props(default)] override_color: Option<Rgb>,
    #[props(default)] accent_color: Option<String>,
    #[props(default)] click_kind: BadgeClickKind,
    #[props(default)] emit_hover: bool,
    on_action: EventHandler<BadgeAction>,
) -> Element {
    let label = display_label(&kind, &value);
    // Accessible name matches the egui widget's `widget_info` label so
    // test harnesses can find badges by the same key on both stacks.
    let access = format!("badge:{field}={value}");

    let mut class = format!("badge {}", kind_class(&kind));
    if active {
        class.push_str(" active");
    }
    if small {
        class.push_str(" small");
    }

    let mut style = String::new();
    if let Some(c) = override_color {
        let (br, bg_, bb) = tint_over(c, (255, 255, 255), 0.30);
        let fg = if perceived_brightness(c) < 0.55 {
            "var(--fg)"
        } else {
            "var(--bg)"
        };
        style.push_str(&format!(
            "--badge-bg:rgb({},{},{});--badge-c:rgb({br},{bg_},{bb});--badge-fg:{fg};",
            c.0, c.1, c.2
        ));
    }
    if let Some(c) = &accent_color {
        style.push_str(&format!("--badge-c:{c};--badge-fg:{c};"));
    }

    let body_kind = kind.clone();
    let (body_field, body_value) = (field.clone(), value.clone());
    let (hover_field, hover_value) = (field.clone(), value.clone());
    let (plus_field, plus_value) = (field.clone(), value.clone());
    let (x_field, x_value) = (field.clone(), value.clone());

    rsx! {
        span {
            class: "{class}",
            style: "{style}",
            role: "button",
            tabindex: "0",
            aria_label: "{access}",
            title: "{value}",
            onclick: move |_| {
                let action = match &body_kind {
                    BadgeKind::Wikilink { target, .. } => {
                        BadgeAction::Navigate { target: target.clone() }
                    }
                    BadgeKind::Url { href, .. } => BadgeAction::OpenUrl { href: href.clone() },
                    _ => match click_kind {
                        BadgeClickKind::Toggle => BadgeAction::Toggle {
                            field: body_field.clone(),
                            value: body_value.clone(),
                        },
                        BadgeClickKind::Clicked => BadgeAction::Clicked {
                            field: body_field.clone(),
                            value: body_value.clone(),
                        },
                    },
                };
                on_action.call(action);
            },
            onmouseenter: move |_| {
                if emit_hover {
                    on_action.call(BadgeAction::Hovered {
                        field: hover_field.clone(),
                        value: hover_value.clone(),
                    });
                }
            },
            span { class: "badge-label", "{label}" }
            // `+` sits left of `×` when both are on (egui trailing-edge order).
            if with_plus {
                button {
                    class: "badge-btn badge-plus",
                    onclick: move |e| {
                        e.stop_propagation();
                        on_action.call(BadgeAction::AddFilter {
                            field: plus_field.clone(),
                            value: plus_value.clone(),
                        });
                    },
                    "+"
                }
            }
            if with_x {
                button {
                    class: "badge-btn badge-x",
                    onclick: move |e| {
                        e.stop_propagation();
                        // `×` is a toggle (removes when active) — egui parity.
                        on_action.call(BadgeAction::Toggle {
                            field: x_field.clone(),
                            value: x_value.clone(),
                        });
                    },
                    "\u{00D7}"
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_hue_deterministic() {
        assert_eq!(tag_hue("alpha"), tag_hue("alpha"));
        assert!((0.0..1.0).contains(&tag_hue("alpha")));
        assert_ne!(tag_hue("alpha"), tag_hue("beta"));
    }

    #[test]
    fn wikilink_label_prefixed() {
        let k = BadgeKind::Wikilink { resolved: true, target: "Page".into() };
        assert_eq!(display_label(&k, "Page"), "\u{27F6} Page");
    }

    #[test]
    fn url_label_prefers_host() {
        let k = BadgeKind::Url { href: "https://example.com/x".into(), host: "example.com".into() };
        assert_eq!(display_label(&k, "https://example.com/x"), "example.com");
        let bare = BadgeKind::Url { href: "https://example.com".into(), host: String::new() };
        assert_eq!(display_label(&bare, "https://example.com"), "https://example.com");
    }
}
