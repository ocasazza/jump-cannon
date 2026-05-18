//! Reusable floating-panel wrapper.
//!
//! Wraps an `egui::Window` with the project's standard floating chrome:
//! squircle background painted in `FLOATING_BACKDROP` with a 1px
//! `palette::BORDER` stroke, a custom header row (label + close `X`
//! button) replacing the default title bar, and an `&mut bool`
//! open/closed flag driven by the X button and the tray launcher row.
//!
//! All floating panels in the app go through this type — adding a new
//! one should be a 5-line builder call, not a copy of `egui::Window`
//! plumbing.

use eframe::egui::{self, Color32, Id, Rect, Stroke};

use crate::ui::squircle;
use crate::ui::state::PanelId;
use crate::ui::theme::{self, palette};

/// Builder for a squircle-backed floating panel whose visibility is
/// driven by a caller-owned `&mut bool`.
pub struct FloatingPanel {
    id: PanelId,
    title: &'static str,
    default_pos: Option<[f32; 2]>,
    default_size: Option<[f32; 2]>,
}

impl FloatingPanel {
    pub fn new(id: PanelId, title: &'static str) -> Self {
        Self {
            id,
            title,
            default_pos: None,
            default_size: None,
        }
    }

    pub fn default_pos(mut self, pos: [f32; 2]) -> Self {
        self.default_pos = Some(pos);
        self
    }

    pub fn default_size(mut self, size: [f32; 2]) -> Self {
        self.default_size = Some(size);
        self
    }

    /// Render the panel. Skips entirely when `!*open`. The X button in
    /// the custom header sets `*open = false`.
    pub fn show<R>(
        self,
        ctx: &egui::Context,
        open: &mut bool,
        body: impl FnOnce(&mut egui::Ui) -> R,
    ) -> Option<R> {
        if !*open {
            return None;
        }

        let frame = theme::floating_frame()
            .fill(Color32::TRANSPARENT)
            .stroke(Stroke::NONE);

        let mut window = egui::Window::new(self.title)
            .id(Id::new(("floating", self.id)))
            .title_bar(false)
            .frame(frame)
            .resizable(true)
            .movable(true)
            .collapsible(false);

        if let Some(pos) = self.default_pos {
            window = window.default_pos(pos);
        }
        if let Some(size) = self.default_size {
            window = window.default_size(size);
        }

        let title = self.title;

        let response = window.show(ctx, |ui| {
            let rect: Rect = ui.max_rect().expand(theme::spacing::SECTION_GAP);
            let mut painter = ui.painter().clone();
            painter.set_layer_id(egui::LayerId::new(
                egui::Order::Background,
                ui.layer_id().id,
            ));
            squircle::paint_squircle(
                &painter,
                rect,
                10.0,
                theme::FLOATING_BACKDROP,
                Stroke::new(1.0, palette::BORDER),
            );

            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(title)
                        .font(theme::mono(theme::font_size::HEADING))
                        .color(palette::TEXT),
                );
                ui.with_layout(
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        if ui.small_button("X").clicked() {
                            *open = false;
                        }
                    },
                );
            });
            ui.separator();

            body(ui)
        });

        response.and_then(|r| r.inner)
    }
}
