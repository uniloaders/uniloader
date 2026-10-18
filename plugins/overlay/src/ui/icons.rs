//! Small buttons whose icon is drawn, not spelled.
//!
//! `✕` and `⟲` are outside what the default font covers and came out as the missing-glyph
//! box in game. Drawing the icon removes the question and stays sharp at any scale.

use egui::{Pos2, Rect, Response, Sense, Stroke, Ui, Vec2};

/// Side of an icon button, in points; the height of a one-line text button.
const SIZE: f32 = 18.0;
/// How far the icon is inset from the button edge.
const INSET: f32 = 5.0;

/// Allocate a square button and let the caller paint it, with the usual hover feel.
fn square(ui: &mut Ui, hint: &str) -> (Response, Rect, Stroke) {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(SIZE), Sense::click());
    let response = response.on_hover_text(hint);
    let visuals = *ui.style().interact(&response);
    if response.hovered() || response.is_pointer_button_down_on() {
        ui.painter()
            .rect_filled(rect, visuals.corner_radius, visuals.weak_bg_fill);
    }
    let stroke = Stroke::new(1.6, visuals.fg_stroke.color);
    (response, rect.shrink(INSET), stroke)
}

/// A close button. Returns whether it was clicked.
pub(crate) fn close(ui: &mut Ui, hint: &str) -> bool {
    let (response, icon, stroke) = square(ui, hint);
    ui.painter()
        .line_segment([icon.left_top(), icon.right_bottom()], stroke);
    ui.painter()
        .line_segment([icon.right_top(), icon.left_bottom()], stroke);
    response.clicked()
}

/// A revert-to-default button: a circular arrow. Returns whether it was clicked.
pub(crate) fn revert(ui: &mut Ui, hint: &str) -> bool {
    let (response, icon, stroke) = square(ui, hint);
    let centre = icon.center();
    let radius = icon.width() / 2.0;
    // An almost closed circle, anticlockwise from the top, leaving a gap for the head.
    let arc: Vec<Pos2> = (0u8..=10)
        .map(|step| {
            let angle = std::f32::consts::FRAC_PI_2 + f32::from(step) * 0.52;
            centre + Vec2::new(angle.cos() * radius, -angle.sin() * radius)
        })
        .collect();
    let tip = arc[0];
    ui.painter().add(egui::Shape::line(arc, stroke));
    let head = vec![
        tip + Vec2::new(0.0, -radius * 0.55),
        tip + Vec2::new(radius * 0.55, radius * 0.1),
        tip + Vec2::new(-radius * 0.45, radius * 0.1),
    ];
    ui.painter()
        .add(egui::Shape::convex_polygon(head, stroke.color, stroke));
    response.clicked()
}
