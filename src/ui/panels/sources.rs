use eframe::egui;

use crate::domain::{SceneId, SourceKind};
use crate::project::{ProjectCommand, SourceCommand};
use crate::snapshots::{SceneItemSnapshot, SourcesSnapshot};
use crate::ui::UiAction;
use crate::ui::editor::SceneEditorState;

const SOURCE_ROW_HEIGHT: f32 = 28.0;
const ICON_WIDTH: f32 = 22.0;
const TOOLBAR_HEIGHT: f32 = 36.0;
const TOOL_BUTTON_SIZE: f32 = 26.0;

#[derive(Default)]
pub(in crate::ui) struct SourcesPanelState {
    scene_id: Option<SceneId>,
    known_item_count: usize,
    add_dialog_open: bool,
    select_new_item: bool,
}

pub(in crate::ui) fn show(
    ui: &mut egui::Ui,
    state: &mut SourcesPanelState,
    editor: &mut SceneEditorState,
    snapshot: &SourcesSnapshot,
    actions: &mut Vec<UiAction>,
) {
    if state.scene_id != snapshot.scene_id {
        state.scene_id = snapshot.scene_id;
        state.known_item_count = snapshot.items.len();
        state.select_new_item = false;
    }
    if state.select_new_item && snapshot.items.len() > state.known_item_count {
        if let Some(item) = snapshot.items.first() {
            editor.select(item.id);
        }
        state.select_new_item = false;
    }
    state.known_item_count = snapshot.items.len();

    show_toolbar(ui, state, snapshot);

    if snapshot.items.is_empty() {
        let scene_name = snapshot.scene_name.as_deref().unwrap_or("selected scene");
        ui.centered_and_justified(|ui| {
            ui.weak(format!("No sources in {scene_name}"));
        });
    } else {
        egui::ScrollArea::vertical()
            .id_salt("sources_list")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for item in &snapshot.items {
                    show_source_row(ui, editor, item);
                }
            });
    }

    show_add_dialog(ui.ctx(), state, snapshot, actions);
}

fn show_source_row(ui: &mut egui::Ui, editor: &mut SceneEditorState, item: &SceneItemSnapshot) {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), SOURCE_ROW_HEIGHT),
        egui::Sense::click(),
    );
    let response = response.on_hover_text(item.kind.display_name());
    let selected = editor.selected_item_id() == Some(item.id);
    if selected {
        ui.painter()
            .rect_filled(rect, 3.0, ui.visuals().selection.bg_fill);
    } else if response.hovered() {
        ui.painter()
            .rect_filled(rect, 3.0, ui.visuals().widgets.hovered.bg_fill);
    }

    let color = if selected {
        ui.visuals().selection.stroke.color
    } else {
        ui.visuals().text_color()
    };
    let eye_center = egui::pos2(rect.left() + ICON_WIDTH * 0.5, rect.center().y);
    paint_visibility(ui.painter(), eye_center, item.visible, color);
    let lock_center = egui::pos2(rect.left() + ICON_WIDTH * 1.5, rect.center().y);
    paint_lock(ui.painter(), lock_center, item.locked, color);
    ui.painter().text(
        egui::pos2(rect.left() + ICON_WIDTH * 2.0 + 4.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        &item.name,
        egui::TextStyle::Body.resolve(ui.style()),
        color,
    );

    if response.clicked() {
        editor.select(item.id);
    }
}

fn show_toolbar(ui: &mut egui::Ui, state: &mut SourcesPanelState, snapshot: &SourcesSnapshot) {
    egui::Panel::bottom("sources_toolbar")
        .exact_size(TOOLBAR_HEIGHT)
        .resizable(false)
        .frame(
            egui::Frame::new()
                .fill(ui.visuals().panel_fill)
                .inner_margin(egui::Margin::symmetric(4, 5)),
        )
        .show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                let response = ui.add_enabled(
                    snapshot.scene_id.is_some(),
                    egui::Button::new("").min_size(egui::vec2(TOOL_BUTTON_SIZE, TOOL_BUTTON_SIZE)),
                );
                let center = response.rect.center();
                let stroke = ui.style().interact(&response).fg_stroke;
                ui.painter().line_segment(
                    [
                        center + egui::vec2(-5.0, 0.0),
                        center + egui::vec2(5.0, 0.0),
                    ],
                    stroke,
                );
                ui.painter().line_segment(
                    [
                        center + egui::vec2(0.0, -5.0),
                        center + egui::vec2(0.0, 5.0),
                    ],
                    stroke,
                );
                if response.on_hover_text("Add source").clicked() {
                    state.add_dialog_open = true;
                }
            });
        });
}

fn show_add_dialog(
    ctx: &egui::Context,
    state: &mut SourcesPanelState,
    snapshot: &SourcesSnapshot,
    actions: &mut Vec<UiAction>,
) {
    if !state.add_dialog_open {
        return;
    }

    let mut open = true;
    let mut add_color = false;
    let mut cancel = false;
    egui::Window::new("Add Source")
        .id(egui::Id::new("add_source_dialog"))
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .collapsible(false)
        .resizable(false)
        .open(&mut open)
        .show(ctx, |ui| {
            ui.set_min_width(280.0);
            ui.label("Source type");
            ui.separator();
            if ui
                .selectable_label(true, SourceKind::Color.display_name())
                .double_clicked()
            {
                add_color = true;
            }
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui.button("Add").clicked() {
                    add_color = true;
                }
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
            });
        });

    if cancel {
        open = false;
    } else if add_color {
        if let Some(scene_id) = snapshot.scene_id {
            actions.push(UiAction::Project(ProjectCommand::Source(
                SourceCommand::AddColor(scene_id),
            )));
            state.select_new_item = true;
        }
        open = false;
    }
    state.add_dialog_open = open;
}

fn paint_visibility(
    painter: &egui::Painter,
    center: egui::Pos2,
    visible: bool,
    color: egui::Color32,
) {
    let stroke = egui::Stroke::new(1.2, color);
    painter.circle_stroke(center, 5.0, stroke);
    if visible {
        painter.circle_filled(center, 1.8, color);
    } else {
        painter.line_segment(
            [
                center + egui::vec2(-4.5, 4.5),
                center + egui::vec2(4.5, -4.5),
            ],
            stroke,
        );
    }
}

fn paint_lock(painter: &egui::Painter, center: egui::Pos2, locked: bool, color: egui::Color32) {
    let stroke = egui::Stroke::new(1.2, color);
    let body = egui::Rect::from_center_size(center + egui::vec2(0.0, 2.0), egui::vec2(8.0, 7.0));
    painter.rect_stroke(body, 1.0, stroke, egui::StrokeKind::Inside);

    let x_offset = if locked { 0.0 } else { 2.0 };
    painter.line_segment(
        [
            center + egui::vec2(-3.0 + x_offset, -1.5),
            center + egui::vec2(-3.0 + x_offset, -4.5),
        ],
        stroke,
    );
    painter.line_segment(
        [
            center + egui::vec2(-3.0 + x_offset, -4.5),
            center + egui::vec2(3.0 + x_offset, -4.5),
        ],
        stroke,
    );
    if locked {
        painter.line_segment(
            [
                center + egui::vec2(3.0, -4.5),
                center + egui::vec2(3.0, -1.5),
            ],
            stroke,
        );
    }
}
