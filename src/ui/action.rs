use eframe::egui;

use crate::project::ProjectCommand;

#[derive(Debug, Clone, PartialEq)]
pub enum UiAction {
    Exit,
    Project(ProjectCommand),
    SetFullscreen(bool),
    SetTheme(egui::ThemePreference),
}
