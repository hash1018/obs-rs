mod menu_bar;
mod status_bar;

use eframe::egui;

use super::{UiAction, UiResources, UiState, docking};

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
        &resources.snapshots.sources,
        resources.i18n,
        actions,
    );
    menu_bar::show_about(ui, state, resources.i18n);
}
