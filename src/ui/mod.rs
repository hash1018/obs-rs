mod action;
mod docking;
mod editor;
mod panels;
mod preview;
mod shell;
mod state;

use eframe::egui;

use crate::i18n::LocalizationManager;
use crate::snapshots::Snapshots;

pub use action::UiAction;
pub use state::UiState;

pub(super) struct UiResources<'a> {
    snapshots: &'a Snapshots,
    i18n: &'a LocalizationManager,
}

pub fn show(
    ui: &mut egui::Ui,
    state: &mut UiState,
    snapshots: &Snapshots,
    i18n: &LocalizationManager,
    actions: &mut Vec<UiAction>,
) {
    let resources = UiResources { snapshots, i18n };
    shell::show(ui, state, &resources, actions);
}
