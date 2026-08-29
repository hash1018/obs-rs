use std::collections::HashMap;

use eframe::egui;

use super::PANEL_MARGIN;
use super::layout::{DockLayout, DockPanel, DockRegionId, REGIONS};
use crate::i18n::{LocalizationManager, TextKey};
use crate::snapshots::Snapshots;
use crate::ui::editor::SceneEditorState;
use crate::ui::{UiAction, UiResources, panels};
use panels::scenes::ScenesPanelState;
use panels::sources::SourcesPanelState;

const SIDE_DEFAULT_SIZE: f32 = 260.0;
const SIDE_MIN_SIZE: f32 = 180.0;
const SIDE_MAX_SIZE: f32 = 520.0;
const BOTTOM_DEFAULT_SIZE: f32 = 240.0;
const BOTTOM_MIN_SIZE: f32 = 140.0;
const BOTTOM_MAX_SIZE: f32 = 460.0;
const SPLITTER_SIZE: f32 = 6.0;
const DROP_ZONE_FRACTION: f32 = 0.25;
const DRAG_START_DISTANCE: f32 = 4.0;
const TITLE_BAR_HEIGHT: f32 = 24.0;
const INSERTION_MARKER_SIZE: f32 = 4.0;

#[derive(Clone, Copy)]
struct DockMove {
    panel: DockPanel,
    region: DockRegionId,
    index: usize,
}

#[derive(Clone, Copy)]
struct DropTarget {
    region: DockRegionId,
    index: usize,
    indicator: DropIndicator,
}

struct RegionOutput {
    panel_drags: Vec<(DockPanel, egui::Response)>,
    rect: egui::Rect,
}

struct DockContent<'a> {
    scenes_state: &'a mut ScenesPanelState,
    sources_state: &'a mut SourcesPanelState,
    editor: &'a mut SceneEditorState,
    snapshots: &'a Snapshots,
    i18n: &'a LocalizationManager,
    actions: &'a mut Vec<UiAction>,
}

#[derive(Clone, Copy)]
enum DropIndicator {
    Area(egui::Rect),
    Insertion(egui::Rect),
}

pub(super) fn show(
    ui: &mut egui::Ui,
    layout: &mut DockLayout,
    scenes_state: &mut ScenesPanelState,
    sources_state: &mut SourcesPanelState,
    editor: &mut SceneEditorState,
    resources: &UiResources<'_>,
    actions: &mut Vec<UiAction>,
) {
    let workspace = ui.available_rect_before_wrap();
    let mut panel_drags = Vec::new();
    let mut region_rects = HashMap::new();
    let mut pending_move = None;
    let mut content = DockContent {
        scenes_state,
        sources_state,
        editor,
        snapshots: resources.snapshots,
        i18n: resources.i18n,
        actions,
    };

    for region in REGIONS {
        if layout.visible_panels(region).is_empty() {
            continue;
        }

        let response = region_panel(region, layout)
            .show(ui, |ui| show_region(ui, layout, region, &mut content));
        region_rects.insert(region, response.inner.rect);
        panel_drags.extend(response.inner.panel_drags);
    }

    for (panel, response) in panel_drags {
        pending_move = pending_move.or(handle_panel_drag(
            ui,
            layout,
            panel,
            response,
            workspace,
            &region_rects,
        ));
    }

    if let Some(dock_move) = pending_move {
        layout.move_panel(dock_move.panel, dock_move.region, dock_move.index);
        ui.ctx().request_repaint();
    }
}

fn region_panel(region: DockRegionId, layout: &DockLayout) -> egui::Panel {
    let panel = match region {
        DockRegionId::Left => egui::Panel::left("left_dock_region"),
        DockRegionId::Right => egui::Panel::right("right_dock_region"),
        DockRegionId::Bottom => egui::Panel::bottom("bottom_dock_region"),
    };
    let panel_min_size = layout
        .visible_panels(region)
        .into_iter()
        .map(|panel| match region {
            DockRegionId::Left | DockRegionId::Right => panel.min_size().x,
            DockRegionId::Bottom => panel.min_size().y,
        })
        .fold(0.0, f32::max);
    let (default_size, configured_min, max_size) = match region {
        DockRegionId::Left | DockRegionId::Right => {
            (SIDE_DEFAULT_SIZE, SIDE_MIN_SIZE, SIDE_MAX_SIZE)
        }
        DockRegionId::Bottom => (BOTTOM_DEFAULT_SIZE, BOTTOM_MIN_SIZE, BOTTOM_MAX_SIZE),
    };
    let min_size = configured_min.max(panel_min_size);

    panel
        .default_size(default_size)
        .min_size(min_size)
        .max_size(max_size.max(min_size))
        .resizable(true)
        .frame(egui::Frame::new())
}

fn show_region(
    ui: &mut egui::Ui,
    layout: &mut DockLayout,
    region: DockRegionId,
    content: &mut DockContent<'_>,
) -> RegionOutput {
    let panels = layout.visible_panels(region);
    let weights = layout.normalized_weights(region, &panels);
    let region_rect = ui.available_rect_before_wrap();
    let axis_length = axis_length(region, region_rect);
    let usable_length =
        (axis_length - SPLITTER_SIZE * panels.len().saturating_sub(1) as f32).max(1.0);
    let mut axis_cursor = axis_min(region, region_rect);
    let mut panel_drags = Vec::with_capacity(panels.len());

    let _ = ui.allocate_rect(region_rect, egui::Sense::hover());

    for (index, panel) in panels.iter().copied().enumerate() {
        let pane_length = if index + 1 == panels.len() {
            axis_max(region, region_rect) - axis_cursor
        } else {
            usable_length * weights[index]
        };
        let pane_rect = rect_along_axis(region, region_rect, axis_cursor, pane_length);
        let title_response = show_panel(ui, pane_rect, panel, content);
        panel_drags.push((panel, title_response));
        axis_cursor += pane_length;

        if let Some(next_panel) = panels.get(index + 1).copied() {
            let splitter_rect = rect_along_axis(region, region_rect, axis_cursor, SPLITTER_SIZE);
            show_splitter(
                ui,
                layout,
                region,
                panel,
                next_panel,
                splitter_rect,
                usable_length,
            );
            axis_cursor += SPLITTER_SIZE;
        }
    }

    RegionOutput {
        panel_drags,
        rect: region_rect,
    }
}

fn show_panel(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    panel: DockPanel,
    content: &mut DockContent<'_>,
) -> egui::Response {
    ui.painter().rect_filled(rect, 0.0, ui.visuals().panel_fill);
    ui.painter().rect_stroke(
        rect,
        0.0,
        ui.visuals().widgets.noninteractive.bg_stroke,
        egui::StrokeKind::Inside,
    );

    // Every edge but the bottom: the toolbar is anchored there and provides
    // its own gap. Insetting the bottom as well left the strip floating a
    // margin above the dock's edge, which reads as buttons pushed upwards.
    let content_rect = egui::Rect::from_min_max(
        rect.min + egui::vec2(PANEL_MARGIN, PANEL_MARGIN),
        egui::pos2(rect.max.x - PANEL_MARGIN, rect.max.y),
    );
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .id(egui::Id::new(("dock_panel_content", panel)))
            .max_rect(content_rect)
            .layout(egui::Layout::top_down(egui::Align::LEFT)),
    );
    child.set_clip_rect(rect);

    let (title_rect, title_response) = child.allocate_exact_size(
        egui::vec2(child.available_width(), TITLE_BAR_HEIGHT),
        egui::Sense::drag(),
    );
    child.painter().text(
        title_rect.left_center(),
        egui::Align2::LEFT_CENTER,
        content.i18n.text(match panel {
            DockPanel::Scenes => TextKey::DockScenes,
            DockPanel::Sources => TextKey::DockSources,
            DockPanel::Controls => TextKey::DockControls,
        }),
        egui::TextStyle::Heading.resolve(child.style()),
        child.visuals().strong_text_color(),
    );
    child.separator();

    match panel {
        DockPanel::Scenes => {
            panels::scenes::show(
                &mut child,
                content.scenes_state,
                &content.snapshots.scenes,
                content.i18n,
                content.actions,
            );
        }
        DockPanel::Sources => {
            panels::sources::show(
                &mut child,
                content.sources_state,
                content.editor,
                &content.snapshots.sources,
                content.i18n,
                content.actions,
            );
        }
        DockPanel::Controls => {
            panels::controls::show(
                &mut child,
                &content.snapshots.status,
                content.i18n,
                content.actions,
            );
        }
    }

    title_response
}

fn show_splitter(
    ui: &mut egui::Ui,
    layout: &mut DockLayout,
    region: DockRegionId,
    first: DockPanel,
    second: DockPanel,
    rect: egui::Rect,
    usable_length: f32,
) {
    let cursor = match region {
        DockRegionId::Left | DockRegionId::Right => egui::CursorIcon::ResizeVertical,
        DockRegionId::Bottom => egui::CursorIcon::ResizeHorizontal,
    };
    let response = ui
        .interact(
            rect,
            egui::Id::new(("dock_splitter", region, first, second)),
            egui::Sense::drag(),
        )
        .on_hover_cursor(cursor);

    if response.hovered() || response.dragged() {
        let center = match region {
            DockRegionId::Left | DockRegionId::Right => [
                egui::pos2(rect.left(), rect.center().y),
                egui::pos2(rect.right(), rect.center().y),
            ],
            DockRegionId::Bottom => [
                egui::pos2(rect.center().x, rect.top()),
                egui::pos2(rect.center().x, rect.bottom()),
            ],
        };
        ui.painter().line_segment(
            center,
            egui::Stroke::new(1.0, ui.visuals().selection.bg_fill),
        );
    }

    if response.dragged() {
        let delta = match region {
            DockRegionId::Left | DockRegionId::Right => response.drag_delta().y,
            DockRegionId::Bottom => response.drag_delta().x,
        };
        layout.resize_pair(
            region,
            first,
            second,
            delta / usable_length,
            pane_axis_min_size(region, first) / usable_length,
            pane_axis_min_size(region, second) / usable_length,
        );
        ui.ctx().request_repaint();
    }
}

fn pane_axis_min_size(region: DockRegionId, panel: DockPanel) -> f32 {
    match region {
        DockRegionId::Left | DockRegionId::Right => panel.min_size().y,
        DockRegionId::Bottom => panel.min_size().x,
    }
}

fn handle_panel_drag(
    ui: &egui::Ui,
    layout: &mut DockLayout,
    panel: DockPanel,
    response: egui::Response,
    workspace: egui::Rect,
    region_rects: &HashMap<DockRegionId, egui::Rect>,
) -> Option<DockMove> {
    let total_drag_distance = response
        .total_drag_delta()
        .map_or(0.0, |delta| delta.length());
    if response.dragged() && total_drag_distance >= DRAG_START_DISTANCE {
        layout.state_mut(panel).drag_active = true;
    }

    let pointer = ui.ctx().input(|input| input.pointer.latest_pos());
    let target =
        pointer.and_then(|pointer| drop_target(layout, workspace, region_rects, pointer, panel));
    if layout.state(panel).drag_active
        && response.dragged()
        && let Some(target) = target
    {
        paint_drop_target(ui, target.indicator);
    }

    if !response.drag_stopped() {
        return None;
    }

    let was_active = layout.state(panel).drag_active;
    layout.state_mut(panel).drag_active = false;
    if was_active {
        target.map(|target| DockMove {
            panel,
            region: target.region,
            index: target.index,
        })
    } else {
        None
    }
}

fn drop_target(
    layout: &DockLayout,
    workspace: egui::Rect,
    region_rects: &HashMap<DockRegionId, egui::Rect>,
    pointer: egui::Pos2,
    dragged_panel: DockPanel,
) -> Option<DropTarget> {
    if !workspace.contains(pointer) {
        return None;
    }

    let region = region_rects
        .iter()
        .find_map(|(region, rect)| rect.contains(pointer).then_some(*region))
        .or_else(|| empty_region_at_pointer(region_rects, workspace, pointer))?;

    let target_rect = region_rects
        .get(&region)
        .copied()
        .unwrap_or_else(|| region_drop_rect(workspace, region));
    let target_count = layout
        .visible_panels(region)
        .into_iter()
        .filter(|panel| *panel != dragged_panel)
        .count();
    let slot_count = target_count + 1;
    let relative = match region {
        DockRegionId::Left | DockRegionId::Right => {
            (pointer.y - target_rect.top()) / target_rect.height()
        }
        DockRegionId::Bottom => (pointer.x - target_rect.left()) / target_rect.width(),
    };
    let index = ((relative.clamp(0.0, 0.999_999) * slot_count as f32) as usize).min(target_count);

    if !layout.move_changes_layout(dragged_panel, region, index) {
        return None;
    }

    let indicator = if layout.panel_region(dragged_panel) == Some(region) {
        DropIndicator::Insertion(insertion_rect(target_rect, region, index, target_count))
    } else {
        DropIndicator::Area(slot_rect(target_rect, region, index, slot_count))
    };

    Some(DropTarget {
        region,
        index,
        indicator,
    })
}

fn empty_region_at_pointer(
    region_rects: &HashMap<DockRegionId, egui::Rect>,
    workspace: egui::Rect,
    pointer: egui::Pos2,
) -> Option<DockRegionId> {
    if !region_rects.contains_key(&DockRegionId::Bottom)
        && pointer.y >= workspace.bottom() - workspace.height() * DROP_ZONE_FRACTION
    {
        Some(DockRegionId::Bottom)
    } else if !region_rects.contains_key(&DockRegionId::Left)
        && pointer.x <= workspace.left() + workspace.width() * DROP_ZONE_FRACTION
    {
        Some(DockRegionId::Left)
    } else if !region_rects.contains_key(&DockRegionId::Right)
        && pointer.x >= workspace.right() - workspace.width() * DROP_ZONE_FRACTION
    {
        Some(DockRegionId::Right)
    } else {
        None
    }
}

fn region_drop_rect(workspace: egui::Rect, region: DockRegionId) -> egui::Rect {
    match region {
        DockRegionId::Left => egui::Rect::from_min_max(
            workspace.min,
            egui::pos2(
                workspace.left() + workspace.width() * DROP_ZONE_FRACTION,
                workspace.bottom(),
            ),
        ),
        DockRegionId::Right => egui::Rect::from_min_max(
            egui::pos2(
                workspace.right() - workspace.width() * DROP_ZONE_FRACTION,
                workspace.top(),
            ),
            workspace.max,
        ),
        DockRegionId::Bottom => egui::Rect::from_min_max(
            egui::pos2(
                workspace.left(),
                workspace.bottom() - workspace.height() * DROP_ZONE_FRACTION,
            ),
            workspace.max,
        ),
    }
}

fn slot_rect(rect: egui::Rect, region: DockRegionId, index: usize, count: usize) -> egui::Rect {
    match region {
        DockRegionId::Left | DockRegionId::Right => {
            let height = rect.height() / count as f32;
            egui::Rect::from_min_max(
                egui::pos2(rect.left(), rect.top() + height * index as f32),
                egui::pos2(rect.right(), rect.top() + height * (index + 1) as f32),
            )
        }
        DockRegionId::Bottom => {
            let width = rect.width() / count as f32;
            egui::Rect::from_min_max(
                egui::pos2(rect.left() + width * index as f32, rect.top()),
                egui::pos2(rect.left() + width * (index + 1) as f32, rect.bottom()),
            )
        }
    }
}

fn insertion_rect(
    rect: egui::Rect,
    region: DockRegionId,
    index: usize,
    remaining_panels: usize,
) -> egui::Rect {
    let fraction = index as f32 / remaining_panels.max(1) as f32;
    match region {
        DockRegionId::Left | DockRegionId::Right => {
            let y = egui::lerp(rect.top()..=rect.bottom(), fraction);
            egui::Rect::from_min_max(
                egui::pos2(rect.left(), y - INSERTION_MARKER_SIZE * 0.5),
                egui::pos2(rect.right(), y + INSERTION_MARKER_SIZE * 0.5),
            )
        }
        DockRegionId::Bottom => {
            let x = egui::lerp(rect.left()..=rect.right(), fraction);
            egui::Rect::from_min_max(
                egui::pos2(x - INSERTION_MARKER_SIZE * 0.5, rect.top()),
                egui::pos2(x + INSERTION_MARKER_SIZE * 0.5, rect.bottom()),
            )
        }
    }
}

fn paint_drop_target(ui: &egui::Ui, indicator: DropIndicator) {
    let color = ui.visuals().selection.bg_fill;
    let painter = ui.ctx().layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("dock_drop_target"),
    ));
    match indicator {
        DropIndicator::Area(rect) => {
            painter.rect_filled(rect, 4.0, color.gamma_multiply(0.3));
            painter.rect_stroke(
                rect,
                4.0,
                egui::Stroke::new(2.0, color),
                egui::StrokeKind::Inside,
            );
        }
        DropIndicator::Insertion(rect) => {
            painter.rect_filled(rect, 2.0, color);
        }
    }
}

fn axis_length(region: DockRegionId, rect: egui::Rect) -> f32 {
    match region {
        DockRegionId::Left | DockRegionId::Right => rect.height(),
        DockRegionId::Bottom => rect.width(),
    }
}

fn axis_min(region: DockRegionId, rect: egui::Rect) -> f32 {
    match region {
        DockRegionId::Left | DockRegionId::Right => rect.top(),
        DockRegionId::Bottom => rect.left(),
    }
}

fn axis_max(region: DockRegionId, rect: egui::Rect) -> f32 {
    match region {
        DockRegionId::Left | DockRegionId::Right => rect.bottom(),
        DockRegionId::Bottom => rect.right(),
    }
}

fn rect_along_axis(
    region: DockRegionId,
    bounds: egui::Rect,
    start: f32,
    length: f32,
) -> egui::Rect {
    match region {
        DockRegionId::Left | DockRegionId::Right => egui::Rect::from_min_max(
            egui::pos2(bounds.left(), start),
            egui::pos2(bounds.right(), start + length),
        ),
        DockRegionId::Bottom => egui::Rect::from_min_max(
            egui::pos2(start, bounds.top()),
            egui::pos2(start + length, bounds.bottom()),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_rects_split_vertical_and_horizontal_regions() {
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(300.0, 200.0));

        assert_eq!(
            slot_rect(rect, DockRegionId::Left, 1, 2),
            egui::Rect::from_min_max(egui::pos2(0.0, 100.0), egui::pos2(300.0, 200.0))
        );
        assert_eq!(
            slot_rect(rect, DockRegionId::Bottom, 1, 3),
            egui::Rect::from_min_max(egui::pos2(100.0, 0.0), egui::pos2(200.0, 200.0))
        );
    }
}
