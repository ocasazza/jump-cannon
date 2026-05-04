//! Reusable pill-shaped badge widget for clickable metadata.
//!
//! Renders a (field, value) pair as a rounded chip that toggles a
//! filter, navigates to a wikilink, or opens a URL — depending on
//! `BadgeKind`. The egui owner threads the returned [`BadgeAction`]
//! back into the [`super::query::QueryModel`] / app routing.

use eframe::egui::{self, Align2, Color32, FontId, Rounding, Sense, Stroke, Vec2};

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
}

pub struct Badge<'a> {
    pub field: &'a str,
    pub value: &'a str,
    pub kind: BadgeKind,
    pub active: bool,
    pub with_x: bool,
}

impl<'a> Badge<'a> {
    pub fn new(field: &'a str, value: &'a str, kind: BadgeKind) -> Self {
        Self {
            field,
            value,
            kind,
            active: false,
            with_x: false,
        }
    }

    pub fn active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }

    pub fn with_x(mut self, with_x: bool) -> Self {
        self.with_x = with_x;
        self
    }

    pub fn show(self, ui: &mut egui::Ui) -> BadgeAction {
        let (bg, border, fg) = colors_for(&self.kind, self.active);

        // Compute label text (kind-specific prefix where useful).
        let label = display_label(&self.kind, self.value);

        let font = FontId::monospace(11.0);
        let galley = ui.painter().layout_no_wrap(label.clone(), font.clone(), fg);
        let text_size = galley.size();

        let pad_x = 8.0;
        let pad_y = 2.0;
        let x_w = if self.with_x { 12.0 } else { 0.0 };
        let total = Vec2::new(text_size.x + pad_x * 2.0 + x_w, text_size.y + pad_y * 2.0);
        let (rect, resp) = ui.allocate_exact_size(total, Sense::click());
        // Surface an accessible label so test harnesses (egui_kittest)
        // can find and synthesise clicks.
        let access_label = format!("badge:{}={}", self.field, self.value);
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &access_label));

        let painter = ui.painter();
        let rounding = Rounding::same(999.0);

        // Hover glow — outer rect just outside the pill.
        if resp.hovered() {
            let outer = rect.expand(2.0);
            painter.rect_filled(outer, Rounding::same(999.0), tint(border, 0.25));
        }

        // Active state: stack a wider purple-tinted halo behind the
        // normal fill so the chip reads "selected" without losing its
        // kind colour.
        if self.active {
            let halo = rect.expand(1.5);
            painter.rect_filled(halo, Rounding::same(999.0), tint(palette::PURPLE, 0.40));
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
                FontId::proportional(12.0),
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
                _ => BadgeAction::Toggle {
                    field: self.field.to_string(),
                    value: self.value.to_string(),
                },
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
        BadgeKind::Generic => (palette::WHITE, palette::WHITE),
    };
    let bg = if active {
        tint(palette::PURPLE, 0.25)
    } else {
        palette::BLACK
    };
    (bg, border, fg)
}
