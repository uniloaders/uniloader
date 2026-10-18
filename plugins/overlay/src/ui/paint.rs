//! Drawing the toasts, notices and dialogs [`crate::notices`] holds.

use egui::{Align2, Color32, Context, Frame, Id, Key, RichText, Vec2};
use uniloader_overlay::showcase::ShownCard;
use uniloader_overlay::{UiAnswer, UiDialog, UiLevel, UiNotice};

use crate::notices::{Entry, Kind};

/// Distance from the screen edge for the toast stack.
const MARGIN: f32 = 12.0;
/// Width of a toast card.
const TOAST_WIDTH: f32 = 320.0;

/// What one frame of drawing decided.
#[derive(Default)]
pub(crate) struct Outcome {
    /// Everything drawn this frame, so the queue can start their clocks.
    pub(crate) shown: Vec<u64>,
    /// Dialogs the user answered: id and how.
    pub(crate) answered: Vec<(u64, UiAnswer)>,
    /// Input dialogs whose text changed: id and the new text.
    pub(crate) typed: Vec<(u64, String)>,
}

/// The accent colour for a level.
fn colour(level: UiLevel) -> Color32 {
    match level {
        UiLevel::SUCCESS => Color32::from_rgb(120, 200, 120),
        UiLevel::WARNING => Color32::from_rgb(230, 190, 100),
        UiLevel::ERROR => Color32::from_rgb(230, 120, 120),
        // Info, and any level from a newer table than this overlay knows.
        _ => Color32::from_rgb(150, 180, 230),
    }
}

/// Draw everything queued.
pub(crate) fn draw(ctx: &Context, entries: &[Entry]) -> Outcome {
    let mut outcome = Outcome::default();
    let mut toast_offset = MARGIN;
    for entry in entries {
        match &entry.kind {
            Kind::Passing(UiNotice::TOAST, _) => toast(ctx, entry, &mut toast_offset),
            Kind::Passing(_, _) => notice(ctx, entry),
            Kind::Modal(kind, buffer) => dialog(ctx, entry, *kind, buffer, &mut outcome),
        }
        outcome.shown.push(entry.id);
    }
    outcome
}

/// A card in the corner, stacked under the ones before it.
fn toast(ctx: &Context, entry: &Entry, offset: &mut f32) {
    let opacity = entry.opacity();
    let area = egui::Area::new(Id::new(("overlay-toast", entry.id)))
        .anchor(Align2::RIGHT_TOP, Vec2::new(-MARGIN, *offset))
        .order(egui::Order::Foreground)
        .interactable(false);
    let response = area.show(ctx, |ui| {
        ui.set_opacity(opacity);
        Frame::popup(ui.style())
            .stroke(egui::Stroke::new(1.5, colour(entry.level)))
            .show(ui, |ui| {
                ui.set_width(TOAST_WIDTH);
                if !entry.title.is_empty() {
                    ui.label(
                        RichText::new(&entry.title)
                            .strong()
                            .color(colour(entry.level)),
                    );
                }
                ui.label(&entry.text);
            });
    });
    *offset += response.response.rect.height() + 8.0;
}

/// The showcase card at the bottom centre: a badge square, a bold title and a coloured line.
/// An icon file is not drawn here (egui would need it as a texture); the badge stands in.
pub(crate) fn card(ctx: &Context, card: &ShownCard) {
    let accent = colour(card.level);
    egui::Area::new(Id::new("overlay-card"))
        .anchor(Align2::CENTER_BOTTOM, Vec2::new(0.0, -MARGIN * 2.0))
        .order(egui::Order::Foreground)
        .interactable(false)
        .show(ctx, |ui| {
            Frame::popup(ui.style()).show(ui, |ui| {
                ui.horizontal(|ui| {
                    if !card.badge.is_empty() {
                        Frame::new()
                            .fill(accent)
                            .corner_radius(4.0)
                            .inner_margin(Vec2::new(8.0, 6.0))
                            .show(ui, |ui| {
                                ui.label(RichText::new(&card.badge).strong().color(Color32::BLACK));
                            });
                    }
                    ui.vertical(|ui| {
                        ui.label(RichText::new(&card.title).strong().size(18.0));
                        if !card.text.is_empty() {
                            ui.label(RichText::new(&card.text).color(accent));
                        }
                    });
                });
            });
        });
}

/// Large text in the middle, for the few things that must be read.
fn notice(ctx: &Context, entry: &Entry) {
    egui::Area::new(Id::new(("overlay-notice", entry.id)))
        .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
        .order(egui::Order::Foreground)
        .interactable(false)
        .show(ctx, |ui| {
            ui.set_opacity(entry.opacity());
            Frame::popup(ui.style())
                .stroke(egui::Stroke::new(1.5, colour(entry.level)))
                .show(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        if !entry.title.is_empty() {
                            ui.label(
                                RichText::new(&entry.title)
                                    .heading()
                                    .color(colour(entry.level)),
                            );
                        }
                        ui.label(RichText::new(&entry.text).size(20.0));
                    });
                });
        });
}

/// A modal window with buttons, and a line to type into for an input dialog.
fn dialog(ctx: &Context, entry: &Entry, kind: UiDialog, buffer: &str, outcome: &mut Outcome) {
    let title = if entry.title.is_empty() {
        "uniloader"
    } else {
        &entry.title
    };
    let accept = if !entry.accept.is_empty() {
        entry.accept.as_str()
    } else if kind == UiDialog::MESSAGE {
        "OK"
    } else {
        "Confirm"
    };
    let cancel = if entry.cancel.is_empty() {
        "Cancel"
    } else {
        &entry.cancel
    };
    let mut open = true;
    let mut text = buffer.to_owned();
    egui::Window::new(title)
        .id(Id::new(("overlay-dialog", entry.id)))
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
        .open(&mut open)
        .show(ctx, |ui| {
            ui.set_max_width(420.0);
            ui.label(RichText::new(&entry.text).color(colour(entry.level)));
            ui.add_space(6.0);
            if kind == UiDialog::INPUT {
                let response = ui
                    .add(egui::TextEdit::singleline(&mut text).desired_width(ui.available_width()));
                response.request_focus();
                if response.has_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    outcome.answered.push((entry.id, UiAnswer::ACCEPTED));
                }
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui.button(accept).clicked() {
                    outcome.answered.push((entry.id, UiAnswer::ACCEPTED));
                }
                if kind != UiDialog::MESSAGE && ui.button(cancel).clicked() {
                    outcome.answered.push((entry.id, UiAnswer::CANCELLED));
                }
            });
        });
    if text != buffer {
        outcome.typed.push((entry.id, text));
    }
    // Escape and the window's close button both mean no.
    if !open || ctx.input(|i| i.key_pressed(Key::Escape)) {
        outcome.answered.push((entry.id, UiAnswer::CANCELLED));
    }
}
