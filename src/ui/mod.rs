mod action;
mod docking;
mod editor;
mod panels;
mod preview;
mod shell;
mod state;

use eframe::egui;

use crate::snapshots::Snapshots;

pub use action::UiAction;
pub use state::UiState;

pub fn show(
    ui: &mut egui::Ui,
    state: &mut UiState,
    snapshots: &Snapshots,
    actions: &mut Vec<UiAction>,
) {
    shell::show(ui, state, snapshots, actions);
}
