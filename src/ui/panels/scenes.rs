use eframe::egui;

const SCENE_ROW_HEIGHT: f32 = 28.0;
const TOOLBAR_HEIGHT: f32 = 36.0;
const TOOL_BUTTON_SIZE: f32 = 26.0;

#[derive(Clone, Copy)]
enum ToolIcon {
    Add,
    Remove,
    Duplicate,
    MoveUp,
    MoveDown,
}

pub(in crate::ui) fn show(ui: &mut egui::Ui) {
    show_toolbar(ui);

    egui::ScrollArea::vertical()
        .id_salt("scenes_list")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let row_width = ui.available_width();
            let _ = ui.add_sized(
                [row_width, SCENE_ROW_HEIGHT],
                egui::Button::new("Scene 1").selected(true),
            );
        });
}

fn show_toolbar(ui: &mut egui::Ui) {
    egui::Panel::bottom("scenes_toolbar")
        .exact_size(TOOLBAR_HEIGHT)
        .resizable(false)
        .frame(
            egui::Frame::new()
                .fill(ui.visuals().panel_fill)
                .inner_margin(egui::Margin::symmetric(4, 5)),
        )
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                tool_button(ui, ToolIcon::Add, "Add scene");
                tool_button(ui, ToolIcon::Remove, "Remove selected scene");
                tool_button(ui, ToolIcon::Duplicate, "Duplicate selected scene");
                tool_button(ui, ToolIcon::MoveUp, "Move selected scene up");
                tool_button(ui, ToolIcon::MoveDown, "Move selected scene down");
            });
        });
}

fn tool_button(ui: &mut egui::Ui, icon: ToolIcon, tooltip: &str) {
    let response = ui.add_sized([TOOL_BUTTON_SIZE, TOOL_BUTTON_SIZE], egui::Button::new(""));
    paint_icon(ui, &response, icon);
    let _ = response.on_hover_text(tooltip);
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
