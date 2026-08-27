use eframe::egui;

use crate::project::{ProjectCommand, SceneCommand};
use crate::snapshots::ScenesSnapshot;

use super::super::UiAction;

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

pub(in crate::ui) fn show(
    ui: &mut egui::Ui,
    snapshot: &ScenesSnapshot,
    actions: &mut Vec<UiAction>,
) {
    show_toolbar(ui, snapshot, actions);

    egui::ScrollArea::vertical()
        .id_salt("scenes_list")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for scene in &snapshot.items {
                let selected = snapshot.selected_scene_id == Some(scene.id);
                let row_width = ui.available_width();
                if ui
                    .add_sized(
                        [row_width, SCENE_ROW_HEIGHT],
                        egui::Button::new(&scene.name).selected(selected),
                    )
                    .clicked()
                {
                    actions.push(scene_action(SceneCommand::Select(scene.id)));
                }
            }
        });
}

fn show_toolbar(ui: &mut egui::Ui, snapshot: &ScenesSnapshot, actions: &mut Vec<UiAction>) {
    let selected = snapshot.selected_scene_id;
    let selected_index =
        selected.and_then(|selected| snapshot.items.iter().position(|scene| scene.id == selected));

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
                if tool_button(ui, ToolIcon::Add, "Add scene", true) {
                    actions.push(scene_action(SceneCommand::Add));
                }
                if tool_button(
                    ui,
                    ToolIcon::Remove,
                    "Remove selected scene",
                    selected.is_some() && snapshot.items.len() > 1,
                ) && let Some(scene_id) = selected
                {
                    actions.push(scene_action(SceneCommand::Delete(scene_id)));
                }
                if tool_button(
                    ui,
                    ToolIcon::Duplicate,
                    "Duplicate selected scene",
                    selected.is_some(),
                ) && let Some(scene_id) = selected
                {
                    actions.push(scene_action(SceneCommand::Duplicate(scene_id)));
                }
                if tool_button(
                    ui,
                    ToolIcon::MoveUp,
                    "Move selected scene up",
                    selected_index.is_some_and(|index| index > 0),
                ) && let Some(scene_id) = selected
                {
                    actions.push(scene_action(SceneCommand::MoveUp(scene_id)));
                }
                if tool_button(
                    ui,
                    ToolIcon::MoveDown,
                    "Move selected scene down",
                    selected_index.is_some_and(|index| index + 1 < snapshot.items.len()),
                ) && let Some(scene_id) = selected
                {
                    actions.push(scene_action(SceneCommand::MoveDown(scene_id)));
                }
            });
        });
}

fn scene_action(command: SceneCommand) -> UiAction {
    UiAction::Project(ProjectCommand::Scene(command))
}

fn tool_button(ui: &mut egui::Ui, icon: ToolIcon, tooltip: &str, enabled: bool) -> bool {
    let response = ui.add_enabled(
        enabled,
        egui::Button::new("").min_size(egui::vec2(TOOL_BUTTON_SIZE, TOOL_BUTTON_SIZE)),
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
