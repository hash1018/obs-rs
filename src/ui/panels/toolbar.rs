//! The button strip both docks put along their bottom edge.
//!
//! Shared rather than duplicated because the icons are drawn from geometry
//! rather than loaded from assets: two copies would drift a pixel at a time,
//! and the two docks are meant to look like one control.

use eframe::egui;

use crate::ui::docking::PANEL_MARGIN;

const BUTTON_SIZE: f32 = 26.0;

/// Tall enough to leave the dock's own margin above and below the buttons.
///
/// Derived rather than chosen, so the gap under the buttons keeps matching
/// the gap beside them when either changes.
pub(super) const HEIGHT: f32 = BUTTON_SIZE + 2.0 * PANEL_MARGIN;
const SIDE_MARGIN: i8 = 4;

/// Draws a dock's bottom button strip.
///
/// Owning the panel here rather than in each dock is what keeps the two
/// looking like one control. The vertical centring is left to the layout
/// instead of being arranged by margins that happen to add up to the button
/// height — that arithmetic is only correct until one of the three numbers
/// changes, and it fails silently by a few pixels when it stops being.
pub(super) fn strip(ui: &mut egui::Ui, id: &'static str, contents: impl FnOnce(&mut egui::Ui)) {
    egui::Panel::bottom(id)
        .exact_size(HEIGHT)
        .resizable(false)
        .frame(
            egui::Frame::new()
                .fill(ui.visuals().panel_fill)
                .inner_margin(egui::Margin::symmetric(SIDE_MARGIN, 0)),
        )
        .show(ui, |ui| {
            ui.horizontal_centered(contents);
        });
}

#[derive(Clone, Copy)]
pub(super) enum ToolIcon {
    Add,
    Remove,
    Duplicate,
    MoveUp,
    MoveDown,
}

/// Returns the `Response` rather than a bare `clicked()`, so callers keep the
/// egui idiom and the button's own rectangle stays measurable.
pub(super) fn button(
    ui: &mut egui::Ui,
    icon: ToolIcon,
    tooltip: impl Into<egui::WidgetText>,
    enabled: bool,
) -> egui::Response {
    let response = ui.add_enabled(
        enabled,
        egui::Button::new("").min_size(egui::vec2(BUTTON_SIZE, BUTTON_SIZE)),
    );
    paint_icon(ui, &response, icon);
    response.on_hover_text(tooltip)
}

fn paint_icon(ui: &egui::Ui, response: &egui::Response, icon: ToolIcon) {
    let center = response.rect.center();
    let stroke = ui.style().interact(response).fg_stroke;
    let painter = ui.painter();

    match icon {
        ToolIcon::Add => {
            painter.line_segment(
                [
                    center + egui::vec2(-5.0, 0.0),
                    center + egui::vec2(5.0, 0.0),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    center + egui::vec2(0.0, -5.0),
                    center + egui::vec2(0.0, 5.0),
                ],
                stroke,
            );
        }
        ToolIcon::Remove => {
            painter.line_segment(
                [
                    center + egui::vec2(-5.0, 0.0),
                    center + egui::vec2(5.0, 0.0),
                ],
                stroke,
            );
        }
        ToolIcon::Duplicate => {
            painter.rect_stroke(
                egui::Rect::from_center_size(center + egui::vec2(-2.0, -2.0), egui::vec2(8.0, 8.0)),
                0.0,
                stroke,
                egui::StrokeKind::Inside,
            );
            painter.rect_stroke(
                egui::Rect::from_center_size(center + egui::vec2(2.0, 2.0), egui::vec2(8.0, 8.0)),
                0.0,
                stroke,
                egui::StrokeKind::Inside,
            );
        }
        ToolIcon::MoveUp => {
            painter.line_segment(
                [
                    center + egui::vec2(-5.0, 2.5),
                    center + egui::vec2(0.0, -2.5),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    center + egui::vec2(0.0, -2.5),
                    center + egui::vec2(5.0, 2.5),
                ],
                stroke,
            );
        }
        ToolIcon::MoveDown => {
            painter.line_segment(
                [
                    center + egui::vec2(-5.0, -2.5),
                    center + egui::vec2(0.0, 2.5),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    center + egui::vec2(0.0, 2.5),
                    center + egui::vec2(5.0, -2.5),
                ],
                stroke,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AGENTS.md requires dock toolbar controls to stay vertically centred.
    /// Measured rather than eyeballed: the offset that matters is a few
    /// pixels, which is exactly the size of mistake a screenshot hides.
    #[test]
    fn buttons_sit_in_the_middle_of_the_strip() {
        let context = egui::Context::default();
        let mut measured = None;
        let mut output = context.run_ui(Default::default(), |context| {
            egui::CentralPanel::default().show(context, |ui| {
                strip(ui, "toolbar_under_test", |ui| {
                    let available = ui.max_rect();
                    let first = button(ui, ToolIcon::Add, "add", true).rect;
                    let _ = button(ui, ToolIcon::MoveUp, "up", true);
                    measured = Some((available, first));
                });
            });
        });
        // Nothing uploads these outside a real renderer, and epaint panics on
        // a delta that is dropped unapplied.
        output.textures_delta.clear();

        let (available, used) = measured.expect("the strip should have drawn");
        assert_eq!(used.height(), BUTTON_SIZE, "button height");
        // The panel's own separator takes a pixel, so the row gets slightly
        // less than `HEIGHT`; what matters is that it gets all of what is left.
        assert!(
            (HEIGHT - available.height()) <= 1.0,
            "the row was offered {} of {HEIGHT}",
            available.height()
        );
        assert!(
            (used.center().y - available.center().y).abs() < 0.5,
            "buttons centred at {} but the strip's middle is {}",
            used.center().y,
            available.center().y,
        );
        // The gap under the buttons is what the dock leaves beside them; the
        // strip sits flush against the dock's bottom edge, so this is the only
        // thing keeping the two from disagreeing.
        let gap = (available.height() - used.height()) / 2.0;
        assert!(
            (gap - PANEL_MARGIN).abs() <= 1.0,
            "gap around the buttons is {gap}, dock margin is {PANEL_MARGIN}"
        );
    }
}
