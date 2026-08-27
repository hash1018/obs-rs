mod layout;
mod renderer;

use eframe::egui;

use crate::snapshots::Snapshots;

use super::{UiAction, panels::scenes::ScenesPanelState};

pub(super) use layout::{DockLayout, DockPanel};

pub(super) fn show(
    ui: &mut egui::Ui,
    layout: &mut DockLayout,
    scenes_state: &mut ScenesPanelState,
    snapshots: &Snapshots,
    actions: &mut Vec<UiAction>,
) {
    renderer::show(ui, layout, scenes_state, snapshots, actions);
}
