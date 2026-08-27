mod gizmo;
mod state;
mod toolbar;
mod viewport_transform;

use eframe::egui;

use crate::domain::{ColorSourceSettings, SourceSettings, Transform};
use crate::i18n::{LocalizationManager, TextKey};
use crate::project::{ProjectCommand, SourceCommand};
use crate::snapshots::{SceneItemSnapshot, SourcesSnapshot};

use super::UiAction;
use super::editor::{SceneEditorState, TransformDrag, TransformDragMode};
use viewport_transform::{ViewportTransform, fit_aspect_ratio};

const PREVIEW_MARGIN: i8 = 18;
pub(super) use state::PreviewViewState;

pub(super) fn show(
    ui: &mut egui::Ui,
    view_state: &mut PreviewViewState,
    editor: &mut SceneEditorState,
    snapshot: &SourcesSnapshot,
    i18n: &LocalizationManager,
    actions: &mut Vec<UiAction>,
) {
    egui::CentralPanel::default()
        .frame(
            egui::Frame::central_panel(ui.style())
                .fill(ui.visuals().faint_bg_color)
                .inner_margin(egui::Margin::same(PREVIEW_MARGIN)),
        )
        .show(ui, |ui| {
            let available_rect = ui.available_rect_before_wrap();
            let workspace_rect = egui::Rect::from_min_max(
                available_rect.min,
                egui::pos2(
                    available_rect.right(),
                    available_rect.bottom() - toolbar::TOOLBAR_HEIGHT - toolbar::TOOLBAR_GAP,
                ),
            );
            let viewport_bounds = workspace_rect.size() * view_state.scale();
            let viewport_size = fit_aspect_ratio(viewport_bounds, snapshot.canvas.aspect_ratio());
            let viewport_rect =
                egui::Rect::from_center_size(workspace_rect.center(), viewport_size);
            let viewport = ViewportTransform::new(viewport_rect, snapshot.canvas);
            let response = ui.interact(
                workspace_rect,
                egui::Id::new("preview_workspace_interaction"),
                egui::Sense::click_and_drag(),
            );

            handle_pointer(ui, &response, viewport, editor, snapshot, actions);
            paint_editor_overflow(ui, workspace_rect, viewport, editor, snapshot);
            paint_composite_frame_placeholder(ui, viewport, i18n);
            paint_editor_overlay(ui, workspace_rect, viewport, editor, snapshot);

            let toolbar_rect = egui::Rect::from_min_size(
                egui::pos2(
                    viewport_rect.left(),
                    viewport_rect.bottom() + toolbar::TOOLBAR_GAP,
                ),
                egui::vec2(toolbar::TOOLBAR_WIDTH, toolbar::TOOLBAR_HEIGHT),
            );
            let mut toolbar_ui = ui.new_child(
                egui::UiBuilder::new()
                    .id_salt("preview_toolbar")
                    .max_rect(toolbar_rect)
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
            );
            toolbar_ui.set_clip_rect(toolbar_rect);
            toolbar::show(&mut toolbar_ui, view_state, i18n);
        });
}

fn handle_pointer(
    ui: &egui::Ui,
    response: &egui::Response,
    viewport: ViewportTransform,
    editor: &mut SceneEditorState,
    snapshot: &SourcesSnapshot,
    actions: &mut Vec<UiAction>,
) {
    let pointer = response.interact_pointer_pos();

    if response.hovered()
        && let Some(pointer) = pointer
        && let Some((handle, _)) = selected_handle_at(pointer, viewport, editor, snapshot)
    {
        ui.ctx().set_cursor_icon(gizmo::cursor(handle));
    }

    if response.dragged() && editor.drag.is_none() {
        let drag_origin = ui.input(|input| input.pointer.press_origin()).or(pointer);
        if let Some(drag_origin) = drag_origin {
            begin_drag(drag_origin, viewport, editor, snapshot);
        }
    }

    if response.dragged()
        && let Some(drag) = editor.drag
        && let Some(item) = snapshot.items.iter().find(|item| item.id == drag.item_id)
        && let Some(settings) = color_settings(item)
    {
        // TransformDrag::original is captured once at the beginning, so this must
        // use the total gesture delta rather than the previous frame's delta.
        let screen_delta = response.total_drag_delta().unwrap_or_default();
        let delta = viewport.screen_delta_to_canvas(screen_delta);
        let transform = match drag.mode {
            TransformDragMode::Move => Transform {
                position: [
                    drag.original.position[0] + delta.x,
                    drag.original.position[1] + delta.y,
                ],
                ..drag.original
            },
            TransformDragMode::Resize(handle) => {
                let original_rect = item_canvas_rect(item, drag.original, settings);
                transform_from_rect(
                    gizmo::resize_rect(original_rect, handle, delta),
                    drag.original,
                    settings,
                    item,
                )
            }
        };
        editor.transform_override = Some((drag.item_id, transform));
        ui.ctx().request_repaint();
    }

    if response.drag_stopped()
        && let Some(drag) = editor.drag.take()
        && let Some((item_id, transform)) = editor.transform_override
        && item_id == drag.item_id
        && transform != drag.original
    {
        actions.push(UiAction::Project(ProjectCommand::Source(
            SourceCommand::SetTransform(item_id, transform),
        )));
    }

    if response.clicked()
        && let Some(pointer) = pointer
    {
        if let Some(item) = hit_test_item(pointer, viewport, editor, snapshot) {
            editor.select(item.id);
        } else {
            editor.clear_selection();
        }
    }
}

fn begin_drag(
    drag_origin: egui::Pos2,
    viewport: ViewportTransform,
    editor: &mut SceneEditorState,
    snapshot: &SourcesSnapshot,
) {
    if let Some((handle, item)) = selected_handle_at(drag_origin, viewport, editor, snapshot) {
        if !item.locked {
            let original = editor.effective_transform(item.id, item.transform);
            editor.drag = Some(TransformDrag {
                item_id: item.id,
                original,
                mode: TransformDragMode::Resize(handle),
            });
        }
    } else if let Some(item) = hit_test_item(drag_origin, viewport, editor, snapshot) {
        editor.select(item.id);
        if !item.locked {
            let original = editor.effective_transform(item.id, item.transform);
            editor.drag = Some(TransformDrag {
                item_id: item.id,
                original,
                mode: TransformDragMode::Move,
            });
        }
    } else {
        editor.clear_selection();
    }
}

fn paint_composite_frame_placeholder(
    ui: &egui::Ui,
    viewport: ViewportTransform,
    i18n: &LocalizationManager,
) {
    let painter = ui.painter().with_clip_rect(viewport.viewport());
    painter.rect_filled(viewport.viewport(), 0.0, egui::Color32::BLACK);
    painter.rect_stroke(
        viewport.viewport(),
        0.0,
        egui::Stroke::new(1.0, egui::Color32::from_gray(64)),
        egui::StrokeKind::Inside,
    );
    painter.text(
        viewport.viewport().center(),
        egui::Align2::CENTER_CENTER,
        i18n.text(TextKey::PreviewNoFrame),
        egui::FontId::proportional(14.0),
        egui::Color32::from_gray(132),
    );
}

fn paint_editor_overflow(
    ui: &egui::Ui,
    workspace: egui::Rect,
    viewport: ViewportTransform,
    editor: &SceneEditorState,
    snapshot: &SourcesSnapshot,
) {
    let Some(item) = selected_item(editor, snapshot).filter(|item| item.visible) else {
        return;
    };
    let Some(settings) = color_settings(item) else {
        return;
    };
    let transform = editor.effective_transform(item.id, item.transform);
    let source_rect = viewport.canvas_rect_to_screen(item_canvas_rect(item, transform, settings));
    let color = egui::Color32::from_rgba_unmultiplied(
        settings.rgba[0],
        settings.rgba[1],
        settings.rgba[2],
        settings.rgba[3],
    )
    .gamma_multiply(0.65);

    for overflow_rect in workspace_overflow_rects(workspace, viewport.viewport()) {
        if overflow_rect.is_positive() {
            ui.painter()
                .with_clip_rect(overflow_rect)
                .rect_filled(source_rect, 0.0, color);
        }
    }
}

fn paint_editor_overlay(
    ui: &egui::Ui,
    workspace: egui::Rect,
    viewport: ViewportTransform,
    editor: &SceneEditorState,
    snapshot: &SourcesSnapshot,
) {
    let Some(item) = selected_item(editor, snapshot) else {
        return;
    };
    let Some(settings) = color_settings(item) else {
        return;
    };
    let transform = editor.effective_transform(item.id, item.transform);
    let rect = viewport.canvas_rect_to_screen(item_canvas_rect(item, transform, settings));
    let painter = ui.painter().with_clip_rect(workspace);
    gizmo::paint(&painter, ui.visuals().selection.bg_fill, rect);
}

fn selected_handle_at<'a>(
    pointer: egui::Pos2,
    viewport: ViewportTransform,
    editor: &SceneEditorState,
    snapshot: &'a SourcesSnapshot,
) -> Option<(super::editor::ResizeHandle, &'a SceneItemSnapshot)> {
    let item = selected_item(editor, snapshot)?;
    let settings = color_settings(item)?;
    let transform = editor.effective_transform(item.id, item.transform);
    let rect = viewport.canvas_rect_to_screen(item_canvas_rect(item, transform, settings));
    gizmo::hit_test(rect, pointer).map(|handle| (handle, item))
}

fn selected_item<'a>(
    editor: &SceneEditorState,
    snapshot: &'a SourcesSnapshot,
) -> Option<&'a SceneItemSnapshot> {
    let selected = editor.selected_item_id()?;
    snapshot.items.iter().find(|item| item.id == selected)
}

fn hit_test_item<'a>(
    pointer: egui::Pos2,
    viewport: ViewportTransform,
    editor: &SceneEditorState,
    snapshot: &'a SourcesSnapshot,
) -> Option<&'a SceneItemSnapshot> {
    if !viewport.viewport().contains(pointer) {
        let item = selected_item(editor, snapshot)?;
        return item_contains_pointer(item, pointer, viewport, editor).then_some(item);
    }

    snapshot
        .items
        .iter()
        .find(|item| item_contains_pointer(item, pointer, viewport, editor))
}

fn item_contains_pointer(
    item: &SceneItemSnapshot,
    pointer: egui::Pos2,
    viewport: ViewportTransform,
    editor: &SceneEditorState,
) -> bool {
    item.visible
        && color_settings(item).is_some_and(|settings| {
            let transform = editor.effective_transform(item.id, item.transform);
            viewport
                .canvas_rect_to_screen(item_canvas_rect(item, transform, settings))
                .contains(pointer)
        })
}

fn workspace_overflow_rects(workspace: egui::Rect, viewport: egui::Rect) -> [egui::Rect; 4] {
    [
        egui::Rect::from_min_max(workspace.min, egui::pos2(workspace.right(), viewport.top())),
        egui::Rect::from_min_max(
            egui::pos2(workspace.left(), viewport.bottom()),
            workspace.max,
        ),
        egui::Rect::from_min_max(
            egui::pos2(workspace.left(), viewport.top()),
            egui::pos2(viewport.left(), viewport.bottom()),
        ),
        egui::Rect::from_min_max(
            egui::pos2(viewport.right(), viewport.top()),
            egui::pos2(workspace.right(), viewport.bottom()),
        ),
    ]
}

fn color_settings(item: &SceneItemSnapshot) -> Option<ColorSourceSettings> {
    match item.settings {
        SourceSettings::Color(settings) => Some(settings),
        SourceSettings::None => None,
    }
}

fn item_canvas_rect(
    item: &SceneItemSnapshot,
    transform: Transform,
    settings: ColorSourceSettings,
) -> egui::Rect {
    let uncropped = egui::vec2(settings.size[0], settings.size[1]);
    let cropped = egui::vec2(
        (uncropped.x - item.crop.left - item.crop.right).max(1.0),
        (uncropped.y - item.crop.top - item.crop.bottom).max(1.0),
    );
    let size = egui::vec2(
        cropped.x * transform.scale[0].max(0.001),
        cropped.y * transform.scale[1].max(0.001),
    );
    let anchor = egui::vec2(transform.anchor[0], transform.anchor[1]);
    let position = egui::pos2(transform.position[0], transform.position[1]);
    egui::Rect::from_min_size(position - size * anchor, size)
}

fn transform_from_rect(
    rect: egui::Rect,
    original: Transform,
    settings: ColorSourceSettings,
    item: &SceneItemSnapshot,
) -> Transform {
    let source_width = (settings.size[0] - item.crop.left - item.crop.right).max(1.0);
    let source_height = (settings.size[1] - item.crop.top - item.crop.bottom).max(1.0);
    let anchor = egui::vec2(original.anchor[0], original.anchor[1]);
    let position = rect.min + rect.size() * anchor;
    Transform {
        position: [position.x, position.y],
        scale: [rect.width() / source_width, rect.height() / source_height],
        ..original
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Crop, SceneCanvas, SceneItemId, SourceKind};

    fn color_item() -> SceneItemSnapshot {
        SceneItemSnapshot {
            id: SceneItemId(1),
            name: "Color Source".into(),
            kind: SourceKind::Color,
            settings: SourceSettings::Color(ColorSourceSettings {
                size: [1920.0, 1080.0],
                rgba: [0, 0, 0, 255],
            }),
            visible: true,
            locked: false,
            transform: Transform {
                position: [960.0, 540.0],
                ..Transform::default()
            },
            crop: Crop::default(),
        }
    }

    #[test]
    fn default_color_source_fills_canvas_and_viewport() {
        let item = color_item();
        let settings = color_settings(&item).unwrap();
        let canvas_rect = item_canvas_rect(&item, item.transform, settings);
        assert_eq!(
            canvas_rect,
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1920.0, 1080.0))
        );

        let viewport_rect =
            egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(960.0, 540.0));
        let viewport = ViewportTransform::new(viewport_rect, SceneCanvas::DEFAULT);
        assert_eq!(viewport.canvas_rect_to_screen(canvas_rect), viewport_rect);
    }

    #[test]
    fn dragging_inside_a_color_source_starts_move_interaction() {
        let item = color_item();
        let snapshot = SourcesSnapshot {
            canvas: SceneCanvas::DEFAULT,
            scene_id: None,
            scene_name: None,
            items: vec![item],
        };
        let viewport = ViewportTransform::new(
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(960.0, 540.0)),
            SceneCanvas::DEFAULT,
        );
        let mut editor = SceneEditorState::default();

        begin_drag(egui::pos2(480.0, 270.0), viewport, &mut editor, &snapshot);

        assert_eq!(editor.selected_item_id(), Some(SceneItemId(1)));
        assert!(matches!(
            editor.drag.map(|drag| drag.mode),
            Some(TransformDragMode::Move)
        ));
    }
}
