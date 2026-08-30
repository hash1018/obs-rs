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

pub fn show(
    ui: &mut egui::Ui,
    state: &mut UiState,
    resources: &UiResources<'_>,
    actions: &mut Vec<UiAction>,
) {
    state.editor.sync(&resources.snapshots.sources);
    menu_bar::show(ui, state, resources.i18n, actions);
    status_bar::show(ui, &resources.snapshots.status, resources.i18n);
    docking::show(
        ui,
        &mut state.dock_layout,
        &mut state.scenes,
        &mut state.sources,
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
    // Last, so it draws over the docks it was opened from.
    settings::show(
        ui.ctx(),
        &mut state.settings,
        resources.snapshots.status.recording_elapsed.is_some(),
        &resources.snapshots.status.encoders,
        &resources.snapshots.status.audio_codecs,
        resources.i18n,
        actions,
    );
}
