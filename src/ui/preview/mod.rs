mod gizmo;
mod state;
mod toolbar;
mod viewport_transform;

use eframe::egui;

use crate::domain::{SourceSettings, Stroke, Transform};
use crate::engine::CompositeFrame;
use crate::i18n::{LocalizationManager, TextKey};
use crate::project::{ProjectCommand, SourceCommand};
use crate::snapshots::{SceneItemSnapshot, SourcesSnapshot};

use super::editor::{SceneEditorState, Tool, TransformDrag, TransformDragMode};
use super::{UiAction, UiResources};
use viewport_transform::{ViewportTransform, fit_aspect_ratio};

const PREVIEW_MARGIN: i8 = 18;
pub(super) use state::PreviewViewState;
pub use state::PreviewZoom;

pub(super) fn show(
    ui: &mut egui::Ui,
    view_state: &mut PreviewViewState,
    editor: &mut SceneEditorState,
    resources: &UiResources<'_>,
    actions: &mut Vec<UiAction>,
) {
    let snapshot = &resources.snapshots.sources;
    let i18n = resources.i18n;
    egui::CentralPanel::default()
        .frame(
            egui::Frame::central_panel(ui.style())
                .fill(ui.visuals().faint_bg_color)
                .inner_margin(egui::Margin::same(PREVIEW_MARGIN)),
        )
        .show(ui, |ui| {
            let available_rect = ui.available_rect_before_wrap();
            // A Drawing brings its own tools, and only while it is the thing
            // selected: they are what says "this is what you are editing", and
            // the toolbar goes back to its resting width the moment it is not.
            //
            // Asked before anything is placed because the band under the
            // picture is sized from it — a toolbar that has to scroll is a
            // scrollbar taller than one that does not.
            let drawing = editor.selected_item_id().and_then(|item_id| {
                snapshot
                    .items
                    .iter()
                    .find(|item| item.id == item_id)
                    .and_then(|item| match &item.settings {
                        SourceSettings::Drawing(drawing) => Some((item_id, drawing.strokes.len())),
                        _ => None,
                    })
            });
            // Solid rather than egui's default floating bar. A floating one
            // allocates no space and is two pixels wide until it is hovered,
            // which is no answer at all to a button you cannot reach — the
            // whole point here is that the bar is visible enough to grab.
            let scroll_style = egui::style::ScrollStyle::solid();
            let band = toolbar_band(
                available_rect.width(),
                drawing.is_some(),
                scroll_style.allocated_width(),
            );
            let toolbar_height = band.height;

            // How large the picture may get. The toolbar's band is taken off
            // the bottom here so that a picture scaled to fill still leaves
            // room for it — this rect answers *how big*, and nothing else.
            let sizing_bounds = egui::vec2(
                available_rect.width(),
                available_rect.height() - toolbar_height - toolbar::TOOLBAR_GAP,
            );
            let viewport_bounds = sizing_bounds * view_state.scale();
            let viewport_size = fit_aspect_ratio(viewport_bounds, snapshot.canvas.aspect_ratio());
            // The picture and the toolbar under it are centred as one block,
            // not the picture alone. Centring the picture in the band-less
            // half and then drawing the toolbar against it left the reserved
            // band stranded at the very bottom: the gap above the picture was
            // always a toolbar shorter than the gap below, by exactly the
            // amount taken off above and never given back.
            //
            // The toolbar stays attached to the picture rather than pinned to
            // the bottom edge, because it belongs to the picture — it is what
            // that picture is zoomed to — and one pinned there reads as the
            // panel's own furniture instead.
            let block_height = viewport_size.y + toolbar::TOOLBAR_GAP + toolbar_height;
            let block_top = available_rect.center().y - block_height / 2.0;
            let viewport_rect = egui::Rect::from_min_size(
                egui::pos2(available_rect.center().x - viewport_size.x / 2.0, block_top),
                viewport_size,
            );
            // The whole panel is the workspace: what the editor takes clicks
            // in, and what a selected source's overflow is drawn against. Not
            // the sizing rect above — that one stops a toolbar short of the
            // bottom edge, so a selection spilling past the picture was
            // clipped higher below than above while the picture itself sat
            // centred. The band the toolbar occupies is drawn over by the
            // toolbar itself, which is painted last.
            let workspace_rect = available_rect;
            let viewport = ViewportTransform::new(viewport_rect, snapshot.canvas);
            let response = ui.interact(
                workspace_rect,
                egui::Id::new("preview_workspace_interaction"),
                egui::Sense::click_and_drag(),
            );

            handle_pointer(ui, &response, viewport, editor, snapshot, actions);
            paint_editor_overflow(ui, workspace_rect, viewport, editor, snapshot);
            paint_composite_frame(ui, viewport, resources.composite_frame, i18n);
            paint_editor_overlay(ui, workspace_rect, viewport, editor, snapshot);

            // Kept inside the panel, and shifted left rather than allowed to
            // run past its edge — the picture is centred, so a narrow window
            // leaves less room to the right of the viewport than the toolbar
            // wants even when the panel itself has enough.
            let toolbar_left = viewport_rect
                .left()
                .min(available_rect.right() - band.width)
                .max(available_rect.left());
            let toolbar_rect = egui::Rect::from_min_size(
                egui::pos2(toolbar_left, viewport_rect.bottom() + toolbar::TOOLBAR_GAP),
                egui::vec2(band.width, band.height),
            );
            let mut toolbar_ui = ui.new_child(
                egui::UiBuilder::new()
                    .id_salt("preview_toolbar")
                    .max_rect(toolbar_rect)
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
            );
            toolbar_ui.set_clip_rect(toolbar_rect);
            toolbar_ui.spacing_mut().scroll = scroll_style;
            // Scrolled only when it has to be. A bar under a toolbar that
            // already fits is furniture, and the band it is drawn in is 26
            // pixels tall — there is no room to spend on nothing.
            egui::ScrollArea::horizontal()
                .id_salt("preview_toolbar_scroll")
                .auto_shrink([false, false])
                .scroll_bar_visibility(if band.scrolls {
                    egui::scroll_area::ScrollBarVisibility::AlwaysVisible
                } else {
                    egui::scroll_area::ScrollBarVisibility::AlwaysHidden
                })
                .show(&mut toolbar_ui, |ui| {
                    // Told how wide it is. Inside a scroll area the inner ui
                    // is handed the viewport's width, so a toolbar laying
                    // itself out against that reports content exactly as wide
                    // as the space it had — and an area whose content fits has
                    // nothing to scroll and draws no bar.
                    ui.set_min_width(band.wanted);
                    toolbar::show(ui, view_state, i18n);
                    if let Some((item_id, strokes)) = drawing {
                        toolbar::show_pen(ui, &mut editor.pen, item_id, strokes, i18n, actions);
                    }
                });
        });
}

/// The band under the picture that the toolbar is drawn in.
struct ToolbarBand {
    /// What the toolbar would like, which is what the contents are laid out
    /// to inside the scroll area — an area whose content is only as wide as
    /// its viewport has nothing to scroll.
    wanted: f32,
    /// What it gets, which is never more than the panel has.
    width: f32,
    /// [`toolbar::TOOLBAR_HEIGHT`], plus room for a scrollbar when there is
    /// one.
    height: f32,
    scrolls: bool,
}

/// How wide and tall the toolbar's band is, given the panel it must fit in.
///
/// The panel is the limit. A toolbar allowed its full width in a window too
/// narrow for it ran off the side and took its last buttons with it — undo
/// and clear are drawn last, so those were the ones that could not be
/// reached.
///
/// `scrollbar` is what a bar costs in this style, asked of egui rather than
/// assumed: its default bar floats and allocates nothing, so a band sized for
/// that would have no room for the solid one this draws.
fn toolbar_band(available: f32, has_pen: bool, scrollbar: f32) -> ToolbarBand {
    let wanted = if has_pen {
        toolbar::TOOLBAR_WIDTH + toolbar::PEN_TOOLBAR_WIDTH
    } else {
        toolbar::TOOLBAR_WIDTH
    };
    let width = wanted.min(available);
    let scrolls = width < wanted;
    ToolbarBand {
        wanted,
        width,
        height: toolbar::TOOLBAR_HEIGHT + if scrolls { scrollbar } else { 0.0 },
        scrolls,
    }
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

    if response.clicked() || response.drag_started() {
        response.request_focus();
    }

    // Before anything else: with a pen or an eraser in hand the pointer draws,
    // and none of the selecting, dragging or resizing below should see it.
    if editor.pen.tool != Tool::Select
        && handle_drawing(ui, response, viewport, editor, snapshot, actions)
    {
        return;
    }

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
                let original_rect = item_canvas_rect(item, drag.original);
                transform_from_rect(
                    gizmo::resize_rect(original_rect, handle, delta),
                    drag.original,
                    item,
                )
            }
        };
        if editor.transform_override != Some((drag.item_id, transform)) {
            actions.push(UiAction::DragSceneItem(drag.item_id, transform));
        }
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

    if response.has_focus()
        && ui.input(|input| input.key_pressed(egui::Key::Delete))
        && let Some(item_id) = editor.selected_item_id()
    {
        actions.push(UiAction::Project(ProjectCommand::Source(
            SourceCommand::Delete(item_id),
        )));
        editor.clear_selection();
    }
}

/// The pointer while a Drawing is being drawn on, rather than moved.
///
/// Returns whether it took the gesture — it declines when the selected item
/// is not a Drawing after all, which leaves the ordinary editor behaviour in
/// place rather than swallowing the click.
fn handle_drawing(
    ui: &egui::Ui,
    response: &egui::Response,
    viewport: ViewportTransform,
    editor: &mut SceneEditorState,
    snapshot: &SourcesSnapshot,
    actions: &mut Vec<UiAction>,
) -> bool {
    let Some(item_id) = editor.selected_item_id() else {
        return false;
    };
    let Some(item) = snapshot.items.iter().find(|item| item.id == item_id) else {
        return false;
    };
    let SourceSettings::Drawing(drawing) = &item.settings else {
        return false;
    };
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
    }
    // Where the pointer is on the Drawing itself, which is what a stroke is
    // recorded in — see `SceneItemSnapshot::canvas_point_to_source`.
    let transform = editor.effective_transform(item_id, item.transform);
    let at = |point: egui::Pos2| {
        let canvas = viewport.screen_to_canvas(point);
        item.canvas_point_to_source(transform, [canvas.x, canvas.y])
    };

    match editor.pen.tool {
        Tool::Select => false,
        Tool::Pen | Tool::Highlighter => {
            if response.drag_started() || response.clicked() {
                editor.pen.stroke = Some(Vec::new());
            }
            if let Some(point) = response.interact_pointer_pos().and_then(at)
                && let Some(stroke) = editor.pen.stroke.as_mut()
                // A pointer that has not moved a whole unit adds nothing but
                // points to rasterize.
                && stroke.last().is_none_or(|last| {
                    (last[0] - point[0]).abs() >= 1.0 || (last[1] - point[1]).abs() >= 1.0
                })
            {
                stroke.push(point);
                let mut strokes = drawing.strokes.clone();
                strokes.push(Stroke {
                    points: stroke.clone(),
                    rgba: editor.pen.rgba(),
                    width: editor.pen.width(),
                });
                actions.push(UiAction::DrawStrokes(item_id, strokes));
                ui.ctx().request_repaint();
            }
            if (response.drag_stopped() || response.clicked())
                && let Some(points) = editor.pen.stroke.take()
                && !points.is_empty()
            {
                actions.push(UiAction::Project(ProjectCommand::Source(
                    SourceCommand::AddStroke(
                        item_id,
                        Stroke {
                            points,
                            rgba: editor.pen.rgba(),
                            width: editor.pen.width(),
                        },
                    ),
                )));
            }
            true
        }
        Tool::Eraser => {
            if (response.dragged() || response.clicked())
                && let Some(point) = response.interact_pointer_pos().and_then(at)
            {
                let hit: Vec<usize> = drawing
                    .strokes
                    .iter()
                    .enumerate()
                    .filter(|(_, stroke)| touches(stroke, point, editor.pen.width()))
                    .map(|(index, _)| index)
                    .collect();
                if !hit.is_empty() {
                    let kept: Vec<Stroke> = drawing
                        .strokes
                        .iter()
                        .enumerate()
                        .filter(|(index, _)| !hit.contains(index))
                        .map(|(_, stroke)| stroke.clone())
                        .collect();
                    // Shown at once and recorded at once: an eraser has no
                    // gesture to wait out the way a stroke does, and each
                    // sweep that finds something is its own undo step.
                    actions.push(UiAction::DrawStrokes(item_id, kept));
                    actions.push(UiAction::Project(ProjectCommand::Source(
                        SourceCommand::RemoveStrokes(item_id, hit),
                    )));
                }
            }
            true
        }
    }
}

/// Whether the eraser at `point` is over any part of `stroke`.
///
/// Distance to each segment rather than to its endpoints: a long stroke drawn
/// in one sweep has very few points in it, and testing only those would let
/// an eraser pass straight through the middle of a line.
fn touches(stroke: &Stroke, point: [f32; 2], reach: f32) -> bool {
    let limit = (stroke.width / 2.0 + reach).powi(2);
    let distance_squared = |a: [f32; 2], b: [f32; 2]| (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2);
    match stroke.points.as_slice() {
        [] => false,
        [only] => distance_squared(*only, point) <= limit,
        points => points.windows(2).any(|pair| {
            let (start, end) = (pair[0], pair[1]);
            let span = distance_squared(start, end);
            if span == 0.0 {
                return distance_squared(start, point) <= limit;
            }
            let along = (((point[0] - start[0]) * (end[0] - start[0])
                + (point[1] - start[1]) * (end[1] - start[1]))
                / span)
                .clamp(0.0, 1.0);
            let nearest = [
                start[0] + (end[0] - start[0]) * along,
                start[1] + (end[1] - start[1]) * along,
            ];
            distance_squared(nearest, point) <= limit
        }),
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
    } else if let Some(item) = drag_target(drag_origin, viewport, editor, snapshot) {
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

/// Draws the Composite Frame, or says there is none yet.
///
/// The frame fills the Viewport exactly: both are the Scene Canvas, one in
/// canvas pixels and one on screen, so this is the single place where the
/// engine's output and the editor's coordinate space meet.
fn paint_composite_frame(
    ui: &egui::Ui,
    viewport: ViewportTransform,
    frame: Option<&CompositeFrame>,
    i18n: &LocalizationManager,
) {
    let painter = ui.painter().with_clip_rect(viewport.viewport());
    painter.rect_filled(viewport.viewport(), 0.0, egui::Color32::BLACK);
    if let Some(frame) = frame {
        painter.image(
            frame.texture_id,
            viewport.viewport(),
            egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );
    } else {
        painter.text(
            viewport.viewport().center(),
            egui::Align2::CENTER_CENTER,
            i18n.text(TextKey::PreviewNoFrame),
            egui::FontId::proportional(14.0),
            egui::Color32::from_gray(132),
        );
    }
    painter.rect_stroke(
        viewport.viewport(),
        0.0,
        egui::Stroke::new(1.0, egui::Color32::from_gray(64)),
        egui::StrokeKind::Inside,
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
    let transform = editor.effective_transform(item.id, item.transform);
    let source_rect = viewport.canvas_rect_to_screen(item_canvas_rect(item, transform));
    let color = overflow_fill(ui, item);

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
    let transform = editor.effective_transform(item.id, item.transform);
    let rect = viewport.canvas_rect_to_screen(item_canvas_rect(item, transform));
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
    let transform = editor.effective_transform(item.id, item.transform);
    let rect = viewport.canvas_rect_to_screen(item_canvas_rect(item, transform));
    gizmo::hit_test(rect, pointer).map(|handle| (handle, item))
}

fn selected_item<'a>(
    editor: &SceneEditorState,
    snapshot: &'a SourcesSnapshot,
) -> Option<&'a SceneItemSnapshot> {
    let selected = editor.selected_item_id()?;
    snapshot.items.iter().find(|item| item.id == selected)
}

/// What a press should act on, which is not always what a click would select.
///
/// The selected item keeps the pointer while it is under it, whatever is drawn
/// in front. Without that, a source behind another cannot be moved where they
/// overlap: the press meant to drag it picks up the one on top instead, and
/// the only part of it you can grab is the part that is not covered.
///
/// Only for a press. A plain click still takes the topmost, because that is
/// the only way back to the item in front — and a selection that swallowed
/// every click inside it would leave a canvas-sized Drawing impossible to
/// select past.
fn drag_target<'a>(
    pointer: egui::Pos2,
    viewport: ViewportTransform,
    editor: &SceneEditorState,
    snapshot: &'a SourcesSnapshot,
) -> Option<&'a SceneItemSnapshot> {
    if let Some(item) = selected_item(editor, snapshot)
        && item_contains_pointer(item, pointer, viewport, editor)
    {
        return Some(item);
    }
    hit_test_item(pointer, viewport, editor, snapshot)
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
    let transform = editor.effective_transform(item.id, item.transform);
    item.visible
        && viewport
            .canvas_rect_to_screen(item_canvas_rect(item, transform))
            .contains(pointer)
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

/// What a SceneItem's off-Canvas overflow is drawn with in the Workspace margin.
///
/// A Color Source is its own colour. Nothing else has pixels yet, so it gets a
/// neutral placeholder: the margin is editor-only and never reaches output, and
/// showing the item's extent there is what keeps it draggable off-Canvas.
fn overflow_fill(ui: &egui::Ui, item: &SceneItemSnapshot) -> egui::Color32 {
    match &item.settings {
        SourceSettings::Color(settings) => egui::Color32::from_rgba_unmultiplied(
            settings.rgba[0],
            settings.rgba[1],
            settings.rgba[2],
            settings.rgba[3],
        ),
        // A Drawing is transparent wherever nothing was drawn, so the
        // placeholder is the same neutral one an unopened capture gets.
        SourceSettings::Drawing(_)
        | SourceSettings::DisplayCapture(_)
        | SourceSettings::WindowCapture(_)
        | SourceSettings::MediaFile(_)
        | SourceSettings::None => ui.visuals().widgets.inactive.bg_fill,
    }
    .gamma_multiply(0.65)
}

fn item_canvas_rect(item: &SceneItemSnapshot, transform: Transform) -> egui::Rect {
    let [x, y, width, height] = item.canvas_rect(transform);
    egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(width, height))
}

fn transform_from_rect(
    rect: egui::Rect,
    original: Transform,
    item: &SceneItemSnapshot,
) -> Transform {
    let source_width = (item.source_size[0] - item.crop.left - item.crop.right).max(1.0);
    let source_height = (item.source_size[1] - item.crop.top - item.crop.bottom).max(1.0);
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
    use crate::domain::{
        ColorSourceSettings, Crop, DisplayCaptureSettings, DisplayCaptureTarget, SceneCanvas,
        SceneItemId, SourceKind,
    };

    fn color_item() -> SceneItemSnapshot {
        SceneItemSnapshot {
            id: SceneItemId(1),
            name: "Color Source".into(),
            kind: SourceKind::Color,
            settings: SourceSettings::Color(ColorSourceSettings {
                size: [1920.0, 1080.0],
                rgba: [0, 0, 0, 255],
            }),
            source_size: [1920.0, 1080.0],
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
        let canvas_rect = item_canvas_rect(&item, item.transform);
        assert_eq!(
            canvas_rect,
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1920.0, 1080.0))
        );

        let viewport_rect =
            egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(960.0, 540.0));
        let viewport = ViewportTransform::new(viewport_rect, SceneCanvas::DEFAULT);
        assert_eq!(viewport.canvas_rect_to_screen(canvas_rect), viewport_rect);
    }

    fn display_capture_item() -> SceneItemSnapshot {
        SceneItemSnapshot {
            id: SceneItemId(2),
            name: "Display Capture".into(),
            kind: SourceKind::DisplayCapture,
            settings: SourceSettings::DisplayCapture(DisplayCaptureSettings {
                target: DisplayCaptureTarget::MonitorName("DP-1".into()),
                size_hint: None,
            }),
            // What `SourceSettings::source_size` stands in with when no picker
            // reported a size.
            source_size: [1920.0, 1080.0],
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
    fn a_source_without_pixels_is_still_selectable_and_draggable() {
        // The editor works on the SceneItem's rectangle, not on the Source's
        // content, so a Source that cannot yet produce a frame must behave in
        // Preview exactly like one that can.
        let snapshot = SourcesSnapshot {
            items: vec![display_capture_item()],
            ..SourcesSnapshot::default()
        };
        let viewport = ViewportTransform::new(
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(960.0, 540.0)),
            SceneCanvas::DEFAULT,
        );
        let mut editor = SceneEditorState::default();

        begin_drag(egui::pos2(480.0, 270.0), viewport, &mut editor, &snapshot);

        assert_eq!(editor.selected_item_id(), Some(SceneItemId(2)));
        assert!(matches!(
            editor.drag.map(|drag| drag.mode),
            Some(TransformDragMode::Move)
        ));

        // And its corner handles are hit-testable, which is what makes it
        // resizable rather than only movable.
        assert!(
            selected_handle_at(egui::pos2(0.0, 0.0), viewport, &editor, &snapshot).is_some(),
            "the top-left resize handle should be reachable"
        );
    }

    #[test]
    fn dragging_inside_a_color_source_starts_move_interaction() {
        let item = color_item();
        let snapshot = SourcesSnapshot {
            items: vec![item],
            ..SourcesSnapshot::default()
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

    /// The bug this exists for: a toolbar wider than the panel used to keep
    /// its full width and hang off the side, taking undo and clear with it.
    #[test]
    fn a_toolbar_too_wide_for_the_panel_is_cut_to_it_and_scrolls() {
        let scrollbar = 10.0;
        let wanted = toolbar::TOOLBAR_WIDTH + toolbar::PEN_TOOLBAR_WIDTH;

        let squeezed = toolbar_band(wanted - 200.0, true, scrollbar);

        assert_eq!(
            squeezed.width,
            wanted - 200.0,
            "it must take the panel's width, not its own"
        );
        assert_eq!(squeezed.wanted, wanted, "and still lay out to its own");
        assert!(squeezed.scrolls);
        assert_eq!(
            squeezed.height,
            toolbar::TOOLBAR_HEIGHT + scrollbar,
            "the band grows by the bar, or the bar is drawn outside the clip"
        );
    }

    /// And costs nothing when it fits, which is the usual case: a bar under a
    /// toolbar with nothing to scroll is furniture, and the band it would be
    /// drawn in is 26 pixels tall.
    #[test]
    fn a_toolbar_that_fits_keeps_its_width_and_grows_no_band() {
        let roomy = toolbar_band(4000.0, true, 10.0);

        assert_eq!(roomy.width, roomy.wanted);
        assert!(!roomy.scrolls);
        assert_eq!(roomy.height, toolbar::TOOLBAR_HEIGHT);
    }

    /// Without a Drawing selected the pen half is not there to make room for.
    #[test]
    fn the_resting_toolbar_is_the_narrow_one() {
        let resting = toolbar_band(4000.0, false, 10.0);

        assert_eq!(resting.wanted, toolbar::TOOLBAR_WIDTH);
        assert!(!resting.scrolls);
    }

    /// Two items on the same spot: a small one in front, a canvas-sized one
    /// behind it.
    fn overlapping() -> (SourcesSnapshot, ViewportTransform) {
        let behind = color_item();
        let front = SceneItemSnapshot {
            id: SceneItemId(9),
            transform: Transform {
                position: [960.0, 540.0],
                scale: [0.25, 0.25],
                ..Transform::default()
            },
            ..color_item()
        };
        let snapshot = SourcesSnapshot {
            // Front-most first, which is the order the dock shows and the
            // order the hit test walks.
            items: vec![front, behind],
            ..SourcesSnapshot::default()
        };
        let viewport = ViewportTransform::new(
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1920.0, 1080.0)),
            SceneCanvas::DEFAULT,
        );
        (snapshot, viewport)
    }

    /// A press keeps hold of what is selected, so the item behind can still be
    /// dragged where the one in front covers it.
    #[test]
    fn a_press_on_the_overlap_drags_the_selected_item_not_the_one_in_front() {
        let (snapshot, viewport) = overlapping();
        let mut editor = SceneEditorState::default();
        editor.select(SceneItemId(1));

        let target = drag_target(egui::pos2(960.0, 540.0), viewport, &editor, &snapshot)
            .expect("both are here");

        assert_eq!(
            target.id,
            SceneItemId(1),
            "the selected item behind must keep the press, or it cannot be \
             moved where something covers it"
        );
    }

    /// And a click still reaches the one in front, which is the only way back
    /// to it — a selection that swallowed every click inside itself would
    /// make a canvas-sized Drawing impossible to select past.
    #[test]
    fn a_click_on_the_overlap_still_takes_the_topmost() {
        let (snapshot, viewport) = overlapping();
        let mut editor = SceneEditorState::default();
        editor.select(SceneItemId(1));

        let clicked = hit_test_item(egui::pos2(960.0, 540.0), viewport, &editor, &snapshot)
            .expect("both are here");

        assert_eq!(clicked.id, SceneItemId(9));
    }

    /// With nothing selected a press behaves as it always did.
    #[test]
    fn a_press_with_no_selection_takes_the_topmost() {
        let (snapshot, viewport) = overlapping();
        let editor = SceneEditorState::default();

        let target = drag_target(egui::pos2(960.0, 540.0), viewport, &editor, &snapshot)
            .expect("both are here");

        assert_eq!(target.id, SceneItemId(9));
    }

    /// Pressing away from the selection picks up whatever is actually there.
    #[test]
    fn a_press_away_from_the_selection_picks_what_is_under_it() {
        let (snapshot, viewport) = overlapping();
        let mut editor = SceneEditorState::default();
        editor.select(SceneItemId(9));

        // Well outside the quarter-scale item in front, still inside the one
        // behind.
        let target = drag_target(egui::pos2(100.0, 100.0), viewport, &editor, &snapshot)
            .expect("the big one is here");

        assert_eq!(target.id, SceneItemId(1));
    }
}
