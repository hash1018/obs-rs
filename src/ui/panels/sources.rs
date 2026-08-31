use std::collections::HashSet;

use eframe::egui;

use crate::capture::{MonitorTarget, SourcePicker, WindowTarget};
use crate::domain::{
    DisplayCaptureSettings, DisplayCaptureTarget, SceneId, SceneItemId, SourceKind,
    WindowCaptureSettings, WindowCaptureTarget,
};
use crate::i18n::{LocalizationManager, TextKey};
use crate::project::{ProjectCommand, SourceCommand};
use crate::snapshots::{SceneItemSnapshot, SourcesSnapshot};
use crate::ui::UiAction;
use crate::ui::editor::SceneEditorState;

use super::elide;
use super::toolbar::{self, ToolIcon};

const SOURCE_ROW_HEIGHT: f32 = 28.0;
const ICON_WIDTH: f32 = 22.0;
const LIST_ROW_HEIGHT: f32 = 26.0;
const SOURCE_KIND_LIST_HEIGHT: f32 = 122.0;
const DISPLAY_LIST_HEIGHT: f32 = 200.0;

#[derive(Default)]
pub(in crate::ui) struct SourcesPanelState {
    scene_id: Option<SceneId>,
    known_item_count: usize,
    add_dialog_open: bool,
    add_kind: AddSourceKind,
    display_dialog_open: bool,
    display_targets: Vec<MonitorTarget>,
    selected_monitor_name: Option<String>,
    window_dialog_open: bool,
    window_targets: Vec<WindowTarget>,
    /// The platform handle of the picked row, which is the only thing in the
    /// list certain to be unique: two windows can share a title, a process,
    /// or both.
    selected_window: Option<isize>,
    select_new_item: bool,
}

#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum AddSourceKind {
    DisplayCapture,
    WindowCapture,
    #[default]
    Color,
    Drawing,
}

/// The gap either side of the disconnected badge.
const BADGE_PADDING: f32 = 6.0;

pub(in crate::ui) fn show(
    ui: &mut egui::Ui,
    state: &mut SourcesPanelState,
    editor: &mut SceneEditorState,
    snapshot: &SourcesSnapshot,
    disconnected: Option<&HashSet<SceneItemId>>,
    i18n: &LocalizationManager,
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

    // Taken before the strip is shown, so the list gets a `Ui` that cannot
    // reach the buttons — see `toolbar::reserve_list`.
    let mut list = toolbar::reserve_list(ui, "sources_list_area");
    show_toolbar(ui, state, editor, snapshot, i18n, actions);
    let ui = &mut list;

    if snapshot.items.is_empty() {
        let fallback_name = i18n.text(TextKey::SourceSelectedScene);
        let scene_name = snapshot.scene_name.as_deref().unwrap_or(&fallback_name);
        let mut args = fluent_bundle::FluentArgs::new();
        args.set("scene", scene_name);
        ui.centered_and_justified(|ui| {
            ui.weak(i18n.text_with(TextKey::SourceEmpty, &args));
        });
    } else {
        toolbar::scroll_content(ui, "sources_list", |ui| {
            for item in &snapshot.items {
                let disconnected = disconnected.is_some_and(|items| items.contains(&item.id));
                show_source_row(ui, editor, item, disconnected, i18n, actions);
            }
        });
    }

    show_add_dialog(ui.ctx(), state, snapshot, i18n, actions);
    show_display_dialog(ui.ctx(), state, snapshot, i18n, actions);
    show_window_dialog(ui.ctx(), state, snapshot, i18n, actions);
}

fn show_source_row(
    ui: &mut egui::Ui,
    editor: &mut SceneEditorState,
    item: &SceneItemSnapshot,
    disconnected: bool,
    i18n: &LocalizationManager,
    actions: &mut Vec<UiAction>,
) {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), SOURCE_ROW_HEIGHT),
        egui::Sense::click(),
    );
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
    // Interacted with after the row itself so these take the click when the
    // pointer is over them, leaving the rest of the row to select the item.
    let eye = icon_hit(ui, rect, 0, ("visible", item.id.0));
    let lock = icon_hit(ui, rect, 1, ("locked", item.id.0));
    paint_visibility(ui.painter(), eye.rect.center(), item.visible, color);
    paint_lock(ui.painter(), lock.rect.center(), item.locked, color);
    let left = rect.left() + ICON_WIDTH * 2.0 + 4.0;
    // Painted before the name, because it is what the name has to fit
    // alongside: a source that is drawing nothing has to say so even where
    // its name is long enough to fill the row on its own.
    let badge = disconnected.then(|| show_disconnected_badge(ui, rect, i18n));
    let name_right = badge
        .as_ref()
        .map_or(rect.right(), |badge: &egui::Response| {
            badge.rect.left() - BADGE_PADDING
        });
    let elided = elide::paint_one_row(
        ui,
        egui::pos2(left, rect.center().y),
        (name_right - left).max(0.0),
        &item.name,
        color,
    );

    // The kind is what this row does not otherwise say. A name that had to be
    // cut is added above it rather than instead of it: one hover should not
    // have to choose between telling you what the source is and telling you
    // what it is called.
    let kind = i18n.text(source_kind_key(item.kind));
    let response = if elided {
        response.on_hover_text(format!(
            "{}
{kind}",
            item.name
        ))
    } else {
        response.on_hover_text(kind)
    };

    if badge.is_some_and(|badge| badge.clicked()) {
        actions.push(UiAction::ReopenSource(item.id));
    } else if eye.clicked() {
        actions.push(source_action(SourceCommand::SetVisible(
            item.id,
            !item.visible,
        )));
    } else if lock.clicked() {
        actions.push(source_action(SourceCommand::SetLocked(
            item.id,
            !item.locked,
        )));
    } else if response.clicked() {
        editor.select(item.id);
    }
}

/// Says at the end of a row that this Source is producing nothing, and offers
/// to open it again.
///
/// A word rather than an icon: what it says is not guessable from a glyph,
/// and the row has the width for it once the name gives way. Clickable
/// because on Linux reopening is the user's to ask for — the portal picker is
/// a dialog, so nothing may raise it on its own.
fn show_disconnected_badge(
    ui: &egui::Ui,
    row: egui::Rect,
    i18n: &LocalizationManager,
) -> egui::Response {
    let text = i18n.text(TextKey::SourceDisconnected);
    let galley = elide::one_row(ui, &text, row.width(), &egui::TextStyle::Small);
    let size = galley.size();
    let rect = egui::Rect::from_min_size(
        egui::pos2(
            row.right() - BADGE_PADDING - size.x,
            row.center().y - size.y / 2.0,
        ),
        size,
    );
    let response = ui
        .interact(
            rect.expand2(egui::vec2(BADGE_PADDING / 2.0, 2.0)),
            ui.id().with(("disconnected", row.top().to_bits())),
            egui::Sense::click(),
        )
        .on_hover_text(i18n.text(TextKey::SourceReopen));
    let color = if response.hovered() {
        ui.visuals().warn_fg_color
    } else {
        ui.visuals().warn_fg_color.gamma_multiply(0.8)
    };
    ui.painter().galley(rect.min, galley, color);
    response
}

/// The clickable area of one row icon, in the slot at `column`.
fn icon_hit(
    ui: &egui::Ui,
    row: egui::Rect,
    column: usize,
    id: (&'static str, i64),
) -> egui::Response {
    let center = egui::pos2(
        row.left() + ICON_WIDTH * (column as f32 + 0.5),
        row.center().y,
    );
    let rect = egui::Rect::from_center_size(center, egui::vec2(ICON_WIDTH, row.height()));
    ui.interact(rect, ui.id().with(id), egui::Sense::click())
}

fn source_action(command: SourceCommand) -> UiAction {
    UiAction::Project(ProjectCommand::Source(command))
}

fn source_kind_key(kind: SourceKind) -> TextKey {
    match kind {
        SourceKind::DisplayCapture => TextKey::SourceKindDisplayCapture,
        SourceKind::WindowCapture => TextKey::SourceKindWindowCapture,
        SourceKind::VideoCapture => TextKey::SourceKindVideoCapture,
        SourceKind::Image => TextKey::SourceKindImage,
        SourceKind::Color => TextKey::SourceKindColor,
        SourceKind::Drawing => TextKey::SourceKindDrawing,
    }
}

fn show_toolbar(
    ui: &mut egui::Ui,
    state: &mut SourcesPanelState,
    editor: &mut SceneEditorState,
    snapshot: &SourcesSnapshot,
    i18n: &LocalizationManager,
    actions: &mut Vec<UiAction>,
) {
    toolbar::strip(ui, "sources_toolbar", |ui| {
        let selected = editor.selected_item_id();
        let index = selected.and_then(|id| snapshot.items.iter().position(|item| item.id == id));

        if toolbar::button(
            ui,
            ToolIcon::Add,
            i18n.text(TextKey::SourceAdd),
            snapshot.scene_id.is_some(),
        )
        .clicked()
        {
            state.add_dialog_open = true;
        }
        if toolbar::button(
            ui,
            ToolIcon::Remove,
            i18n.text(TextKey::SourceRemove),
            selected.is_some(),
        )
        .clicked()
            && let Some(item_id) = selected
        {
            actions.push(source_action(SourceCommand::Delete(item_id)));
            editor.clear_selection();
        }
        // The dock lists front-most first, so "up" moves an item in
        // front of its neighbour.
        if toolbar::button(
            ui,
            ToolIcon::MoveUp,
            i18n.text(TextKey::SourceMoveUp),
            index.is_some_and(|index| index > 0),
        )
        .clicked()
            && let Some(item_id) = selected
        {
            actions.push(source_action(SourceCommand::MoveUp(item_id)));
        }
        if toolbar::button(
            ui,
            ToolIcon::MoveDown,
            i18n.text(TextKey::SourceMoveDown),
            index.is_some_and(|index| index + 1 < snapshot.items.len()),
        )
        .clicked()
            && let Some(item_id) = selected
        {
            actions.push(source_action(SourceCommand::MoveDown(item_id)));
        }
    });
}

fn show_add_dialog(
    ctx: &egui::Context,
    state: &mut SourcesPanelState,
    snapshot: &SourcesSnapshot,
    i18n: &LocalizationManager,
    actions: &mut Vec<UiAction>,
) {
    if !state.add_dialog_open {
        return;
    }

    let mut open = true;
    let mut add_requested = false;
    let mut cancel = false;
    egui::Window::new(i18n.text(TextKey::SourceAddTitle))
        .id(egui::Id::new("add_source_dialog"))
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .collapsible(false)
        .resizable(false)
        .open(&mut open)
        .show(ctx, |ui| {
            ui.set_min_width(280.0);
            ui.label(i18n.text(TextKey::SourceType));
            ui.add_space(4.0);
            show_list_view(ui, SOURCE_KIND_LIST_HEIGHT, |ui| {
                let display_label = i18n.text(TextKey::SourceKindDisplayCapture);
                let response = list_row(
                    ui,
                    &display_label,
                    state.add_kind == AddSourceKind::DisplayCapture,
                );
                if response.clicked() {
                    state.add_kind = AddSourceKind::DisplayCapture;
                }
                if response.double_clicked() {
                    add_requested = true;
                }

                let window_label = i18n.text(TextKey::SourceKindWindowCapture);
                let response = list_row(
                    ui,
                    &window_label,
                    state.add_kind == AddSourceKind::WindowCapture,
                );
                if response.clicked() {
                    state.add_kind = AddSourceKind::WindowCapture;
                }
                if response.double_clicked() {
                    add_requested = true;
                }

                let color_label = i18n.text(TextKey::SourceKindColor);
                let response = list_row(ui, &color_label, state.add_kind == AddSourceKind::Color);
                if response.clicked() {
                    state.add_kind = AddSourceKind::Color;
                }
                if response.double_clicked() {
                    add_requested = true;
                }

                let drawing_label = i18n.text(TextKey::SourceKindDrawing);
                let response =
                    list_row(ui, &drawing_label, state.add_kind == AddSourceKind::Drawing);
                if response.clicked() {
                    state.add_kind = AddSourceKind::Drawing;
                }
                if response.double_clicked() {
                    add_requested = true;
                }
            });
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui.button(i18n.text(TextKey::ActionAdd)).clicked() {
                    add_requested = true;
                }
                if ui.button(i18n.text(TextKey::ActionCancel)).clicked() {
                    cancel = true;
                }
            });
        });

    if cancel {
        open = false;
    } else if add_requested {
        match state.add_kind {
            AddSourceKind::Color => {
                if let Some(scene_id) = snapshot.scene_id {
                    actions.push(UiAction::Project(ProjectCommand::Source(
                        SourceCommand::AddColor(scene_id),
                    )));
                    state.select_new_item = true;
                }
            }
            AddSourceKind::Drawing => {
                if let Some(scene_id) = snapshot.scene_id {
                    actions.push(UiAction::Project(ProjectCommand::Source(
                        SourceCommand::AddDrawing(scene_id),
                    )));
                    state.select_new_item = true;
                }
            }
            AddSourceKind::DisplayCapture => {
                prepare_display_picker(state, snapshot.scene_id, actions)
            }
            AddSourceKind::WindowCapture => {
                prepare_window_picker(state, snapshot.scene_id, actions)
            }
        }
        open = false;
    }
    state.add_dialog_open = open;
}

fn prepare_display_picker(
    state: &mut SourcesPanelState,
    scene_id: Option<SceneId>,
    actions: &mut Vec<UiAction>,
) {
    state.display_targets.clear();
    state.selected_monitor_name = None;

    match crate::capture::source_picker() {
        SourcePicker::Enumerated { monitors, .. } => {
            state.selected_monitor_name = monitors
                .iter()
                .find(|monitor| monitor.is_primary)
                .or_else(|| monitors.first())
                .map(|monitor| monitor.name.clone());
            state.display_targets = monitors;
            state.display_dialog_open = true;
        }
        SourcePicker::SystemDialog => {
            if let Some(scene_id) = scene_id {
                actions.push(UiAction::OpenSystemDisplayPicker(scene_id));
            }
        }
    }
}

fn show_display_dialog(
    ctx: &egui::Context,
    state: &mut SourcesPanelState,
    snapshot: &SourcesSnapshot,
    i18n: &LocalizationManager,
    actions: &mut Vec<UiAction>,
) {
    if !state.display_dialog_open {
        return;
    }

    let mut open = true;
    let mut add = false;
    let mut back = false;
    let mut cancel = false;
    egui::Window::new(i18n.text(TextKey::SourceDisplayTitle))
        .id(egui::Id::new("display_capture_dialog"))
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .collapsible(false)
        .resizable(false)
        .open(&mut open)
        .show(ctx, |ui| {
            ui.set_min_width(360.0);
            ui.label(i18n.text(TextKey::SourceDisplayPrompt));
            ui.add_space(4.0);

            show_list_view(ui, DISPLAY_LIST_HEIGHT, |ui| {
                if state.display_targets.is_empty() {
                    ui.weak(i18n.text(TextKey::SourceDisplayNone));
                } else {
                    egui::ScrollArea::vertical()
                        .id_salt("display_capture_targets")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            for target in &state.display_targets {
                                let selected = state.selected_monitor_name.as_deref()
                                    == Some(target.name.as_str());
                                let label = monitor_label(i18n, target);
                                if list_row(ui, &label, selected).clicked() {
                                    state.selected_monitor_name = Some(target.name.clone());
                                }
                            }
                        });
                }
            });

            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        state.selected_monitor_name.is_some(),
                        egui::Button::new(i18n.text(TextKey::ActionAdd)),
                    )
                    .clicked()
                {
                    add = true;
                }
                if ui.button(i18n.text(TextKey::ActionBack)).clicked() {
                    back = true;
                }
                if ui.button(i18n.text(TextKey::ActionCancel)).clicked() {
                    cancel = true;
                }
            });
        });

    if back {
        open = false;
        state.add_dialog_open = true;
    } else if cancel {
        open = false;
    } else if add {
        let selected = state
            .selected_monitor_name
            .take()
            .and_then(|name| {
                state
                    .display_targets
                    .iter()
                    .find(|target| target.name == name)
            })
            .map(|target| DisplayCaptureSettings {
                target: DisplayCaptureTarget::MonitorName(target.name.clone()),
                // The dialog just showed the user this size; storing it is what
                // makes the new item appear at the display's own shape.
                size_hint: Some([target.rect.width, target.rect.height]),
            });
        if let (Some(scene_id), Some(settings)) = (snapshot.scene_id, selected) {
            actions.push(UiAction::Project(ProjectCommand::Source(
                SourceCommand::AddDisplayCapture { scene_id, settings },
            )));
            state.select_new_item = true;
        }
        open = false;
    }

    state.display_dialog_open = open;
    if !open {
        state.display_targets.clear();
    }
}

/// Chooses how a Window Capture is picked, which is not the same everywhere.
///
/// Where this process may enumerate windows it draws the list itself, the way
/// the display picker does. Where the system owns the picker there is nothing
/// to show and nothing to store: the item is added pointing at the portal,
/// and the portal asks which window the first time the Source opens. Its
/// answer comes back as a restore token, which is what reopens it after that.
fn prepare_window_picker(
    state: &mut SourcesPanelState,
    scene_id: Option<SceneId>,
    actions: &mut Vec<UiAction>,
) {
    state.window_targets.clear();
    state.selected_window = None;

    match crate::capture::source_picker() {
        SourcePicker::Enumerated { windows, .. } => {
            state.selected_window = windows.first().map(|window| window.handle);
            state.window_targets = windows;
            state.window_dialog_open = true;
        }
        SourcePicker::SystemDialog => {
            if let Some(scene_id) = scene_id {
                actions.push(UiAction::Project(ProjectCommand::Source(
                    SourceCommand::AddWindowCapture {
                        scene_id,
                        settings: WindowCaptureSettings {
                            target: WindowCaptureTarget::Portal {
                                restore_token: None,
                            },
                            size_hint: None,
                        },
                    },
                )));
                state.select_new_item = true;
            }
        }
    }
}

fn show_window_dialog(
    ctx: &egui::Context,
    state: &mut SourcesPanelState,
    snapshot: &SourcesSnapshot,
    i18n: &LocalizationManager,
    actions: &mut Vec<UiAction>,
) {
    if !state.window_dialog_open {
        return;
    }

    let mut open = true;
    let mut add = false;
    let mut back = false;
    let mut cancel = false;
    egui::Window::new(i18n.text(TextKey::SourceWindowTitle))
        .id(egui::Id::new("window_capture_dialog"))
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .collapsible(false)
        .resizable(false)
        .open(&mut open)
        .show(ctx, |ui| {
            ui.set_min_width(360.0);
            ui.label(i18n.text(TextKey::SourceWindowPrompt));
            ui.add_space(4.0);

            show_list_view(ui, DISPLAY_LIST_HEIGHT, |ui| {
                if state.window_targets.is_empty() {
                    ui.weak(i18n.text(TextKey::SourceWindowNone));
                } else {
                    egui::ScrollArea::vertical()
                        .id_salt("window_capture_targets")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            for target in &state.window_targets {
                                let selected = state.selected_window == Some(target.handle);
                                let label = window_label(i18n, target);
                                if list_row(ui, &label, selected).clicked() {
                                    state.selected_window = Some(target.handle);
                                }
                            }
                        });
                }
            });

            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        state.selected_window.is_some(),
                        egui::Button::new(i18n.text(TextKey::ActionAdd)),
                    )
                    .clicked()
                {
                    add = true;
                }
                if ui.button(i18n.text(TextKey::ActionBack)).clicked() {
                    back = true;
                }
                if ui.button(i18n.text(TextKey::ActionCancel)).clicked() {
                    cancel = true;
                }
            });
        });

    if back {
        open = false;
        state.add_dialog_open = true;
    } else if cancel {
        open = false;
    } else if add {
        let selected = state
            .selected_window
            .take()
            .and_then(|handle| {
                state
                    .window_targets
                    .iter()
                    .find(|target| target.handle == handle)
            })
            .map(|target| WindowCaptureSettings {
                // The handle itself is deliberately not stored: it is only
                // meaningful while this window lives, and the point of the
                // pair below is to find the window again in a later session.
                target: WindowCaptureTarget::Window {
                    process: target.process.clone(),
                    title: target.title.clone(),
                },
                size_hint: Some([target.size.0, target.size.1]),
            });
        if let (Some(scene_id), Some(settings)) = (snapshot.scene_id, selected) {
            actions.push(UiAction::Project(ProjectCommand::Source(
                SourceCommand::AddWindowCapture { scene_id, settings },
            )));
            state.select_new_item = true;
        }
        open = false;
    }

    state.window_dialog_open = open;
    if !open {
        state.window_targets.clear();
    }
}

fn window_label(i18n: &LocalizationManager, target: &WindowTarget) -> String {
    let mut args = fluent_bundle::FluentArgs::new();
    args.set("title", target.title.as_str());
    args.set("process", target.process.as_str());
    i18n.text_with(TextKey::SourceWindowRow, &args).into_owned()
}

fn monitor_label(i18n: &LocalizationManager, target: &MonitorTarget) -> String {
    let mut args = fluent_bundle::FluentArgs::new();
    args.set("name", target.name.as_str());
    args.set("width", i64::from(target.rect.width));
    args.set("height", i64::from(target.rect.height));
    let key = if target.is_primary {
        TextKey::SourceDisplayMonitorPrimary
    } else {
        TextKey::SourceDisplayMonitor
    };
    i18n.text_with(key, &args).into_owned()
}

fn show_list_view(ui: &mut egui::Ui, height: f32, contents: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(ui.visuals().extreme_bg_color)
        .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
        .inner_margin(egui::Margin::same(2))
        .show(ui, |ui| {
            ui.set_height(height);
            contents(ui);
        });
}

fn list_row(ui: &mut egui::Ui, text: &str, selected: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), LIST_ROW_HEIGHT),
        egui::Sense::click(),
    );
    let (fill, text_color) = if selected {
        (
            ui.visuals().selection.bg_fill,
            ui.visuals().selection.stroke.color,
        )
    } else if response.hovered() {
        (
            ui.visuals().widgets.hovered.weak_bg_fill,
            ui.visuals().widgets.hovered.fg_stroke.color,
        )
    } else {
        (egui::Color32::TRANSPARENT, ui.visuals().text_color())
    };
    ui.painter().rect_filled(rect, 2.0, fill);
    let left = rect.left() + 6.0;
    let elided = elide::paint_one_row(
        ui,
        egui::pos2(left, rect.center().y),
        rect.right() - left - 6.0,
        text,
        text_color,
    );
    // A window title is the worst case in these lists and the one that has to
    // survive: it is what tells two windows of one program apart.
    if elided {
        response.on_hover_text(text)
    } else {
        response
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::MonitorRect;
    use crate::i18n::Locale;

    #[test]
    fn primary_monitor_label_is_localized() {
        let target = MonitorTarget {
            name: r"\\.\DISPLAY1".into(),
            rect: MonitorRect {
                x: 0,
                y: 0,
                width: 1920,
                height: 1080,
            },
            is_primary: true,
        };
        let mut i18n = LocalizationManager::new(Locale::EnUs);
        assert_eq!(
            monitor_label(&i18n, &target),
            r"\\.\DISPLAY1 — 1920×1080 (Primary)"
        );

        i18n.set_locale(Locale::KoKr);
        assert_eq!(
            monitor_label(&i18n, &target),
            r"\\.\DISPLAY1 — 1920×1080 (주 모니터)"
        );
    }
}
