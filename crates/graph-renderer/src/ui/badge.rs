//! Reusable pill-shaped badge widget for clickable metadata.
//!
//! Renders a (field, value) pair as a rounded chip that toggles a
//! filter, navigates to a wikilink, or opens a URL — depending on
//! `BadgeKind`. The egui owner threads the returned [`BadgeAction`]
//! back into the [`super::query::QueryModel`] / app routing.

use eframe::egui::{self, Align2, Color32, Rounding, Sense, Stroke, Vec2};

use crate::ui::theme::palette;

#[derive(Debug, Clone)]
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

#[derive(Debug)]
pub enum BadgeAction {
    None,
    Toggle { field: String, value: String },
    Navigate { target: String },
    OpenUrl { href: String },
    /// Emitted when the badge body is hovered this frame and no other
    /// action would fire. Carries (field, value) so the caller can
    /// surface tooltip / preview UI without disturbing the existing
    /// click contract.
    Hovered { field: String, value: String },
    /// Emitted on click when the caller opted into raw-click semantics
    /// via [`Badge::click_kind`] with [`BadgeClickKind::Clicked`]. The
    /// default click behaviour still emits `Toggle` so existing call
    /// sites (filter toggles) are unaffected.
    Clicked { field: String, value: String },
}

/// Selects the click semantics a [`Badge`] uses. Default is `Toggle`
/// (back-compat with every existing call site that toggles a filter).
/// Switching to `Clicked` lets a caller drive arbitrary click-handler
/// code paths without conflating with the toggle-filter contract.
#[derive(Debug, Clone, Copy)]
pub enum BadgeClickKind {
    Toggle,
    Clicked,
}

pub struct Badge<'a> {
    pub field: &'a str,
    pub value: &'a str,
    pub kind: BadgeKind,
    pub active: bool,
    pub with_x: bool,
    pub small: bool,
    /// Override the per-kind base color. Used by the inspector + modal
    /// to tint every badge for a focused node with that node's
    /// community swatch — links the chip back to the canvas community
    /// hue without losing the kind-discriminating glyph (`⟶`, host).
    pub override_color: Option<Color32>,
    /// What variant a non-Wikilink/Url click emits. Default `Toggle`.
    pub click_kind: BadgeClickKind,
    /// When true, emit `BadgeAction::Hovered { field, value }` if the
    /// badge is hovered this frame AND no other action fires. Default
    /// false to keep existing match arms tight.
    pub emit_hover: bool,
}

impl<'a> Badge<'a> {
    pub fn new(field: &'a str, value: &'a str, kind: BadgeKind) -> Self {
        Self {
            field,
            value,
            kind,
            active: false,
            with_x: false,
            small: false,
            override_color: None,
            click_kind: BadgeClickKind::Toggle,
            emit_hover: false,
        }
    }

    /// Switch the click action emitted on plain (non-wikilink/non-url)
    /// click. Defaults to [`BadgeClickKind::Toggle`] for back-compat.
    pub fn click_kind(mut self, kind: BadgeClickKind) -> Self {
        self.click_kind = kind;
        self
    }

    /// Opt in to receiving [`BadgeAction::Hovered`] when the badge is
    /// hovered AND no other action fires. Off by default so existing
    /// `match` arms stay minimal.
    pub fn emit_hover(mut self, on: bool) -> Self {
        self.emit_hover = on;
        self
    }

    pub fn active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }

    pub fn with_x(mut self, with_x: bool) -> Self {
        self.with_x = with_x;
        self
    }

    /// Cramped variant for the filter-strip / chip-strip use where the
    /// host frame already provides padding. Default (false) is the
    /// roomier modal/inspector geometry that breathes around Courier
    /// Prime descenders.
    pub fn small(mut self, small: bool) -> Self {
        self.small = small;
        self
    }

    /// Tint the badge with `color` instead of the per-kind palette
    /// pick. Border + fg derive from this single hue (white text when
    /// the hue is dark enough, black otherwise) so the chip stays
    /// readable across the full Tableau20 community palette.
    pub fn override_color(mut self, color: Color32) -> Self {
        self.override_color = Some(color);
        self
    }

    /// Stable hue derivation (0.0..1.0) for tag-like values. Extracted
    /// so filter_strip and inspector can share the colour mapping and
    /// so unit tests can assert determinism without rendering.
    pub fn tag_hue(value: &str) -> f32 {
        // FNV-1a 32-bit — small, deterministic, no extra deps.
        let mut h: u32 = 0x811C_9DC5;
        for b in value.as_bytes() {
            h ^= *b as u32;
            h = h.wrapping_mul(0x0100_0193);
        }
        (h % 360) as f32 / 360.0
    }

    pub fn show(self, ui: &mut egui::Ui) -> BadgeAction {
        let (bg_base, border, fg) = if let Some(c) = self.override_color {
            // Community-tinted variant: chip wears the override colour
            // as its bg, with a border one shade brighter (mixed with
            // WHITE) and an fg chosen for contrast — white on dark
            // hues, black on light — so the chip stays readable
            // across the full Tableau20 palette.
            let fg = if perceived_brightness(c) < 0.55 {
                palette::WHITE
            } else {
                palette::BLACK
            };
            (c, tint_over(c, palette::WHITE, 0.30), fg)
        } else {
            colors_for(&self.kind, self.active)
        };

        // Compute label text (kind-specific prefix where useful).
        let label = display_label(&self.kind, self.value);

        let font = crate::ui::theme::mono(crate::ui::theme::font_size::BODY);
        let galley = ui.painter().layout_no_wrap(label.clone(), font.clone(), fg);
        let text_size = galley.size();

        let (pad_x, pad_y) = if self.small { (6.0, 2.0) } else { (10.0, 4.0) };
        let x_w = if self.with_x { 12.0 } else { 0.0 };
        let total = Vec2::new(text_size.x + pad_x * 2.0 + x_w, text_size.y + pad_y * 2.0);
        let (rect, resp) = ui.allocate_exact_size(total, Sense::click());
        // Surface an accessible label so test harnesses (egui_kittest)
        // can find and synthesise clicks.
        let access_label = format!("badge:{}={}", self.field, self.value);
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &access_label));

        // Radius scales with content height instead of pretending to be
        // infinite — keeps the pill shape crisp if the host UI scale
        // changes (font-size resize, dpi flip).
        let radius = text_size.y / 2.0 + pad_y;
        let rounding = Rounding::same(radius);

        // Hover lift: brighten the fill toward the border colour so
        // click affordance reads independently from the active halo.
        let bg = if resp.hovered() && !self.active {
            tint_over(bg_base, border, 0.18)
        } else {
            bg_base
        };

        let painter = ui.painter();

        // Glow lives in the same paint pass (no stacked outer Frame),
        // so it respects the ambient clip rect — no more bleed past
        // the panel edge or behind sibling widgets.
        if self.active {
            painter.rect_filled(
                rect.expand(2.0),
                Rounding::same(radius + 2.0),
                tint(palette::PURPLE, 0.40),
            );
        } else if resp.hovered() {
            painter.rect_filled(
                rect.expand(2.0),
                Rounding::same(radius + 2.0),
                tint(border, 0.20),
            );
        }

        painter.rect(rect, rounding, bg, Stroke::new(1.0, border));

        let label_pos = egui::pos2(rect.left() + pad_x, rect.center().y);
        painter.text(
            label_pos,
            Align2::LEFT_CENTER,
            label,
            font,
            fg,
        );

        let mut action = BadgeAction::None;

        // Optional ✕ close button — draw last so it sits on top.
        let mut x_clicked = false;
        if self.with_x {
            let x_rect = egui::Rect::from_center_size(
                egui::pos2(rect.right() - pad_x - x_w / 2.0 + 4.0, rect.center().y),
                Vec2::new(x_w, x_w),
            );
            let x_resp = ui.interact(x_rect, ui.id().with(("badge-x", self.field, self.value)), Sense::click());
            let x_color = if x_resp.hovered() { palette::WHITE } else { tint(fg, 0.7) };
            painter.text(
                x_rect.center(),
                Align2::CENTER_CENTER,
                "\u{00D7}",
                crate::ui::theme::mono(crate::ui::theme::font_size::HEADING),
                x_color,
            );
            if x_resp.clicked() {
                x_clicked = true;
            }
        }

        if x_clicked {
            // Treat ✕ as a toggle (which removes if currently active).
            return BadgeAction::Toggle {
                field: self.field.to_string(),
                value: self.value.to_string(),
            };
        }

        if resp.clicked() {
            action = match &self.kind {
                BadgeKind::Wikilink { target, .. } => BadgeAction::Navigate {
                    target: target.clone(),
                },
                BadgeKind::Url { href, .. } => BadgeAction::OpenUrl {
                    href: href.clone(),
                },
                _ => match self.click_kind {
                    BadgeClickKind::Toggle => BadgeAction::Toggle {
                        field: self.field.to_string(),
                        value: self.value.to_string(),
                    },
                    BadgeClickKind::Clicked => BadgeAction::Clicked {
                        field: self.field.to_string(),
                        value: self.value.to_string(),
                    },
                },
            };
        }

        // Hover fallback: only when caller opted in via `emit_hover`
        // AND no click-driven action consumed the frame. Keeps default
        // call sites (toggle/navigate/open) free of new match arms.
        if self.emit_hover && matches!(action, BadgeAction::None) && resp.hovered() {
            action = BadgeAction::Hovered {
                field: self.field.to_string(),
                value: self.value.to_string(),
            };
        }
        action
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

fn tint(c: Color32, a: f32) -> Color32 {
    let a = (a.clamp(0.0, 1.0) * 255.0) as u8;
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), a)
}

/// Rec. 709 luma in [0, 1]. Used to pick a foreground (white vs black)
/// that stays readable across the full categorical palette.
fn perceived_brightness(c: Color32) -> f32 {
    let r = c.r() as f32 / 255.0;
    let g = c.g() as f32 / 255.0;
    let b = c.b() as f32 / 255.0;
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

/// Opaquely blend `over` (at strength `a`) on top of `base`. Used for
/// the hover lift so the lifted background reads as a subset of the
/// border palette, not as the active halo's purple.
fn tint_over(base: Color32, over: Color32, a: f32) -> Color32 {
    let a = a.clamp(0.0, 1.0);
    let mix = |b: u8, o: u8| -> u8 {
        let v = b as f32 * (1.0 - a) + o as f32 * a;
        v.round().clamp(0.0, 255.0) as u8
    };
    Color32::from_rgb(
        mix(base.r(), over.r()),
        mix(base.g(), over.g()),
        mix(base.b(), over.b()),
    )
}

fn colors_for(kind: &BadgeKind, active: bool) -> (Color32, Color32, Color32) {
    let (border, fg) = match kind {
        BadgeKind::Tag => (palette::PURPLE, palette::WHITE),
        BadgeKind::Doctype => (palette::INFO, palette::WHITE),
        BadgeKind::Folder => (palette::GREY, palette::WHITE),
        BadgeKind::Author => (palette::INFO, palette::WHITE),
        BadgeKind::Entity { .. } => (palette::WARNING, palette::WHITE),
        BadgeKind::Wikilink { resolved, .. } => {
            if *resolved {
                (palette::INFO, palette::INFO)
            } else {
                (palette::BAD, palette::BAD)
            }
        }
        BadgeKind::Url { .. } => (palette::GOOD, palette::GOOD),
        BadgeKind::Date => (palette::WARNING, palette::WHITE),
        BadgeKind::Status => (palette::GOOD, palette::WHITE),
        // Default chip border picks BORDER so an unstyled pill sits
        // quietly against the dark panel; hover/active still escalate
        // (hover halo + purple active halo are painted in `show()`).
        BadgeKind::Generic => (palette::BORDER, palette::WHITE),
    };
    let bg = if active {
        tint(palette::PURPLE, 0.25)
    } else {
        palette::BLACK
    };
    (bg, border, fg)
}
