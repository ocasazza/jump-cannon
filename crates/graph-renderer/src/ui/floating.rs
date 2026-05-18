//! Reusable floating-panel wrapper.
//!
//! Wraps an `egui::Window` with the project's standard floating chrome:
//! squircle background painted in `FLOATING_BACKDROP` with a 1px
//! `palette::BORDER` stroke, a custom header row (label + close `X`
//! button) replacing the default title bar, and tray-aware
//! show/hide based on [`TrayState`].
//!
//! Collapsing a panel via the `X` calls `tray.collapse(id)`; the panel
//! then renders nothing. The tray strip (rendered elsewhere) is
//! responsible for restoring panels — that is intentionally not the
//! panel's job.

use eframe::egui::{self, Color32, Id, Rect, Stroke};

use crate::ui::squircle;
use crate::ui::state::{PanelId, TrayState};
use crate::ui::theme::{self, palette};

/// Builder for a tray-aware, squircle-backed floating panel.
///
/// Construct with [`FloatingPanel::new`], optionally configure
/// `default_pos` / `default_size`, then call [`FloatingPanel::show`]
/// with the egui context, the app's `TrayState`, and the body closure.
pub struct FloatingPanel {
    id: PanelId,
    title: &'static str,
    default_pos: Option<[f32; 2]>,
    default_size: Option<[f32; 2]>,
}

impl FloatingPanel {
    /// New floating-panel wrapper for `id`. `title` is rendered into
    /// the custom header row.
    pub fn new(id: PanelId, title: &'static str) -> Self {
        Self {
            id,
            title,
            default_pos: None,
            default_size: None,
        }
    }

    /// Initial window position (egui only honours this on first
    /// appearance; subsequent positions come from the egui memory
    /// keyed by the panel id).
    pub fn default_pos(mut self, pos: [f32; 2]) -> Self {
        self.default_pos = Some(pos);
        self
    }

    /// Initial window size. Same first-appearance-only semantics as
    /// `default_pos`.
    pub fn default_size(mut self, size: [f32; 2]) -> Self {
        self.default_size = Some(size);
        self
    }

    /// Render the panel. If `tray.is_collapsed(id)`, the closure is
    /// not invoked and no window is drawn. Otherwise the window opens
    /// with the custom squircle backdrop and header row, and `body`
    /// runs below a separator.
    pub fn show<R>(
        self,
        ctx: &egui::Context,
        tray: &mut TrayState,
        body: impl FnOnce(&mut egui::Ui) -> R,
    ) -> Option<R> {
        if tray.is_collapsed(self.id) {
            return None;
        }

        // Custom frame: start from the project's floating preset, then
        // null out fill + stroke so the squircle painted underneath
        // does all the visible work. Keeps inner_margin / no-shadow.
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

        let id = self.id;
        let title = self.title;

        let response = window.show(ctx, |ui| {
            // Paint the squircle backdrop behind the content. We use
            // a painter scoped to the entire window rect (ui.max_rect
            // here is the post-margin content rect; the parent frame
            // is transparent, so painting onto the layer at this rect
            // covers the inner area — the 8px inner_margin keeps
            // content from touching the squircle stroke).
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

            // Custom header row: title left, close button right.
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
                            tray.collapse(id);
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
