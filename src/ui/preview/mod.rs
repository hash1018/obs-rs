mod gizmo;
mod state;
mod toolbar;
mod viewport_transform;

use eframe::egui;

use crate::domain::{Crop, SceneCanvas, SourceSettings, Stroke, Transform};
use crate::engine::CompositeFrame;
use crate::i18n::{LocalizationManager, TextKey};
use crate::project::{ProjectCommand, SourceCommand};
use crate::snapshots::{SceneItemSnapshot, SourcesSnapshot};

use super::editor::{ResizeHandle, SceneEditorState, Tool, TransformDrag, TransformDragMode};
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
            paint_hovered_item(ui, workspace_rect, viewport, editor, snapshot);
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
        // Read as the drag begins rather than per frame: letting go of Alt
        // half way through a gesture must not turn a crop into a resize under
        // the pointer.
        let cropping = ui.input(|input| input.modifiers.alt);
        if let Some(drag_origin) = drag_origin {
            begin_drag(drag_origin, viewport, editor, snapshot, cropping);
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
        let (transform, crop) = match drag.mode {
            TransformDragMode::Move => (
                Transform {
                    position: [
                        drag.original.position[0] + delta.x,
                        drag.original.position[1] + delta.y,
                    ],
                    ..drag.original
                },
                drag.crop,
            ),
            TransformDragMode::Resize(handle) => {
                let original_rect = item_canvas_rect(item, drag.original);
                (
                    transform_from_rect(
                        gizmo::resize_rect(original_rect, handle, delta),
                        drag.original,
                        item,
                    ),
                    drag.crop,
                )
            }
            TransformDragMode::Crop(handle) => crop_drag(item, drag, handle, delta),
        };
        if editor.transform_override != Some((drag.item_id, transform))
            || editor.crop_override != Some((drag.item_id, crop))
        {
            actions.push(UiAction::DragSceneItem(drag.item_id, transform, crop));
        }
        editor.transform_override = Some((drag.item_id, transform));
        editor.crop_override = Some((drag.item_id, crop));
        ui.ctx().request_repaint();
    }

    if response.drag_stopped()
        && let Some(drag) = editor.drag.take()
    {
        // A crop drag moves both, so both are recorded — and each only when
        // it actually moved, so a plain drag still writes one row.
        if let Some((item_id, crop)) = editor.crop_override
            && item_id == drag.item_id
            && crop != drag.crop
        {
            actions.push(UiAction::Project(ProjectCommand::Source(
                SourceCommand::SetCrop(item_id, crop),
            )));
        }
        if let Some((item_id, transform)) = editor.transform_override
            && item_id == drag.item_id
            && transform != drag.original
        {
            actions.push(UiAction::Project(ProjectCommand::Source(
                SourceCommand::SetTransform(item_id, transform),
            )));
        }
    }

    // Alt+double-click on the selected item puts a crop back. On a handle it
    // is that edge alone, in the middle of the item it is all four — the same
    // place each gesture starts, so undoing one is where doing it was.
    if response.double_clicked()
        && ui.input(|input| input.modifiers.alt)
        && let Some(pointer) = pointer
        && let Some(item) = selected_item(editor, snapshot)
        && !item.locked
    {
        let crop = editor.effective_crop(item.id, item.crop);
        let reset = match selected_handle_at(pointer, viewport, editor, snapshot) {
            Some((handle, _)) => uncrop_edges(crop, handle),
            None if edited_canvas_rect(item, editor, viewport).contains(pointer) => Crop::default(),
            None => crop,
        };
        if reset != crop {
            actions.push(UiAction::Project(ProjectCommand::Source(
                SourceCommand::SetCrop(item.id, reset),
            )));
            // Straight to the compositor as well, for the same reason the
            // drag goes there: the project's answer arrives a frame or two
            // later, and the picture should not wait for it.
            let transform = editor.effective_transform(item.id, item.transform);
            actions.push(UiAction::DragSceneItem(item.id, transform, reset));
            editor.crop_override = Some((item.id, reset));
        }
        return;
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
    cropping: bool,
) {
    if let Some((handle, item)) = selected_handle_at(drag_origin, viewport, editor, snapshot) {
        if !item.locked {
            let original = editor.effective_transform(item.id, item.transform);
            editor.drag = Some(TransformDrag {
                item_id: item.id,
                original,
                crop: editor.effective_crop(item.id, item.crop),
                // The same handles either way. Alt is what every editor with
                // both gestures uses to tell them apart, and a handle is
                // where both of them start.
                mode: if cropping {
                    TransformDragMode::Crop(handle)
                } else {
                    TransformDragMode::Resize(handle)
                },
            });
        }
    } else if let Some(item) = drag_target(drag_origin, viewport, editor, snapshot) {
        editor.select(item.id);
        if !item.locked {
            let original = editor.effective_transform(item.id, item.transform);
            editor.drag = Some(TransformDrag {
                item_id: item.id,
                original,
                crop: editor.effective_crop(item.id, item.crop),
                mode: TransformDragMode::Move,
            });
        }
    } else {
        editor.clear_selection();
    }
}

/// Where a crop drag has taken the item: what is cut off, and the rectangle
/// that leaves.
///
/// The edge under the pointer moves and the opposite one stays put. Cropping
/// from the left while the whole item slid left would be aiming at a moving
/// target — and it is the *un*dragged edges that a person is lining the
/// picture up against.
///
/// The delta arrives in Canvas units and the crop is in the Source's own
/// pixels, so it is divided by the scale on the way in. What comes back out
/// is a rectangle whose width is `(source - crop) * scale` again, which is
/// why the scale survives the gesture unchanged: both sides shrank by the
/// same amount.
fn crop_drag(
    item: &SceneItemSnapshot,
    drag: TransformDrag,
    handle: ResizeHandle,
    delta: egui::Vec2,
) -> (Transform, Crop) {
    let scale = [
        drag.original.scale[0].max(0.001),
        drag.original.scale[1].max(0.001),
    ];
    let (left, right, top, bottom) = (
        matches!(
            handle,
            ResizeHandle::Left | ResizeHandle::TopLeft | ResizeHandle::BottomLeft
        ),
        matches!(
            handle,
            ResizeHandle::Right | ResizeHandle::TopRight | ResizeHandle::BottomRight
        ),
        matches!(
            handle,
            ResizeHandle::Top | ResizeHandle::TopLeft | ResizeHandle::TopRight
        ),
        matches!(
            handle,
            ResizeHandle::Bottom | ResizeHandle::BottomLeft | ResizeHandle::BottomRight
        ),
    );
    // In source pixels, and only on the edges this handle holds.
    let horizontal = delta.x / scale[0];
    let vertical = delta.y / scale[1];
    let crop = clamp_crop(
        Crop {
            left: drag.crop.left + if left { horizontal } else { 0.0 },
            right: drag.crop.right - if right { horizontal } else { 0.0 },
            top: drag.crop.top + if top { vertical } else { 0.0 },
            bottom: drag.crop.bottom - if bottom { vertical } else { 0.0 },
        },
        item.source_size,
    );

    // What was actually taken, after the clamp — the pointer can ask for more
    // than the picture has, and the rectangle must follow what happened
    // rather than what was asked.
    let original_rect =
        egui::Rect::from(RectOf(item.canvas_rect_cropped(drag.original, drag.crop)));
    let cut = egui::vec2(
        (crop.left - drag.crop.left) * scale[0],
        (crop.top - drag.crop.top) * scale[1],
    );
    let width = (item.source_size[0] - crop.left - crop.right).max(1.0) * scale[0];
    let height = (item.source_size[1] - crop.top - crop.bottom).max(1.0) * scale[1];
    let rect = egui::Rect::from_min_size(original_rect.min + cut, egui::vec2(width, height));

    (
        transform_from_rect_cropped(rect, drag.original, item, crop),
        crop,
    )
}

/// Keeps a crop inside the picture: never negative, and never so deep that
/// nothing is left. One pixel is the floor because a layer of no width is one
/// the compositor would refuse — and because a source you have cropped away
/// entirely is one you meant to hide.
///
/// Shared with the Properties dock, which can be typed into as freely as this
/// can be dragged.
pub(in crate::ui) fn clamp_crop(crop: Crop, source_size: [f32; 2]) -> Crop {
    let clamp = |near: f32, far: f32, extent: f32| {
        let near = near.max(0.0).min((extent - 1.0).max(0.0));
        let far = far.max(0.0).min((extent - near - 1.0).max(0.0));
        (near, far)
    };
    let (left, right) = clamp(crop.left, crop.right, source_size[0]);
    let (top, bottom) = clamp(crop.top, crop.bottom, source_size[1]);
    Crop {
        left,
        top,
        right,
        bottom,
    }
}

/// `[x, y, width, height]` as egui reads it.
struct RectOf([f32; 4]);

impl From<RectOf> for egui::Rect {
    fn from(RectOf([x, y, width, height]): RectOf) -> Self {
        Self::from_min_size(egui::pos2(x, y), egui::vec2(width, height))
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
    let source_rect = edited_canvas_rect(item, editor, viewport);
    let color = overflow_fill(ui, item);

    let hatch = egui::Stroke::new(1.0, ui.visuals().weak_text_color().gamma_multiply(0.9));

    for overflow_rect in workspace_overflow_rects(workspace, viewport.viewport()) {
        if !overflow_rect.is_positive() {
            continue;
        }
        let painter = ui.painter().with_clip_rect(overflow_rect);
        painter.rect_filled(source_rect, 0.0, color);
        // The fill says what the Source is; the hatching says this part of it
        // is not in the output. A colour alone cannot say the second thing —
        // a Color Source is drawn in its own colour on both sides of the
        // Canvas edge, and a neutral grey over a dark Workspace reads as the
        // picture merely continuing. A pattern reads whatever it is over.
        hatch_area(&painter, source_rect.intersect(overflow_rect), hatch);
    }
}

/// Diagonal stripes across `area`, in screen space.
///
/// Placed on a grid the whole Workspace shares rather than measured from the
/// area's own corner, so the stripes stay put as a Source is dragged: a
/// pattern that slides with what it covers reads as motion of the thing
/// rather than as a mark on it.
fn hatch_area(painter: &egui::Painter, area: egui::Rect, stroke: egui::Stroke) {
    /// Far enough apart to read as stripes rather than as a tint, close
    /// enough that a thin sliver of overflow still gets one.
    const SPACING: f32 = 9.0;

    if !area.is_positive() {
        return;
    }
    // Every 45-degree line is `x + y = offset`, so the ones crossing this
    // area are the multiples of SPACING between its nearest and furthest
    // corner sums.
    let first = ((area.min.x + area.min.y) / SPACING).ceil() * SPACING;
    let last = area.max.x + area.max.y;
    let mut offset = first;
    while offset <= last {
        if let Some(segment) = hatch_segment(area, offset) {
            painter.line_segment(segment, stroke);
        }
        offset += SPACING;
    }
}

/// Where the 45-degree line `x + y == offset` enters and leaves `area`, or
/// `None` for one that passes outside the corner it is nearest.
fn hatch_segment(area: egui::Rect, offset: f32) -> Option<[egui::Pos2; 2]> {
    let top = (offset - area.max.x).max(area.min.y);
    let bottom = (offset - area.min.x).min(area.max.y);
    (bottom > top).then(|| {
        [
            egui::pos2(offset - top, top),
            egui::pos2(offset - bottom, bottom),
        ]
    })
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
    let rect = edited_canvas_rect(item, editor, viewport);
    let painter = ui.painter().with_clip_rect(workspace);
    paint_cropped_away(&painter, ui.visuals(), item, editor, viewport);
    paint_alignment_guides(&painter, item, editor, viewport, snapshot.canvas);
    paint_outline(&painter, rect, editor.effective_crop(item.id, item.crop));
    gizmo::paint_handles(&painter, EDITOR_MARK, rect);
}

/// What a click would take, outlined under the pointer.
///
/// A Scene is a stack of rectangles with no edges of their own, and which
/// one a click lands on is a question the picture cannot answer: a capture
/// covering the Canvas and a Colour Source behind it look the same until one
/// of them is selected. This answers it before the click rather than after.
///
/// Not the selected item, which has an outline already, and not during a
/// drag, when what is under the pointer is not what the pointer is doing.
/// Drawn before the selection overlay so that where the two meet, the
/// selection is what is on top.
fn paint_hovered_item(
    ui: &egui::Ui,
    workspace: egui::Rect,
    viewport: ViewportTransform,
    editor: &SceneEditorState,
    snapshot: &SourcesSnapshot,
) {
    if editor.drag.is_some() {
        return;
    }
    let Some(pointer) = ui.ctx().pointer_hover_pos() else {
        return;
    };
    // Inside the picture only. The margin around it belongs to the Workspace,
    // and an item hanging into it is being shown where it reaches rather than
    // offered to be clicked.
    if !viewport.viewport().contains(pointer) {
        return;
    }
    let Some(item) = hit_test_item(pointer, viewport, editor, snapshot) else {
        return;
    };
    if editor.selected_item_id() == Some(item.id) {
        return;
    }
    ui.painter().with_clip_rect(workspace).rect_stroke(
        edited_canvas_rect(item, editor, viewport),
        0.0,
        egui::Stroke::new(HOVER_WIDTH, EDITOR_HOVER_MARK),
        egui::StrokeKind::Outside,
    );
}

/// The selected item's outline, one edge at a time.
///
/// An edge a crop cut is marked instead of outlined: green and dashed, and
/// no red line under it. Both at once would be one thicker line of an
/// in-between colour, and the thing worth seeing is which of the four this
/// is.
///
/// Without it a cropped Source is indistinguishable from a smaller one — the
/// same rectangle either way — and the difference matters as soon as anyone
/// wonders where the rest of the picture went.
fn paint_outline(painter: &egui::Painter, rect: egui::Rect, crop: Crop) {
    let edge = egui::Stroke::new(OUTLINE_WIDTH, EDITOR_MARK);
    let cut_edge = egui::Stroke::new(CROP_EDGE_WIDTH, EDITOR_CROP_MARK);
    for (cut, segment) in cropped_edges(rect, crop) {
        if cut > 0.0 {
            painter.extend(egui::Shape::dashed_line(
                &segment, cut_edge, CROP_DASH, CROP_GAP,
            ));
        } else {
            painter.line_segment(segment, edge);
        }
    }
}

/// How much each edge of `rect` had cut off it, with the line that edge is —
/// top, left, right, bottom.
fn cropped_edges(rect: egui::Rect, crop: Crop) -> [(f32, [egui::Pos2; 2]); 4] {
    [
        (crop.top, [rect.left_top(), rect.right_top()]),
        (crop.left, [rect.left_top(), rect.left_bottom()]),
        (crop.right, [rect.right_top(), rect.right_bottom()]),
        (crop.bottom, [rect.left_bottom(), rect.right_bottom()]),
    ]
}

/// What the editor draws over the picture with.
///
/// Not the theme's selection colour, which everything else selected uses.
/// That colour is chosen to sit on the application's own surfaces, where the
/// background is ours and any legible colour stays legible. These marks sit
/// on the Scene, where what is underneath is whatever the user is capturing
/// — and a blue outline over a window title bar, a code editor or a browser
/// is a blue line on blue. Red is the one hue interfaces almost never use as
/// a background, which is why every editor that draws over video reaches for
/// it.
///
/// Fixed rather than derived, and the same in either theme, because what it
/// has to contrast with is not the theme.
const EDITOR_MARK: egui::Color32 = egui::Color32::from_rgb(0xFF, 0x3B, 0x30);

/// Thinner than the outline the guides measure to, so the two read as what
/// they are: one is the item's edge, the others are distances from it.
const GUIDE_WIDTH: f32 = 2.0;

/// How far a guide's number sits off its line.
///
/// Far enough that the line does not strike through the digits, close enough
/// that it is plainly that line's number and not the next one's.
const LABEL_MARGIN: f32 = 5.0;

/// Larger than the interface's own text, because it is read at a glance
/// while the pointer is somewhere else, and with nothing behind it but the
/// Scene.
const LABEL_SIZE: f32 = 15.0;

/// What the numbers are written in.
///
/// Fixed, like [`EDITOR_MARK`] and for the same reason: the digits sit on
/// the Scene, and the Scene is whatever is being captured rather than
/// anything the theme has a say in. Taking the theme's own text colour
/// wrote them in near-black under a light theme, over captures the rest of
/// this overlay assumes are dark.
const LABEL_MARK: egui::Color32 = egui::Color32::WHITE;

/// Drawn behind the numbers so that white digits survive a white window.
///
/// White on the Scene is legible until the Scene is a browser, a document
/// or a code editor on a light background, at which point the label is not
/// dim — it is gone, and a reading that disappears exactly where somebody
/// is working is worse than one that was never offered.
///
/// The other answer is a plate behind them, which this deliberately is not:
/// a filled box covers the picture the number is being read against, and
/// the number is a distance to an edge of that picture. An outline costs
/// the width of a stroke and hides nothing.
const LABEL_HALO: egui::Color32 = egui::Color32::BLACK;

/// How far out the halo's copies sit, in screen pixels.
const HALO_REACH: f32 = 1.0;

/// How many copies make the halo.
///
/// Eight, spread evenly, so no corner is left open. Four — up, down, left,
/// right — leaves the diagonals bare, and a digit is mostly curves, whose
/// edges face the corners as often as the sides.
const HALO_COPIES: u32 = 8;

/// What a cropped edge is drawn with — see [`paint_outline`].
///
/// A second colour because it answers a second question. The outline says
/// where the item is; this says the picture stops there because it was cut,
/// and the two are worth telling apart at a glance.
const EDITOR_CROP_MARK: egui::Color32 = egui::Color32::from_rgb(0x32, 0xD7, 0x4B);

/// Screen pixels, not Canvas ones: these are marks on the display rather
/// than part of the Scene, so they stay the same width at every zoom.
const OUTLINE_WIDTH: f32 = 3.0;

/// What the item under the pointer is outlined with — see
/// [`paint_hovered_item`].
///
/// Blue against the selection's red, which is the pair every editor that
/// draws over a picture ends up at: two hues that no interface uses as a
/// background, far enough apart to tell without looking twice.
const EDITOR_HOVER_MARK: egui::Color32 = egui::Color32::from_rgb(0x0A, 0x84, 0xFF);

/// Lighter than the selection's, because it is an offer rather than a state.
const HOVER_WIDTH: f32 = 2.0;

/// Heavier than the plain edge, because a dashed line of the same weight
/// reads lighter than a solid one — half of it is not there.
const CROP_EDGE_WIDTH: f32 = 4.0;

/// Long enough to read as a dash at the size an edge usually is, with a gap
/// wide enough that the line never reads as solid at a glance.
const CROP_DASH: f32 = 9.0;
const CROP_GAP: f32 = 9.0;

/// How far the selected item sits from each Canvas edge.
///
/// In Canvas pixels, which is the only unit the answer is useful in: the
/// Workspace is whatever size the window leaves it, so a distance measured on
/// screen would read differently at every zoom for the same Scene. What the
/// numbers are for is placing a Source exactly — centred, or flush to an edge
/// — which is arithmetic the user would otherwise do by eye.
///
/// From the moment it is selected rather than only while it is moving: where
/// a Source sits is worth reading before deciding to move it, and a figure
/// that appears only once the pointer is down cannot be read against the one
/// it had before.
fn paint_alignment_guides(
    painter: &egui::Painter,
    item: &SceneItemSnapshot,
    editor: &SceneEditorState,
    viewport: ViewportTransform,
    canvas: SceneCanvas,
) {
    let transform = editor.effective_transform(item.id, item.transform);
    let crop = editor.effective_crop(item.id, item.crop);
    let rect = egui::Rect::from(RectOf(item.canvas_rect_cropped(transform, crop)));
    let stroke = egui::Stroke::new(GUIDE_WIDTH, EDITOR_MARK);

    // Each guide runs from the edge of the item to the edge of the Canvas,
    // along the middle of the side it measures — which is where there is
    // most room for it and where the eye already is.
    let middle = rect.center();
    let ends = [
        (egui::pos2(middle.x, rect.min.y), egui::pos2(middle.x, 0.0)),
        (egui::pos2(rect.min.x, middle.y), egui::pos2(0.0, middle.y)),
        (
            egui::pos2(rect.max.x, middle.y),
            egui::pos2(canvas.width, middle.y),
        ),
        (
            egui::pos2(middle.x, rect.max.y),
            egui::pos2(middle.x, canvas.height),
        ),
    ];
    for (gap, (from, to)) in edge_gaps(rect, canvas).into_iter().zip(ends) {
        let Some(gap) = guide_label(gap) else {
            continue;
        };
        let from = viewport.canvas_to_screen(from);
        let to = viewport.canvas_to_screen(to);
        painter.line_segment([from, to], stroke);
        // Beside the guide rather than on it. With no plate behind them the
        // digits have only their own shape to be read by, and a rule through
        // the middle of that is what takes it away.
        let (at, align) = if (to.x - from.x).abs() < f32::EPSILON {
            (
                from.lerp(to, 0.5) + egui::vec2(LABEL_MARGIN, 0.0),
                egui::Align2::LEFT_CENTER,
            )
        } else {
            (
                from.lerp(to, 0.5) - egui::vec2(0.0, LABEL_MARGIN),
                egui::Align2::CENTER_BOTTOM,
            )
        };
        let galley = painter.layout_no_wrap(
            format!("{gap} px"),
            egui::FontId::proportional(LABEL_SIZE),
            egui::Color32::PLACEHOLDER,
        );
        let at = align.anchor_size(at, galley.size()).min;
        for step in 0..HALO_COPIES {
            let angle = std::f32::consts::TAU * step as f32 / HALO_COPIES as f32;
            let offset = egui::Vec2::angled(angle) * HALO_REACH;
            painter.galley(at + offset, galley.clone(), LABEL_HALO);
        }
        painter.galley(at, galley, LABEL_MARK);
    }
}

/// What a guide prints for `gap`, or `None` for an edge with nothing to
/// measure.
///
/// A flush edge is the one case where the number says less than the picture
/// already does: the outline and the Canvas edge lie along each other, and
/// "0 px" beside a line of no length only repeats it. A Source filling the
/// Canvas is flush on all four, which is where a Scene usually starts and
/// where this was worst — four figures, none of them an answer to anything.
///
/// Rounded before the test, because the label is rounded too: a gap of two
/// tenths of a pixel reads "0 px" like any other, and dropping the number
/// while still drawing its guide would be the worse half of both.
fn guide_label(gap: f32) -> Option<i32> {
    let gap = gap.round() as i32;
    (gap != 0).then_some(gap)
}

/// The gap from each Canvas edge to `rect` — top, left, right, bottom.
///
/// Signed, so an item past an edge reports how far past rather than zero.
fn edge_gaps(rect: egui::Rect, canvas: SceneCanvas) -> [f32; 4] {
    [
        rect.min.y,
        rect.min.x,
        canvas.width - rect.max.x,
        canvas.height - rect.max.y,
    ]
}

/// The crop with whichever edges this handle holds put back.
fn uncrop_edges(crop: Crop, handle: ResizeHandle) -> Crop {
    let cleared = |held: bool, value: f32| if held { 0.0 } else { value };
    Crop {
        left: cleared(
            matches!(
                handle,
                ResizeHandle::Left | ResizeHandle::TopLeft | ResizeHandle::BottomLeft
            ),
            crop.left,
        ),
        right: cleared(
            matches!(
                handle,
                ResizeHandle::Right | ResizeHandle::TopRight | ResizeHandle::BottomRight
            ),
            crop.right,
        ),
        top: cleared(
            matches!(
                handle,
                ResizeHandle::Top | ResizeHandle::TopLeft | ResizeHandle::TopRight
            ),
            crop.top,
        ),
        bottom: cleared(
            matches!(
                handle,
                ResizeHandle::Bottom | ResizeHandle::BottomLeft | ResizeHandle::BottomRight
            ),
            crop.bottom,
        ),
    }
}

/// What a crop is leaving out, while it is being dragged.
///
/// Faint, and only during the gesture: a crop with nothing showing behind it
/// is a cut you cannot judge — you can see how much is gone but not what, and
/// the edge you are lining up is exactly what has just been hidden. It goes
/// as soon as the pointer comes up, because after that the crop *is* the
/// picture.
fn paint_cropped_away(
    painter: &egui::Painter,
    visuals: &egui::Visuals,
    item: &SceneItemSnapshot,
    editor: &SceneEditorState,
    viewport: ViewportTransform,
) {
    let Some(drag) = editor.drag else {
        return;
    };
    if !matches!(drag.mode, TransformDragMode::Crop(_)) || drag.item_id != item.id {
        return;
    }
    let transform = editor.effective_transform(item.id, item.transform);
    let crop = editor.effective_crop(item.id, item.crop);
    let [visible_x, visible_y, _, _] = item.canvas_rect_cropped(transform, crop);
    let [scale_x, scale_y] = [transform.scale[0].max(0.001), transform.scale[1].max(0.001)];
    // The whole picture where it would be if nothing were cut: the visible
    // rectangle, pushed back out by what each edge took.
    let whole = viewport.canvas_rect_to_screen(egui::Rect::from_min_size(
        egui::pos2(
            visible_x - crop.left * scale_x,
            visible_y - crop.top * scale_y,
        ),
        egui::vec2(item.source_size[0] * scale_x, item.source_size[1] * scale_y),
    ));
    painter.rect_filled(whole, 0.0, visuals.extreme_bg_color.gamma_multiply(0.35));
    painter.rect_stroke(
        whole,
        0.0,
        egui::Stroke::new(1.0, visuals.weak_text_color()),
        egui::StrokeKind::Inside,
    );
}

fn selected_handle_at<'a>(
    pointer: egui::Pos2,
    viewport: ViewportTransform,
    editor: &SceneEditorState,
    snapshot: &'a SourcesSnapshot,
) -> Option<(super::editor::ResizeHandle, &'a SceneItemSnapshot)> {
    let item = selected_item(editor, snapshot)?;
    let rect = edited_canvas_rect(item, editor, viewport);
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
    item.visible && edited_canvas_rect(item, editor, viewport).contains(pointer)
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
        | SourceSettings::Rtsp(_)
        | SourceSettings::VideoCapture(_)
        | SourceSettings::Image(_) => ui.visuals().widgets.inactive.bg_fill,
    }
    .gamma_multiply(0.65)
}

fn item_canvas_rect(item: &SceneItemSnapshot, transform: Transform) -> egui::Rect {
    egui::Rect::from(RectOf(item.canvas_rect(transform)))
}

/// Where the item is *right now*, gesture included: the transform being
/// dragged and the crop being dragged, either of which may be ahead of what
/// the project holds.
///
/// Everything the editor draws and hit-tests goes through this, so a gizmo
/// follows a crop as closely as it follows a resize.
fn edited_canvas_rect(
    item: &SceneItemSnapshot,
    editor: &SceneEditorState,
    viewport: ViewportTransform,
) -> egui::Rect {
    let transform = editor.effective_transform(item.id, item.transform);
    let crop = editor.effective_crop(item.id, item.crop);
    viewport.canvas_rect_to_screen(egui::Rect::from(RectOf(
        item.canvas_rect_cropped(transform, crop),
    )))
}

fn transform_from_rect(
    rect: egui::Rect,
    original: Transform,
    item: &SceneItemSnapshot,
) -> Transform {
    transform_from_rect_cropped(rect, original, item, item.crop)
}

/// The same, for a crop the item does not have yet — see
/// [`SceneItemSnapshot::canvas_rect_cropped`].
fn transform_from_rect_cropped(
    rect: egui::Rect,
    original: Transform,
    item: &SceneItemSnapshot,
    crop: Crop,
) -> Transform {
    let source_width = (item.source_size[0] - crop.left - crop.right).max(1.0);
    let source_height = (item.source_size[1] - crop.top - crop.bottom).max(1.0);
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
            peak_db: None,
            position: None,
        }
    }

    /// Each cropped edge is marked on the side it was cut from. Getting this
    /// wrong is drawing the mark on the opposite edge, which looks right
    /// until someone crops one side.
    #[test]
    fn a_cropped_edge_is_marked_on_the_side_it_was_cut_from() {
        let rect = egui::Rect::from_min_max(egui::pos2(10.0, 20.0), egui::pos2(110.0, 70.0));
        let crop = Crop {
            left: 0.0,
            top: 8.0,
            right: 4.0,
            bottom: 0.0,
        };

        let marked: Vec<[egui::Pos2; 2]> = cropped_edges(rect, crop)
            .into_iter()
            .filter(|(cut, _)| *cut > 0.0)
            .map(|(_, segment)| segment)
            .collect();

        assert_eq!(
            marked,
            vec![
                [rect.left_top(), rect.right_top()],
                [rect.right_top(), rect.right_bottom()],
            ],
            "the top and right were cut, so the top and right are marked"
        );
    }

    /// The four numbers are gaps to the Canvas edges, so they and the item
    /// account for the whole of it — which is what makes them addable, and
    /// how a Source gets centred by making two of them equal.
    #[test]
    fn the_alignment_gaps_and_the_item_span_the_canvas() {
        let canvas = SceneCanvas {
            width: 1920.0,
            height: 1080.0,
        };
        let rect = egui::Rect::from_min_size(egui::pos2(1106.0, 427.0), egui::vec2(633.0, 357.0));

        let [top, left, right, bottom] = edge_gaps(rect, canvas);

        assert_eq!(
            [top, left],
            [427.0, 1106.0],
            "measured from the Canvas edge"
        );
        assert_eq!(left + rect.width() + right, canvas.width);
        assert_eq!(top + rect.height() + bottom, canvas.height);
    }

    /// An item hanging off the edge has a negative gap rather than a clamped
    /// zero: how far past it went is the thing worth knowing, and zero would
    /// say it was flush.
    #[test]
    fn an_item_outside_the_canvas_has_a_negative_gap() {
        let canvas = SceneCanvas {
            width: 1920.0,
            height: 1080.0,
        };
        let rect = egui::Rect::from_min_size(egui::pos2(-40.0, 0.0), egui::vec2(200.0, 100.0));

        let [_, left, right, _] = edge_gaps(rect, canvas);

        assert_eq!(left, -40.0);
        assert_eq!(right, 1760.0, "still measured from the far edge");
    }

    /// Flush edges are left unlabelled, and a Source filling the Canvas is
    /// flush on all four. What hangs off the Canvas still reports, because
    /// how far past it went is the one thing the picture cannot say.
    #[test]
    fn an_edge_that_would_read_zero_is_not_labelled() {
        assert_eq!(guide_label(0.0), None);
        assert_eq!(guide_label(0.4), None, "this one would have printed 0 px");
        assert_eq!(guide_label(-0.4), None);
        assert_eq!(guide_label(1.0), Some(1));
        assert_eq!(
            guide_label(-370.0),
            Some(-370),
            "past the edge still counts"
        );
    }

    /// A stripe crossing the area has both ends on its border, at 45
    /// degrees, and nothing outside it — the whole of what makes the
    /// hatching a hatching rather than a scribble over the Workspace.
    #[test]
    fn a_hatch_stripe_is_a_diagonal_across_the_area_and_stays_inside_it() {
        let area = egui::Rect::from_min_max(egui::pos2(10.0, 20.0), egui::pos2(60.0, 50.0));

        // Through the middle: its two corner sums bracket every line that
        // crosses at all, so this one has to.
        let offset = (area.min.x + area.min.y + area.max.x + area.max.y) / 2.0;
        let [enter, leave] = hatch_segment(area, offset).expect("a line through the middle");

        for point in [enter, leave] {
            assert!(
                area.contains(point),
                "{point:?} is outside {area:?}, which the clip would hide \
                 rather than the geometry preventing"
            );
            assert!(
                (point.x + point.y - offset).abs() < 0.001,
                "{point:?} is not on the line x + y = {offset}"
            );
        }
        assert!(
            ((leave.x - enter.x).abs() - (leave.y - enter.y).abs()).abs() < 0.001,
            "a 45-degree stripe covers as much x as y: {enter:?} to {leave:?}"
        );
    }

    /// The lines the loop would draw past either corner have nothing of the
    /// area in them, and a stripe of zero length is a dot the pattern does
    /// not want.
    #[test]
    fn a_hatch_stripe_outside_the_area_is_not_drawn() {
        let area = egui::Rect::from_min_max(egui::pos2(10.0, 20.0), egui::pos2(60.0, 50.0));

        assert!(
            hatch_segment(area, area.min.x + area.min.y - 1.0).is_none(),
            "before the near corner"
        );
        assert!(
            hatch_segment(area, area.max.x + area.max.y + 1.0).is_none(),
            "past the far corner"
        );
        assert!(
            hatch_segment(area, area.min.x + area.min.y).is_none(),
            "exactly on the near corner is a point, not a stripe"
        );
    }

    /// The whole of a crop drag: the edge under the pointer moves, the
    /// opposite one stays where it was, and the scale comes out unchanged —
    /// which is what makes a crop a crop rather than a resize.
    #[test]
    fn cropping_moves_the_dragged_edge_and_leaves_the_others_alone() {
        let item = color_item();
        let drag = TransformDrag {
            item_id: item.id,
            original: item.transform,
            crop: Crop::default(),
            mode: TransformDragMode::Crop(ResizeHandle::Left),
        };
        let before = egui::Rect::from(RectOf(item.canvas_rect(item.transform)));

        // A hundred Canvas units in, at scale 1.
        let (transform, crop) = crop_drag(&item, drag, ResizeHandle::Left, egui::vec2(100.0, 0.0));

        assert_eq!(crop.left, 100.0);
        assert_eq!((crop.right, crop.top, crop.bottom), (0.0, 0.0, 0.0));
        assert_eq!(
            transform.scale, item.transform.scale,
            "a crop takes pixels away, it does not resize what is left"
        );
        let after = egui::Rect::from(RectOf(item.canvas_rect_cropped(transform, crop)));
        assert!(
            (after.right() - before.right()).abs() < 0.01,
            "the edge that was not dragged must not move: {} against {}",
            after.right(),
            before.right()
        );
        assert!(
            (after.left() - (before.left() + 100.0)).abs() < 0.01,
            "the dragged edge follows the pointer"
        );
    }

    /// A crop cannot eat the whole picture, and cannot go negative — the
    /// pointer is free to ask for both.
    #[test]
    fn a_crop_is_held_inside_the_picture() {
        let item = color_item();
        let drag = TransformDrag {
            item_id: item.id,
            original: item.transform,
            crop: Crop::default(),
            mode: TransformDragMode::Crop(ResizeHandle::Left),
        };

        let (_, past_the_end) =
            crop_drag(&item, drag, ResizeHandle::Left, egui::vec2(9_000.0, 0.0));
        assert_eq!(past_the_end.left, item.source_size[0] - 1.0);

        let (_, backwards) = crop_drag(&item, drag, ResizeHandle::Left, egui::vec2(-500.0, 0.0));
        assert_eq!(
            backwards.left, 0.0,
            "dragging a crop outwards past the edge uncrops it and stops"
        );
    }

    /// The reset gesture: a handle puts back the edges it holds, and nothing
    /// else.
    #[test]
    fn uncropping_a_corner_clears_only_its_own_two_edges() {
        let crop = Crop {
            left: 10.0,
            top: 20.0,
            right: 30.0,
            bottom: 40.0,
        };

        let corner = uncrop_edges(crop, ResizeHandle::TopLeft);

        assert_eq!((corner.left, corner.top), (0.0, 0.0));
        assert_eq!(
            (corner.right, corner.bottom),
            (30.0, 40.0),
            "the edges the handle does not hold are left as they were"
        );
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
            peak_db: None,
            position: None,
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

        begin_drag(
            egui::pos2(480.0, 270.0),
            viewport,
            &mut editor,
            &snapshot,
            false,
        );

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

        begin_drag(
            egui::pos2(480.0, 270.0),
            viewport,
            &mut editor,
            &snapshot,
            false,
        );

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
