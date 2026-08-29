mod action;
mod docking;
mod editor;
mod panels;
mod preview;
mod settings;
mod shell;
mod state;

use eframe::egui;

use crate::engine::CompositeFrame;
use crate::i18n::LocalizationManager;
use crate::snapshots::Snapshots;

pub use action::UiAction;
pub use state::UiState;

pub(super) struct UiResources<'a> {
    snapshots: &'a Snapshots,
    i18n: &'a LocalizationManager,
    /// The latest frame the engine composited, or `None` before the first one
    /// arrives or when no engine is running.
    composite_frame: Option<&'a CompositeFrame>,
}

pub fn show(
    ui: &mut egui::Ui,
    state: &mut UiState,
    snapshots: &Snapshots,
    i18n: &LocalizationManager,
    composite_frame: Option<&CompositeFrame>,
    actions: &mut Vec<UiAction>,
) {
    let resources = UiResources {
        snapshots,
        i18n,
        composite_frame,
    };
    shell::show(ui, state, &resources, actions);
}
