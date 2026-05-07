//! Global egui theme for the graph-renderer chrome.
//!
//! High-contrast white-on-black, square corners, 1px white borders.
//! Accent palette is provided for semantic UI states (errors, warnings,
//! info, success) but should be used sparingly — the dominant aesthetic
//! is white-on-black.

use eframe::egui;

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

/// Install Courier Prime under both `Proportional` and `Monospace`
/// family slots so every text style in egui resolves to it. Bundled
/// from `assets/fonts/CourierPrime-{Regular,Bold}.ttf` — small enough
/// (~70 KB each) to embed via `include_bytes!` without bloating the
/// wasm bundle. The default egui fonts (ProggyClean / Hack) stay as
/// glyph fallbacks so unicode coverage doesn't regress.
fn install_fonts(ctx: &egui::Context) {
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
    // Make Courier Prime the *first* candidate for both families. The
    // fallback chain that egui already populated (ProggyClean for
    // monospace, Ubuntu-Light / NotoEmoji for proportional) stays in
    // place for any glyphs Courier Prime doesn't cover.
    if let Some(list) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
        list.insert(0, "courier-prime".into());
    }
    if let Some(list) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
        list.insert(0, "courier-prime".into());
    }
    ctx.set_fonts(fonts);
}

pub fn apply(ctx: &egui::Context) {
    install_fonts(ctx);

    let mut style = (*ctx.style()).clone();
    let v = &mut style.visuals;

    v.dark_mode = true;
    v.override_text_color = Some(palette::WHITE);
    v.window_fill = palette::BLACK;
    v.panel_fill = palette::BLACK;
    v.window_stroke = egui::Stroke::new(1.0, egui::Color32::WHITE);
    v.menu_rounding = egui::Rounding::ZERO;
    v.window_rounding = egui::Rounding::ZERO;
    v.widgets.noninteractive.rounding = egui::Rounding::ZERO;
    v.widgets.inactive.rounding = egui::Rounding::ZERO;
    v.widgets.hovered.rounding = egui::Rounding::ZERO;
    v.widgets.active.rounding = egui::Rounding::ZERO;
    v.widgets.open.rounding = egui::Rounding::ZERO;

    let bg = egui::Color32::BLACK;
    let fg = egui::Color32::WHITE;
    let stroke = egui::Stroke::new(1.0, fg);
    v.widgets.noninteractive.bg_fill = bg;
    v.widgets.noninteractive.bg_stroke = stroke;
    v.widgets.noninteractive.fg_stroke = stroke;
    v.widgets.inactive.bg_fill = bg;
    v.widgets.inactive.bg_stroke = stroke;
    v.widgets.inactive.fg_stroke = stroke;
    v.widgets.inactive.weak_bg_fill = bg;
    v.widgets.hovered.bg_fill = fg;
    v.widgets.hovered.bg_stroke = stroke;
    v.widgets.hovered.fg_stroke = egui::Stroke::new(1.0, bg);
    v.widgets.hovered.weak_bg_fill = fg;
    v.widgets.active.bg_fill = fg;
    v.widgets.active.bg_stroke = stroke;
    v.widgets.active.fg_stroke = egui::Stroke::new(1.0, bg);
    v.widgets.active.weak_bg_fill = fg;
    v.widgets.open.bg_fill = bg;

    // Selections / focus rings ride the brand purple so they're
    // visually anchored to the rest of the palette.
    v.selection.bg_fill = palette::PURPLE;
    v.selection.stroke = egui::Stroke::new(1.0, palette::WHITE);

    // Square slider handles — matches the no-rounding aesthetic.
    v.slider_trailing_fill = false;
    v.handle_shape = egui::style::HandleShape::Rect { aspect_ratio: 0.5 };

    // Typography hierarchy. Courier Prime is now installed as both the
    // Proportional and Monospace family (see `install_fonts` below), so
    // every TextStyle inherits the same monospaced look — the user
    // wants the IBM-CGA / terminal aesthetic everywhere.
    use egui::{FontFamily, FontId, TextStyle};
    style.text_styles.insert(
        TextStyle::Heading,
        FontId::new(12.0, FontFamily::Monospace),
    );
    style.text_styles.insert(
        TextStyle::Body,
        FontId::new(11.0, FontFamily::Monospace),
    );
    style.text_styles.insert(
        TextStyle::Monospace,
        FontId::new(11.0, FontFamily::Monospace),
    );
    style.text_styles.insert(
        TextStyle::Button,
        FontId::new(11.0, FontFamily::Monospace),
    );
    style.text_styles.insert(
        TextStyle::Small,
        FontId::new(10.0, FontFamily::Monospace),
    );

    // Tighter vertical rhythm: 4px between items (slider-to-slider).
    style.spacing.button_padding = egui::vec2(8.0, 4.0);
    style.spacing.item_spacing = egui::vec2(6.0, 4.0);
    style.spacing.window_margin = egui::Margin::same(0.0);

    ctx.set_style(style);
}
