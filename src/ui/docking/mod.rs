mod layout;
mod renderer;

use eframe::egui;

use crate::snapshots::Snapshots;

use super::UiAction;

pub(super) use layout::{DockLayout, DockPanel};

pub(super) fn show(
    ui: &mut egui::Ui,
    layout: &mut DockLayout,
    snapshots: &Snapshots,
    actions: &mut Vec<UiAction>,
) {
    renderer::show(ui, layout, snapshots, actions);
}
