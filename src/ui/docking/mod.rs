mod layout;
mod renderer;

use eframe::egui;

pub(super) use layout::{DockLayout, DockPanel};

pub(super) fn show(ui: &mut egui::Ui, layout: &mut DockLayout) {
    renderer::show(ui, layout);
}
