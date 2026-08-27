//! The button strip both docks put along their bottom edge.
//!
//! Shared rather than duplicated because the icons are drawn from geometry
//! rather than loaded from assets: two copies would drift a pixel at a time,
//! and the two docks are meant to look like one control.

use eframe::egui;

pub(super) const HEIGHT: f32 = 36.0;
const BUTTON_SIZE: f32 = 26.0;
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

pub(super) fn button(
    ui: &mut egui::Ui,
    icon: ToolIcon,
    tooltip: impl Into<egui::WidgetText>,
    enabled: bool,
) -> bool {
    let response = ui.add_enabled(
        enabled,
        egui::Button::new("").min_size(egui::vec2(BUTTON_SIZE, BUTTON_SIZE)),
    );
    paint_icon(ui, &response, icon);
    response.on_hover_text(tooltip).clicked()
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
