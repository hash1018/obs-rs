mod menu_bar;
mod preview;
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
    menu_bar::show(ui, state, actions);
    status_bar::show(ui, &snapshots.status);
    docking::show(ui, &mut state.dock_layout);
    preview::show(ui);
    menu_bar::show_about(ui, state);
}
