use eframe::egui;

use crate::domain::SceneId;
use crate::i18n::{LocalizationManager, TextKey};
use crate::project::{ProjectCommand, SceneCommand};
use crate::snapshots::ScenesSnapshot;

use super::super::UiAction;
use super::toolbar::{self, ToolIcon};

const SCENE_ROW_HEIGHT: f32 = 28.0;

#[derive(Default)]
pub(in crate::ui) struct ScenesPanelState {
    rename: Option<RenameState>,
}

struct RenameState {
    scene_id: SceneId,
    name: String,
    request_focus: bool,
    error: Option<TextKey>,
}

pub(in crate::ui) fn show(
    ui: &mut egui::Ui,
    state: &mut ScenesPanelState,
    snapshot: &ScenesSnapshot,
    i18n: &LocalizationManager,
    actions: &mut Vec<UiAction>,
) {
    if state.rename.as_ref().is_some_and(|rename| {
        !snapshot
            .items
            .iter()
            .any(|scene| scene.id == rename.scene_id)
    }) {
        state.rename = None;
    }

    show_toolbar(ui, snapshot, i18n, actions);

    egui::ScrollArea::vertical()
        .id_salt("scenes_list")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for scene in &snapshot.items {
                let selected = snapshot.selected_scene_id == Some(scene.id);
                let row_width = ui.available_width();
                if state
                    .rename
                    .as_ref()
                    .is_some_and(|rename| rename.scene_id == scene.id)
                {
                    show_rename_editor(ui, state, snapshot, scene.id, row_width, i18n, actions);
                    continue;
                }

                let response = ui.add_sized(
                    [row_width, SCENE_ROW_HEIGHT],
                    egui::Button::new(&scene.name).selected(selected),
                );
                if response.clicked() {
                    actions.push(scene_action(SceneCommand::Select(scene.id)));
                }
                if response.double_clicked() {
                    state.rename = Some(RenameState {
                        scene_id: scene.id,
                        name: scene.name.clone(),
                        request_focus: true,
                        error: None,
                    });
                }
            }
        });
}

fn show_rename_editor(
    ui: &mut egui::Ui,
    state: &mut ScenesPanelState,
    snapshot: &ScenesSnapshot,
    scene_id: SceneId,
    row_width: f32,
    i18n: &LocalizationManager,
    actions: &mut Vec<UiAction>,
) {
    let rename = state.rename.as_mut().expect("rename state must exist");
    let mut response = ui.add_sized(
        [row_width, SCENE_ROW_HEIGHT],
        egui::TextEdit::singleline(&mut rename.name)
            .id_salt(("scene_rename", scene_id.0))
            .vertical_align(egui::Align::Center)
            .background_color(rename.error.map_or(ui.visuals().extreme_bg_color, |_| {
                ui.visuals().error_fg_color.gamma_multiply(0.2)
            })),
    );
    if response.changed() {
        rename.error = None;
    }
    if let Some(error) = rename.error {
        response = response.on_hover_text(i18n.text(error));
    }
    if rename.request_focus {
        response.request_focus();
        rename.request_focus = false;
    }

    let cancel = ui.input(|input| input.key_pressed(egui::Key::Escape));
    let commit = ui.input(|input| input.key_pressed(egui::Key::Enter));
    let lost_focus = response.lost_focus();

    if cancel {
        state.rename = None;
    } else if commit || lost_focus {
        let name = rename.name.trim();
        rename.error = if name.is_empty() {
            Some(TextKey::SceneNameEmpty)
        } else if snapshot
            .items
            .iter()
            .any(|scene| scene.id != scene_id && scene.name == name)
        {
            Some(TextKey::SceneNameDuplicate)
        } else {
            None
        };

        if rename.error.is_none() {
            actions.push(scene_action(SceneCommand::Rename(
                scene_id,
                name.to_owned(),
            )));
            state.rename = None;
        } else {
            response.request_focus();
        }
    }
}

fn show_toolbar(
    ui: &mut egui::Ui,
    snapshot: &ScenesSnapshot,
    i18n: &LocalizationManager,
    actions: &mut Vec<UiAction>,
) {
    let selected = snapshot.selected_scene_id;
    let selected_index =
        selected.and_then(|selected| snapshot.items.iter().position(|scene| scene.id == selected));

    egui::Panel::bottom("scenes_toolbar")
        .exact_size(toolbar::HEIGHT)
        .resizable(false)
        .frame(
            egui::Frame::new()
                .fill(ui.visuals().panel_fill)
                .inner_margin(egui::Margin::symmetric(4, 5)),
        )
        .show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                if toolbar::button(ui, ToolIcon::Add, i18n.text(TextKey::SceneAdd), true) {
                    actions.push(scene_action(SceneCommand::Add));
                }
                if toolbar::button(
                    ui,
                    ToolIcon::Remove,
                    i18n.text(TextKey::SceneRemove),
                    selected.is_some() && snapshot.items.len() > 1,
                ) && let Some(scene_id) = selected
                {
                    actions.push(scene_action(SceneCommand::Delete(scene_id)));
                }
                if toolbar::button(
                    ui,
                    ToolIcon::Duplicate,
                    i18n.text(TextKey::SceneDuplicate),
                    selected.is_some(),
                ) && let Some(scene_id) = selected
                {
                    actions.push(scene_action(SceneCommand::Duplicate(scene_id)));
                }
                if toolbar::button(
                    ui,
                    ToolIcon::MoveUp,
                    i18n.text(TextKey::SceneMoveUp),
                    selected_index.is_some_and(|index| index > 0),
                ) && let Some(scene_id) = selected
                {
                    actions.push(scene_action(SceneCommand::MoveUp(scene_id)));
                }
                if toolbar::button(
                    ui,
                    ToolIcon::MoveDown,
                    i18n.text(TextKey::SceneMoveDown),
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
