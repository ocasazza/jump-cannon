//! Global egui theme for the graph-renderer chrome.
//!
//! High-contrast white-on-black, square corners, 1px white borders.
//! Accent palette is provided for semantic UI states (errors, warnings,
//! info, success) but should be used sparingly — the dominant aesthetic
//! is white-on-black.

use eframe::egui;

/// Spacing constants for vertical/horizontal rhythm. Use these in
/// place of hardcoded `add_space()` magic numbers so the entire UI
/// shares one set of breakpoints.
pub mod spacing {
    /// 4 px — gap between adjacent items in the same group (e.g.
    /// slider directly above slider).
    pub const ITEM_GAP: f32 = 4.0;
    /// 6 px — gap before/after a divider rule.
    pub const DIVIDER_GAP: f32 = 6.0;
    /// 8 px — gap between distinct sub-blocks (e.g. after a section
    /// header, before the next sub-group label).
    pub const SECTION_GAP: f32 = 8.0;
}

/// Subgroup label dim alpha — uppercase sub-headers within a section.
/// Hits ≈0.6 alpha against `palette::TEXT`.
pub fn subgroup_label_color() -> eframe::egui::Color32 {
    eframe::egui::Color32::from_rgba_premultiplied(153, 153, 153, 153)
}

/// Hint text dim alpha — italic helper text under a control. ~0.5
/// alpha against `palette::TEXT`.
pub fn hint_label_color() -> eframe::egui::Color32 {
    eframe::egui::Color32::from_rgba_premultiplied(128, 128, 128, 128)
}

/// Faint stroke for sub-group dividers — barely-there 0.3-alpha rule
/// that separates option groups within a section.
pub fn subgroup_separator_color() -> eframe::egui::Color32 {
    eframe::egui::Color32::from_rgba_premultiplied(77, 77, 77, 77)
}

pub mod accent {
    use eframe::egui::Color32;

    // Legacy bright accents — kept for any callers that already use them.
    pub const RED: Color32 = Color32::from_rgb(0xff, 0x3b, 0x3b);
    pub const GREEN: Color32 = Color32::from_rgb(0x3b, 0xff, 0x7a);
    pub const BLUE: Color32 = Color32::from_rgb(0x3b, 0x9b, 0xff);
    pub const YELLOW: Color32 = Color32::from_rgb(0xff, 0xd8, 0x3b);
}

/// Core palette. Built around the user-chosen brand purple
/// `#BC83BA` (HSL ≈ 302°, 30%, 63%). Semantic states (`good`, `bad`,
/// `warning`, `info`) are derived from a single tuned chroma/lightness
/// pair via [`palette::hsl`] so they sit at the same visual weight as
/// PURPLE — no neon color clashing with the muted brand mark.
///
/// Pure NEUTRAL trio (`BLACK`, `GREY`, `WHITE`) for backgrounds and
/// chrome.
pub mod palette {
    use eframe::egui::Color32;

    // Brand.
    /// `#BC83BA` — primary brand mark / selection accent.
    pub const PURPLE: Color32 = Color32::from_rgb(188, 131, 186);

    // Neutrals (kept square, no rounding).
    pub const BLACK: Color32 = Color32::from_rgb(0x05, 0x07, 0x10);
    pub const GREY:  Color32 = Color32::from_rgb(0x80, 0x80, 0x88);
    pub const WHITE: Color32 = Color32::WHITE;

    /// Default chrome stroke (panel borders, separators). Darker than
    /// pure WHITE so the high-contrast outlines don't fight the
    /// canvas. Used by `apply()` for `widget.bg_stroke` and
    /// `window_stroke`.
    pub const BORDER: Color32 = Color32::from_rgb(0x40, 0x44, 0x4C);
    /// Default icon stroke for the activity-bar glyphs and section
    /// header rules — same darker-grey family as BORDER, slightly
    /// lighter for readability on the dark panel fill.
    pub const ICON:   Color32 = Color32::from_rgb(0x6A, 0x6E, 0x78);
    /// Default body text colour. Just-slightly off-white so it reads
    /// as "ink" rather than the maximum-contrast LED-on-black look.
    pub const TEXT:   Color32 = Color32::from_rgb(0xD8, 0xD8, 0xDC);

    // Semantic. Derived at L=63%, S~45% (matching PURPLE's HSL anchor
    // so all four colors carry the same visual weight). See
    // `derive_semantic` for the math; values inlined here so this stays
    // a const-eval module that egui code can use without runtime calc.

    /// Spring green — complement of PURPLE (H≈122). "Good / success".
    pub const GOOD:    Color32 = Color32::from_rgb(0x83, 0xCC, 0x95);
    /// Coral — analogous to PURPLE's warm side (H≈10).  "Bad / error".
    pub const BAD:     Color32 = Color32::from_rgb(0xCC, 0x88, 0x83);
    /// Amber — triadic-ish (H≈40). "Warning / caution".
    pub const WARNING: Color32 = Color32::from_rgb(0xCC, 0xB4, 0x83);
    /// Sky blue — opposite-warm-cool (H≈200). "Info / hint".
    pub const INFO:    Color32 = Color32::from_rgb(0x83, 0xB7, 0xCC);
}

/// Typography size tokens. Every font construction in the project
/// should pull from here so a global font-size bump is one edit, and
/// so ad-hoc 12.0/14.0 magic numbers stop drifting against the
/// `text_styles` hierarchy installed by [`apply`].
pub mod font_size {
    /// Section + heading text. Matches `TextStyle::Heading`.
    pub const HEADING: f32 = 12.0;
    /// Default body / button / monospace. Matches `TextStyle::Body`.
    pub const BODY: f32 = 11.0;
    /// Subgroup labels, hint text, captions. Matches `TextStyle::Small`.
    pub const SMALL: f32 = 10.0;
    /// Canvas placeholder / overlay text — bigger so it reads at a
    /// glance from far away. Used by the "loading…" centre-canvas
    /// label and similar one-shot overlays.
    pub const DISPLAY: f32 = 14.0;
}

/// Construct a `FontId` in the project's monospace family at `size`.
///
/// The whole UI is monospace per [`apply`]'s typography decision, so
/// callers should prefer this over `egui::FontId::proportional(_)` —
/// even with Courier Prime sitting at position 0 of the Proportional
/// family, egui's metric overrides differ subtly between Proportional
/// and Monospace lookups, which is the cause of the cross-tab font
/// drift this helper exists to prevent.
pub fn mono(size: f32) -> eframe::egui::FontId {
    eframe::egui::FontId::new(size, eframe::egui::FontFamily::Monospace)
}

/// HSL → RGB helper. Inputs in [0,1]; output as `Color32`.
/// Used by the const palette above as the source of truth — runtime
/// callers can derive matching shades via this if they need to tint
/// dynamically (e.g. fading a bad-state to its low-saturation form).
pub fn hsl(h: f32, s: f32, l: f32) -> eframe::egui::Color32 {
    fn hue_to_rgb(p: f32, q: f32, mut t: f32) -> f32 {
        if t < 0.0 { t += 1.0; }
        if t > 1.0 { t -= 1.0; }
        if t < 1.0 / 6.0 { return p + (q - p) * 6.0 * t; }
        if t < 1.0 / 2.0 { return q; }
        if t < 2.0 / 3.0 { return p + (q - p) * (2.0 / 3.0 - t) * 6.0; }
        p
    }
    let (r, g, b) = if s == 0.0 {
        (l, l, l)
    } else {
        let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
        let p = 2.0 * l - q;
        (
            hue_to_rgb(p, q, h + 1.0 / 3.0),
            hue_to_rgb(p, q, h),
            hue_to_rgb(p, q, h - 1.0 / 3.0),
        )
    };
    eframe::egui::Color32::from_rgb(
        (r * 255.0).round().clamp(0.0, 255.0) as u8,
        (g * 255.0).round().clamp(0.0, 255.0) as u8,
        (b * 255.0).round().clamp(0.0, 255.0) as u8,
    )
}

/// Parameterizable chrome theme. Custom builds plug their own
/// instance into [`apply`] to override panel colours, accent rings,
/// font sizes, and font choice without forking this file.
///
/// The dim-helper functions (`subgroup_label_color`, `hint_label_color`,
/// `subgroup_separator_color`) are not yet derived from this struct —
/// they remain const-alpha tints. A future iteration could pull them
/// in as `ChromePalette` fields if a custom build needs to override
/// them.
#[derive(Clone, Debug)]
pub struct ChromeTheme {
    pub palette: ChromePalette,
    pub accent: AccentPalette,
    pub typography: Typography,
    pub fonts: FontChoice,
}

#[derive(Clone, Debug)]
pub struct ChromePalette {
    pub purple: egui::Color32,
    pub black: egui::Color32,
    pub grey: egui::Color32,
    pub white: egui::Color32,
    pub border: egui::Color32,
    pub icon: egui::Color32,
    pub text: egui::Color32,
    pub good: egui::Color32,
    pub bad: egui::Color32,
    pub warning: egui::Color32,
    pub info: egui::Color32,
}

#[derive(Clone, Debug)]
pub struct AccentPalette {
    pub red: egui::Color32,
    pub green: egui::Color32,
    pub blue: egui::Color32,
    pub yellow: egui::Color32,
}

#[derive(Clone, Debug)]
pub struct Typography {
    pub heading: f32,
    pub body: f32,
    pub small: f32,
    pub display: f32,
}

#[derive(Clone, Debug)]
pub enum FontChoice {
    /// Bundled Courier Prime — current default.
    CourierPrime,
    /// Use egui's defaults (ProggyClean / Hack / Ubuntu-Light) without
    /// installing any extra fonts. Useful for lean custom builds.
    EguiDefaults,
    /// Caller provides a font family list and bytes. Each entry is
    /// inserted at index 0 of both Monospace and Proportional families
    /// in the order given (last entry ends up first).
    Custom {
        families: Vec<(String, &'static [u8])>,
    },
}

// `Default for ChromeTheme` mirrors the `palette::*` / `accent::*` /
// `font_size::*` consts bit-for-bit so existing call sites that read
// those re-exports keep matching what `apply_default` actually
// installs into the egui context.
impl Default for ChromeTheme {
    fn default() -> Self {
        Self {
            palette: ChromePalette {
                purple: palette::PURPLE,
                black: palette::BLACK,
                grey: palette::GREY,
                white: palette::WHITE,
                border: palette::BORDER,
                icon: palette::ICON,
                text: palette::TEXT,
                good: palette::GOOD,
                bad: palette::BAD,
                warning: palette::WARNING,
                info: palette::INFO,
            },
            accent: AccentPalette {
                red: accent::RED,
                green: accent::GREEN,
                blue: accent::BLUE,
                yellow: accent::YELLOW,
            },
            typography: Typography {
                heading: font_size::HEADING,
                body: font_size::BODY,
                small: font_size::SMALL,
                display: font_size::DISPLAY,
            },
            fonts: FontChoice::CourierPrime,
        }
    }
}

/// Install fonts according to `FontChoice`. For `EguiDefaults` this
/// is a no-op — egui keeps its bundled fonts. For `CourierPrime` and
/// `Custom`, each entry is inserted at index 0 of both Monospace and
/// Proportional families so the project's typography hierarchy
/// resolves consistently across both font-family slots.
fn install_fonts(ctx: &egui::Context, choice: &FontChoice) {
    match choice {
        FontChoice::EguiDefaults => {}
        FontChoice::CourierPrime => {
            let mut fonts = egui::FontDefinitions::default();
            fonts.font_data.insert(
                "courier-prime".into(),
                std::sync::Arc::new(egui::FontData::from_static(include_bytes!(
                    "../../assets/fonts/CourierPrime-Regular.ttf"
                ))),
            );
            fonts.font_data.insert(
                "courier-prime-bold".into(),
                std::sync::Arc::new(egui::FontData::from_static(include_bytes!(
                    "../../assets/fonts/CourierPrime-Bold.ttf"
                ))),
            );
            // Bold variant immediately after Regular so `RichText::strong()`
            // picks up real bold glyphs instead of synthetic-bolding.
            for family in [egui::FontFamily::Monospace, egui::FontFamily::Proportional] {
                if let Some(list) = fonts.families.get_mut(&family) {
                    list.insert(0, "courier-prime-bold".into());
                    list.insert(0, "courier-prime".into());
                }
            }
            ctx.set_fonts(fonts);
        }
        FontChoice::Custom { families } => {
            let mut fonts = egui::FontDefinitions::default();
            for (name, bytes) in families {
                fonts.font_data.insert(
                    name.clone(),
                    std::sync::Arc::new(egui::FontData::from_static(bytes)),
                );
            }
            for family in [egui::FontFamily::Monospace, egui::FontFamily::Proportional] {
                if let Some(list) = fonts.families.get_mut(&family) {
                    for (name, _) in families.iter().rev() {
                        list.insert(0, name.clone().into());
                    }
                }
            }
            ctx.set_fonts(fonts);
        }
    }
}

/// Install `theme` into `ctx`. This is the parameterized entry point —
/// custom builds construct their own `ChromeTheme` and call this
/// directly. The default jump-cannon chrome goes through
/// [`apply_default`].
pub fn apply(ctx: &egui::Context, theme: &ChromeTheme) {
    install_fonts(ctx, &theme.fonts);

    let p = &theme.palette;
    let mut style = (*ctx.style()).clone();
    let v = &mut style.visuals;

    v.dark_mode = true;
    v.override_text_color = Some(p.text);
    v.window_fill = p.black;
    v.panel_fill = p.black;
    v.window_stroke = egui::Stroke::new(1.0, p.border);
    v.menu_rounding = egui::Rounding::ZERO;
    v.window_rounding = egui::Rounding::ZERO;
    v.widgets.noninteractive.rounding = egui::Rounding::ZERO;
    v.widgets.inactive.rounding = egui::Rounding::ZERO;
    v.widgets.hovered.rounding = egui::Rounding::ZERO;
    v.widgets.active.rounding = egui::Rounding::ZERO;
    v.widgets.open.rounding = egui::Rounding::ZERO;

    let bg = p.black;
    let fg = p.white;
    let chrome_stroke = egui::Stroke::new(1.0, p.border);
    let text_stroke = egui::Stroke::new(1.0, p.text);
    v.widgets.noninteractive.bg_fill = bg;
    v.widgets.noninteractive.bg_stroke = chrome_stroke;
    v.widgets.noninteractive.fg_stroke = text_stroke;
    v.widgets.inactive.bg_fill = bg;
    v.widgets.inactive.bg_stroke = chrome_stroke;
    v.widgets.inactive.fg_stroke = text_stroke;
    v.widgets.inactive.weak_bg_fill = bg;
    v.widgets.hovered.bg_fill = fg;
    v.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, fg);
    v.widgets.hovered.fg_stroke = egui::Stroke::new(1.0, bg);
    v.widgets.hovered.weak_bg_fill = fg;
    v.widgets.active.bg_fill = fg;
    v.widgets.active.bg_stroke = egui::Stroke::new(1.0, fg);
    v.widgets.active.fg_stroke = egui::Stroke::new(1.0, bg);
    v.widgets.active.weak_bg_fill = fg;
    v.widgets.open.bg_fill = bg;

    // Selections / focus rings ride the brand purple by default.
    v.selection.bg_fill = p.purple;
    v.selection.stroke = egui::Stroke::new(1.0, p.white);

    v.slider_trailing_fill = false;
    v.handle_shape = egui::style::HandleShape::Rect { aspect_ratio: 0.5 };

    use egui::{FontFamily, FontId, TextStyle};
    let t = &theme.typography;
    style.text_styles.insert(TextStyle::Heading, FontId::new(t.heading, FontFamily::Monospace));
    style.text_styles.insert(TextStyle::Body, FontId::new(t.body, FontFamily::Monospace));
    style.text_styles.insert(TextStyle::Monospace, FontId::new(t.body, FontFamily::Monospace));
    style.text_styles.insert(TextStyle::Button, FontId::new(t.body, FontFamily::Monospace));
    style.text_styles.insert(TextStyle::Small, FontId::new(t.small, FontFamily::Monospace));

    style.spacing.button_padding = egui::vec2(8.0, 4.0);
    style.spacing.item_spacing = egui::vec2(6.0, 4.0);
    style.spacing.window_margin = egui::Margin::same(0.0);

    ctx.set_style(style);
}

/// Back-compat entry point: install jump-cannon's default chrome
/// theme. Existing call sites (`App::new`, `App::on_save`, regression
/// tests) call this so they don't have to know about `ChromeTheme`.
pub fn apply_default(ctx: &egui::Context) {
    apply(ctx, &ChromeTheme::default());
}
