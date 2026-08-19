//! Shared card / chip primitives for one-of-N pickers.
//!
//! Both the Layout panel's engine picker and the Settings panel's
//! importer picker had identical HTML structure (head with title +
//! chip strip, optional description, optional primary action) with
//! different CSS class prefixes. This module unifies both behind
//! `SelectableCard` and `card_chip` helpers while keeping the
//! existing class names so the browser regression selectors
//! (`[data-source-id]`, `[data-active]`, `[data-viewing]`, …) keep
//! working on the live cluster.

use dioxus::prelude::*;

/// One-pill chip used inside `SelectableCard`'s chip strip. Returns
/// the raw span so the host can compose multiple into a single
/// `select-card-chips` div without spawning one rsx! child per
/// chip. `class_suffix` is appended to `select-card-chip` — the
/// host can drive per-kind colour rules in CSS without touching
/// Rust.
pub(crate) fn card_chip(class_suffix: &str, label: &str) -> Element {
    let suffix = if class_suffix.is_empty() {
        String::new()
    } else {
        format!(" {class_suffix}")
    };
    rsx! {
        span { class: "select-card-chip{suffix}", "{label}" }
    }
}

#[component]
pub(crate) fn SelectableCard(
    /// Stable identifier the browser regression suite and the
    /// data-source-id attribute both key on. For the importer
    /// selection this is the OKF source id; for the layout panel
    /// it's the engine id.
    source_id: String,
    /// Optional kind tag (e.g. "physics" / "static" / "github" /
    /// "okf"). Used by CSS for the kind-specific chip colour and
    /// surfaced in the data-kind attribute for selectors.
    kind: String,
    /// CSS class chain. Already includes any state class
    /// (`active` / `default` / `ghost` / `viewing`); the host
    /// builds the string.
    card_class: String,
    /// Name shown in the card head. For the importer this is
    /// `display_name`; for the layout panel it's the engine label.
    name: String,
    /// ID rendered under the name (e.g. OKF source id,
    /// `jump-cannon:gpu-force`). Pass an empty string to skip.
    subtitle: String,
    /// List of small pill badges rendered in the chip strip. Each
    /// element is `(class_suffix, label)` — the class is appended
    /// to `select-card-chip` so a host can define per-kind
    /// colour rules in CSS without touching the Rust component.
    chips: Vec<(String, String)>,
    /// Optional description paragraph. Pass an empty string to skip.
    description: String,
    /// Optional disabled-reason text. Rendered with the
    /// `select-card-why` class (e.g. "node host not configured" /
    /// "graph-api cannot construct this source at runtime"). Pass
    /// an empty string to skip.
    disabled_reason: String,
    /// `true` for the default / currently-active card. CSS paints
    /// a primary-colour border + outline. Independent of the
    /// action button state.
    #[props(default)]
    is_default: bool,
    /// `true` for the currently-viewed card (the user switched to
    /// this source / engine via the URL or sessionStorage). Pairs
    /// with a yellow-tone border.
    #[props(default)]
    is_viewing: bool,
    /// `true` for the kept-but-unadvertised remote selection. The
    /// card is still selectable but its border is dashed.
    #[props(default)]
    is_ghost: bool,
    /// Disables the card and the action button. Used when
    /// graph-api can't construct the source or the engine host is
    /// not configured.
    #[props(default)]
    disabled: bool,
    /// Native disabled-reason surfaced as the `title` attribute
    /// (browser tooltip) and rendered in the disabled card body
    /// when `disabled_reason` is non-empty.
    #[props(default)]
    title: String,
    /// Action fired when the card is clicked. Layout cards use
    /// this for the whole-card button; importer cards use a
    /// dedicated action button row instead.
    #[props(default)]
    on_select: Option<EventHandler<()>>,
    /// Action body slot — host provides any extra content (an
    /// `Action` button row, identity facts, a `<details>`
    /// disclosure, etc.) without the card having to know about
    /// it. Rendered after the description and before the
    /// disabled-reason text so the natural reading order is
    /// preserved.
    children: Element,
) -> Element {
    rsx! {
        button {
            class: "{card_class}",
            r#type: "button",
            disabled: "{disabled}",
            "data-source-id": "{source_id}",
            "data-kind": "{kind}",
            "data-default": if is_default { "true" } else { "false" },
            "data-viewing": if is_viewing { "true" } else { "false" },
            "data-ghost": if is_ghost { "true" } else { "false" },
            title: "{title}",
            "aria-pressed": if is_viewing { "true" } else { "false" },
            onclick: move |_| {
                if let Some(h) = on_select.as_ref() {
                    h.call(());
                }
            },
            div { class: "select-card-head",
                div { class: "select-card-title",
                    h3 { "{name}" }
                    if !subtitle.is_empty() {
                        code { class: "select-card-subtitle", "{subtitle}" }
                    }
                }
                if !chips.is_empty() {
                    div { class: "select-card-chips",
                        for (class_suffix, label) in chips.iter() {
                            {card_chip(class_suffix, label)}
                        }
                    }
                }
            }
            if !description.is_empty() {
                p { class: "select-card-desc", "{description}" }
            }
            {children}
            if !disabled_reason.is_empty() {
                span { class: "select-card-why", "{disabled_reason}" }
            }
        }
    }
}
