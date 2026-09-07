mod hotkeys;
mod menu_bar;
mod status_bar;

use eframe::egui;

use super::{UiAction, UiResources, UiState, docking, settings};

use crate::i18n::{LocalizationManager, TextKey};

/// Asks before closing the window on a running recording.
///
/// Modal in the sense that matters — it is the only thing that can answer the
/// question — but not in egui's: the window behind it stays live, because a
/// recording is still running and its clock is part of what the answer
/// depends on.
fn confirm_exit(
    ctx: &egui::Context,
    state: &mut UiState,
    i18n: &LocalizationManager,
    actions: &mut Vec<UiAction>,
) {
    if !state.exit_confirm_open {
        return;
    }
    let mut open = true;
    let mut answered = false;
    egui::Window::new(i18n.text(TextKey::ExitWhileRecordingTitle))
        .id(egui::Id::new("exit_confirm"))
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .collapsible(false)
        .resizable(false)
        .open(&mut open)
        .show(ctx, |ui| {
            ui.set_max_width(320.0);
            ui.label(i18n.text(TextKey::ExitWhileRecordingBody));
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui.button(i18n.text(TextKey::ExitStopAndQuit)).clicked() {
                    actions.push(UiAction::StopRecordingAndExit);
                    answered = true;
                }
                // Carrying on is the safe answer, so it is the one the window's
                // own close button and Escape land on.
                if ui.button(i18n.text(TextKey::ExitKeepRecording)).clicked() {
                    answered = true;
                }
            });
        });
    if answered || !open {
        state.exit_confirm_open = false;
    }
}

/// Says that the project could not be opened, once.
///
/// Dismissible rather than fatal: the application still composites, still
/// previews and still records, and a user who wants those should not be shut
/// out of them. What they must not do is add Sources for an hour and find out
/// at the end — so this is a window in the middle of the screen rather than a
/// line in a status bar.
fn report_project_error(ctx: &egui::Context, state: &mut UiState, i18n: &LocalizationManager) {
    let Some(error) = state.project_error.clone() else {
        return;
    };
    let mut open = true;
    let mut answered = false;
    egui::Window::new(i18n.text(TextKey::ProjectUnavailableTitle))
        .id(egui::Id::new("project_error"))
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .collapsible(false)
        .resizable(false)
        .open(&mut open)
        .show(ctx, |ui| {
            ui.set_max_width(420.0);
            ui.label(i18n.text(TextKey::ProjectUnavailableBody));
            ui.add_space(8.0);
            ui.weak(error);
            ui.add_space(12.0);
            if ui
                .button(i18n.text(TextKey::ProjectUnavailableDismiss))
                .clicked()
            {
                answered = true;
            }
        });
    if answered || !open {
        state.project_error = None;
    }
}

pub fn show(
    ui: &mut egui::Ui,
    state: &mut UiState,
    resources: &UiResources<'_>,
    actions: &mut Vec<UiAction>,
) {
    state.editor.sync(&resources.snapshots.sources);
    // Before anything is drawn, so a chord is spent on what it is bound to
    // rather than on whatever widget happens to have focus.
    hotkeys::dispatch(
        ui.ctx(),
        state,
        resources.snapshots,
        &resources.settings.hotkeys,
        actions,
    );
    menu_bar::show(ui, state, resources.i18n, actions);
    status_bar::show(ui, &resources.snapshots.status, resources.i18n);
    docking::show(
        ui,
        &mut state.dock_layout,
        docking::PanelStates {
            scenes: &mut state.scenes,
            sources: &mut state.sources,
            filters: &mut state.filters,
        },
        &mut state.editor,
        resources,
        actions,
    );
    super::preview::show(
        ui,
        &mut state.preview,
        &mut state.editor,
        resources,
        actions,
    );
    menu_bar::show_about(ui, state, resources.i18n);
    confirm_exit(ui.ctx(), state, resources.i18n, actions);
    report_project_error(ui.ctx(), state, resources.i18n);
    // Last, so it draws over the docks it was opened from.
    settings::show(
        ui.ctx(),
        &mut state.settings,
        resources.snapshots.status.recording_elapsed.is_some(),
        resources.snapshots.status.streaming_elapsed.is_some(),
        &resources.snapshots.status.encoders,
        &resources.snapshots.status.audio_codecs,
        resources.audio_devices,
        &resources.snapshots.audio,
        resources.i18n,
        actions,
    );
}
