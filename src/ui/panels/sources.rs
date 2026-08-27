use eframe::egui;

use crate::domain::SceneItemId;
use crate::snapshots::{SceneItemSnapshot, SourcesSnapshot};

const SOURCE_ROW_HEIGHT: f32 = 28.0;
const ICON_WIDTH: f32 = 22.0;

#[derive(Default)]
pub(in crate::ui) struct SourcesPanelState {
    scene_id: Option<crate::domain::SceneId>,
    pub(super) selected_item_id: Option<SceneItemId>,
}

pub(in crate::ui) fn show(
    ui: &mut egui::Ui,
    state: &mut SourcesPanelState,
    snapshot: &SourcesSnapshot,
) {
    if state.scene_id != snapshot.scene_id {
        state.scene_id = snapshot.scene_id;
        state.selected_item_id = None;
    }
    if state
        .selected_item_id
        .is_some_and(|selected| !snapshot.items.iter().any(|item| item.id == selected))
    {
        state.selected_item_id = None;
    }

    if snapshot.items.is_empty() {
        let scene_name = snapshot.scene_name.as_deref().unwrap_or("selected scene");
        ui.centered_and_justified(|ui| {
            ui.weak(format!("No sources in {scene_name}"));
        });
        return;
    }

    egui::ScrollArea::vertical()
        .id_salt("sources_list")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for item in &snapshot.items {
                show_source_row(ui, state, item);
            }
        });
}

fn show_source_row(ui: &mut egui::Ui, state: &mut SourcesPanelState, item: &SceneItemSnapshot) {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), SOURCE_ROW_HEIGHT),
        egui::Sense::click(),
    );
    let response = response.on_hover_text(item.kind.display_name());
    let selected = state.selected_item_id == Some(item.id);
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
        state.selected_item_id = Some(item.id);
    }
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
