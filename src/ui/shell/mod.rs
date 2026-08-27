mod menu_bar;
mod status_bar;

use eframe::egui;

use crate::snapshots::Snapshots;

use super::{UiAction, UiState, docking};

pub fn show(
    ui: &mut egui::Ui,
    state: &mut UiState,
    snapshots: &Snapshots,
    actions: &mut Vec<UiAction>,
) {
    state.editor.sync(&snapshots.sources);
    menu_bar::show(ui, state, actions);
    status_bar::show(ui, &snapshots.status);
    docking::show(
        ui,
        &mut state.dock_layout,
        &mut state.scenes,
        &mut state.sources,
        &mut state.editor,
        snapshots,
        actions,
    );
    super::preview::show(
        ui,
        &mut state.preview,
        &mut state.editor,
        &snapshots.sources,
        actions,
    );
    menu_bar::show_about(ui, state);
}
