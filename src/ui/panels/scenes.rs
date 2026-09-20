use eframe::egui;

use crate::domain::{MAX_TRANSITION_MS, MIN_TRANSITION_MS, SceneId, Transition, TransitionKind};
use crate::i18n::{LocalizationManager, TextKey};
use crate::project::{ProjectCommand, SceneCommand};
use crate::snapshots::ScenesSnapshot;

use super::super::UiAction;
use super::elide;
use super::toolbar::{self, ToolIcon};

const SCENE_ROW_HEIGHT: f32 = 28.0;
/// What a `Button` keeps for itself either side of its label.
const BUTTON_PADDING: f32 = 12.0;

#[derive(Default)]
pub(in crate::ui) struct ScenesPanelState {
    rename: Option<RenameState>,
    /// The Scene whose deletion is being asked about — see
    /// `show_delete_dialog`.
    deleting: Option<SceneId>,
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

    // Taken before either strip is shown, so the list gets a `Ui` that
    // cannot reach them — see `toolbar::reserve_list_below`. The transition
    // row is added first because a bottom panel takes the bottom: added
    // after the buttons it would sit above them.
    let mut list = toolbar::reserve_list_below(ui, "scenes_list_area", TRANSITION_ROW_HEIGHT);
    show_transition(ui, snapshot, i18n, actions);
    show_toolbar(ui, snapshot, &mut state.deleting, i18n, actions);
    show_delete_dialog(ui.ctx(), state, snapshot, i18n, actions);

    toolbar::scroll_content(&mut list, "scenes_list", |ui| {
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

            // Laid out here rather than left to the button, which would
            // rather grow than give anything up — and a dock cannot grow.
            let label = elide::one_row(
                ui,
                &scene.name,
                row_width - BUTTON_PADDING,
                &egui::TextStyle::Button,
            );
            let elided = label.elided;
            let response = ui.add_sized(
                [row_width, SCENE_ROW_HEIGHT],
                egui::Button::new(label).selected(selected),
            );
            let response = if elided {
                response.on_hover_text(&scene.name)
            } else {
                response
            };
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
    deleting: &mut Option<SceneId>,
    i18n: &LocalizationManager,
    actions: &mut Vec<UiAction>,
) {
    let selected = snapshot.selected_scene_id;
    let selected_index =
        selected.and_then(|selected| snapshot.items.iter().position(|scene| scene.id == selected));

    toolbar::strip(ui, "scenes_toolbar", |ui| {
        if toolbar::button(ui, ToolIcon::Add, i18n.text(TextKey::SceneAdd), true).clicked() {
            actions.push(scene_action(SceneCommand::Add));
        }
        if toolbar::button(
            ui,
            ToolIcon::Remove,
            i18n.text(TextKey::SceneRemove),
            selected.is_some() && snapshot.items.len() > 1,
        )
        .clicked()
            && let Some(scene_id) = selected
        {
            // Asked about first where other Scenes show this one: deleting it
            // empties them too — see `show_delete_dialog`.
            match snapshot
                .items
                .iter()
                .find(|scene| scene.id == scene_id)
                .is_some_and(|scene| !scene.shown_in.is_empty())
            {
                true => *deleting = Some(scene_id),
                false => actions.push(scene_action(SceneCommand::Delete(scene_id))),
            }
        }
        if toolbar::button(
            ui,
            ToolIcon::Duplicate,
            i18n.text(TextKey::SceneDuplicate),
            selected.is_some(),
        )
        .clicked()
            && let Some(scene_id) = selected
        {
            actions.push(scene_action(SceneCommand::Duplicate(scene_id)));
        }
        if toolbar::button(
            ui,
            ToolIcon::MoveUp,
            i18n.text(TextKey::SceneMoveUp),
            selected_index.is_some_and(|index| index > 0),
        )
        .clicked()
            && let Some(scene_id) = selected
        {
            actions.push(scene_action(SceneCommand::MoveUp(scene_id)));
        }
        if toolbar::button(
            ui,
            ToolIcon::MoveDown,
            i18n.text(TextKey::SceneMoveDown),
            selected_index.is_some_and(|index| index + 1 < snapshot.items.len()),
        )
        .clicked()
            && let Some(scene_id) = selected
        {
            actions.push(scene_action(SceneCommand::MoveDown(scene_id)));
        }
    });
}

fn scene_action(command: SceneCommand) -> UiAction {
    UiAction::Project(ProjectCommand::Scene(command))
}

/// How tall the row under the buttons is. A combo box and a number field,
/// with the same air around them the buttons have.
const TRANSITION_ROW_HEIGHT: f32 = 30.0;

/// What the number field needs before it starts clipping its own suffix.
const DURATION_WIDTH: f32 = 62.0;

/// What a Scene switch does, under the list it applies to.
///
/// The kind is written the moment it is picked. The length is written when
/// the drag ends, the way every other number here is: a drag through a second
/// of values is one setting, not four hundred, and what is being dragged is
/// kept in egui's own memory meanwhile — without that the field reads back
/// the stored value every frame and will not move at all.
fn show_transition(
    ui: &mut egui::Ui,
    snapshot: &ScenesSnapshot,
    i18n: &LocalizationManager,
    actions: &mut Vec<UiAction>,
) {
    toolbar::row(ui, "scenes_transition", TRANSITION_ROW_HEIGHT, |ui| {
        let stored = snapshot.transition;
        ui.label(i18n.text(TextKey::SceneTransition));

        let held = egui::Id::new("scene-transition-ms");
        let width = (ui.available_width() - DURATION_WIDTH - ui.spacing().item_spacing.x).max(60.0);
        egui::ComboBox::from_id_salt("scene-transition-kind")
            .width(width)
            .selected_text(kind_label(stored.kind, i18n))
            .show_ui(ui, |ui| {
                for kind in TransitionKind::ALL {
                    if ui
                        .selectable_label(stored.kind == kind, kind_label(kind, i18n))
                        .clicked()
                        && stored.kind != kind
                    {
                        actions.push(scene_action(SceneCommand::SetTransition(Transition {
                            kind,
                            ..stored
                        })));
                    }
                }
            });

        // Dimmed rather than hidden for a cut: what it would be is worth
        // seeing while deciding whether to turn one on.
        let mut milliseconds = ui
            .data(|data| data.get_temp::<u32>(held))
            .unwrap_or(stored.milliseconds);
        let field = ui.add_enabled(
            stored.kind.is_animated(),
            egui::DragValue::new(&mut milliseconds)
                .range(MIN_TRANSITION_MS..=MAX_TRANSITION_MS)
                .speed(5)
                .suffix(" ms"),
        );
        if field.changed() {
            ui.data_mut(|data| data.insert_temp(held, milliseconds));
        }
        if field.drag_stopped() || field.lost_focus() {
            ui.data_mut(|data| data.remove_temp::<u32>(held));
            if milliseconds != stored.milliseconds {
                actions.push(scene_action(SceneCommand::SetTransition(Transition {
                    milliseconds,
                    ..stored
                })));
            }
        }
        field.on_hover_text(i18n.text(TextKey::SceneTransitionLength));
    });
}

fn kind_label(kind: TransitionKind, i18n: &LocalizationManager) -> std::borrow::Cow<'_, str> {
    i18n.text(match kind {
        TransitionKind::Cut => TextKey::SceneTransitionCut,
        TransitionKind::Fade => TextKey::SceneTransitionFade,
        TransitionKind::FadeToBlack => TextKey::SceneTransitionFadeToBlack,
    })
}

/// Asks before a Scene that other Scenes show is deleted.
///
/// Deleting it takes the items showing it with it, wherever they are — so
/// this names those Scenes rather than leaving somebody to find an overlay
/// missing from a Scene they were not looking at. A Scene nothing shows is
/// deleted where it is asked for, with no question.
fn show_delete_dialog(
    ctx: &egui::Context,
    state: &mut ScenesPanelState,
    snapshot: &ScenesSnapshot,
    i18n: &LocalizationManager,
    actions: &mut Vec<UiAction>,
) {
    let Some(scene_id) = state.deleting else {
        return;
    };
    let Some(scene) = snapshot.items.iter().find(|scene| scene.id == scene_id) else {
        state.deleting = None;
        return;
    };

    let mut delete = false;
    let mut cancel = false;
    let shown = crate::ui::dialog::show(
        ctx,
        "scene_delete_dialog",
        &i18n.text(TextKey::SceneDeleteTitle),
        |ui| {
            ui.set_min_width(320.0);
            let mut args = fluent_bundle::FluentArgs::new();
            args.set("scene", scene.name.clone());
            args.set("scenes", scene.shown_in.join(", "));
            ui.label(i18n.text_with(TextKey::SceneDeleteUsed, &args));
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui.button(i18n.text(TextKey::ActionDelete)).clicked() {
                    delete = true;
                }
                if ui.button(i18n.text(TextKey::ActionCancel)).clicked() {
                    cancel = true;
                }
            });
        },
    );

    if delete {
        actions.push(scene_action(SceneCommand::Delete(scene_id)));
    }
    if delete || cancel || shown.escaped {
        state.deleting = None;
    }
}
