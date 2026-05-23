//! Filter sidebar section: card-stream query builder.
//!
//! See `ui/query.rs` for the data model. This file is the egui renderer:
//! a horizontally-flowing strip of card widgets, plus a row of
//! secondary "add" buttons below.

use eframe::egui;

use crate::ui::query::{
    remove_matching_paren_close, remove_matching_paren_open, Card, ConnectorOp, Op,
};
use crate::ui::sections::reset_row;
use crate::ui::state::AppState;
use crate::ui::theme::{accent, palette};

pub fn show(ui: &mut egui::Ui, state: &mut AppState) {
    state.snapshot_source = Some("Filter".into());
    if reset_row(ui) {
        state.query = Default::default();
    }
    let mut delete_at: Option<usize> = None;
    let mut cycle_connector_at: Option<usize> = None;
    let mut cycle_op_at: Option<usize> = None;
    let mut append_filter = false;

    ui.horizontal_wrapped(|ui| {
        let len = state.query.cards.len();
        for i in 0..len {
            let card_clone = state.query.cards[i].clone();
            // Render uses a mutable borrow for the variants that own
            // editable strings/bools. We dispatch on a snapshot so we
            // can pass a mutable ref into the small renderers without
            // aliasing.
            render_card(
                ui,
                i,
                &mut state.query.cards[i],
                &card_clone,
                &mut delete_at,
                &mut cycle_connector_at,
                &mut cycle_op_at,
            );
        }

        // Tail "+" button: append a default Filter (preceded by AND).
        let plus = egui::Button::new(egui::RichText::new("+").monospace())
            .min_size(egui::vec2(24.0, 24.0))
            .stroke(egui::Stroke::new(1.0, palette::BORDER))
            .fill(egui::Color32::BLACK);
        if ui.add(plus).on_hover_text("Add filter").clicked() {
            append_filter = true;
        }
    });

    // Apply queued mutations.
    if append_filter {
        // Only prepend a connector if the last card isn't already a
        // connector / paren-open / NOT.
        let needs_connector = match state.query.cards.last() {
            Some(Card::Connector { .. })
            | Some(Card::ParenOpen)
            | Some(Card::Not) => false,
            None => false,
            _ => true,
        };
        if needs_connector {
            state.query.cards.push(Card::Connector {
                op: ConnectorOp::And,
            });
        }
        state.query.cards.push(Card::Filter {
            field: "tag".into(),
            op: Op::Eq,
            value: String::new(),
        });
    }
    if let Some(idx) = cycle_op_at {
        if let Some(Card::Filter { op, .. }) = state.query.cards.get_mut(idx) {
            *op = op.cycle();
        }
    }
    if let Some(idx) = cycle_connector_at {
        if let Some(Card::Connector { op }) = state.query.cards.get_mut(idx) {
            *op = match op {
                ConnectorOp::And => ConnectorOp::Or,
                ConnectorOp::Or => ConnectorOp::And,
            };
        }
    }
    if let Some(idx) = delete_at {
        // Search card has no delete button so this index can never
        // point at one in practice, but guard anyway.
        if idx < state.query.cards.len() {
            let removed = state.query.cards.remove(idx);
            match removed {
                Card::ParenOpen => remove_matching_paren_close(&mut state.query.cards, idx),
                Card::ParenClose => remove_matching_paren_open(&mut state.query.cards, idx),
                _ => {}
            }
        }
    }

    ui.add_space(8.0);

    // Secondary add-buttons row.
    ui.horizontal_wrapped(|ui| {
        if ui.button("+ and").clicked() {
            state.query.cards.push(Card::Connector {
                op: ConnectorOp::And,
            });
        }
        if ui.button("+ or").clicked() {
            state.query.cards.push(Card::Connector {
                op: ConnectorOp::Or,
            });
        }
        if ui.button("+ ( )").clicked() {
            state.query.cards.push(Card::ParenOpen);
            state.query.cards.push(Card::ParenClose);
        }
        if ui.button("+ not").clicked() {
            state.query.cards.push(Card::Not);
        }
        let clear_text =
            egui::RichText::new("Clear").color(accent::RED).size(12.0);
        if ui.button(clear_text).clicked() {
            state.query.clear();
        }
    });
}

fn render_card(
    ui: &mut egui::Ui,
    index: usize,
    card: &mut Card,
    snapshot: &Card,
    delete_at: &mut Option<usize>,
    cycle_connector_at: &mut Option<usize>,
    cycle_op_at: &mut Option<usize>,
) {
    let frame = egui::Frame::none()
        .stroke(egui::Stroke::new(1.0, palette::BORDER))
        .fill(egui::Color32::BLACK)
        .inner_margin(egui::Margin::symmetric(4.0, 2.0));

    frame.show(ui, |ui| {
        ui.horizontal(|ui| match card {
            Card::Search { value, regex } => {
                ui.label(egui::RichText::new("search:").monospace().size(11.0));
                ui.add(
                    egui::TextEdit::singleline(value)
                        .desired_width(120.0)
                        .hint_text("text…"),
                );
                let label = if *regex { ".*" } else { "abc" };
                if ui
                    .small_button(egui::RichText::new(label).monospace())
                    .on_hover_text("Toggle regex")
                    .clicked()
                {
                    *regex = !*regex;
                }
                // No delete: system card.
                let _ = snapshot;
            }
            Card::Filter { field, op, value } => {
                ui.add(
                    egui::TextEdit::singleline(field)
                        .desired_width(80.0)
                        .hint_text("field"),
                );
                if ui
                    .small_button(egui::RichText::new(op.label()).monospace())
                    .on_hover_text("Cycle operator")
                    .clicked()
                {
                    *cycle_op_at = Some(index);
                }
                ui.add(
                    egui::TextEdit::singleline(value)
                        .desired_width(80.0)
                        .hint_text("value"),
                );
                if ui
                    .small_button(egui::RichText::new("×").monospace())
                    .on_hover_text("Delete filter")
                    .clicked()
                {
                    *delete_at = Some(index);
                }
            }
            Card::Connector { op } => {
                if ui
                    .button(egui::RichText::new(op.label()).monospace().strong())
                    .on_hover_text("Click to toggle AND/OR")
                    .clicked()
                {
                    *cycle_connector_at = Some(index);
                }
                if ui
                    .small_button(egui::RichText::new("×").monospace())
                    .on_hover_text("Delete connector")
                    .clicked()
                {
                    *delete_at = Some(index);
                }
            }
            Card::ParenOpen => {
                ui.label(egui::RichText::new("(").monospace().strong());
                if ui
                    .small_button(egui::RichText::new("×").monospace())
                    .on_hover_text("Delete paren pair")
                    .clicked()
                {
                    *delete_at = Some(index);
                }
            }
            Card::ParenClose => {
                ui.label(egui::RichText::new(")").monospace().strong());
                if ui
                    .small_button(egui::RichText::new("×").monospace())
                    .on_hover_text("Delete paren pair")
                    .clicked()
                {
                    *delete_at = Some(index);
                }
            }
            Card::Not => {
                ui.label(egui::RichText::new("not").monospace().strong());
                if ui
                    .small_button(egui::RichText::new("×").monospace())
                    .on_hover_text("Delete NOT")
                    .clicked()
                {
                    *delete_at = Some(index);
                }
            }
        });
    });
}
