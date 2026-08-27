use eframe::egui;

use crate::project::ProjectCommand;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiAction {
    Exit,
    Project(ProjectCommand),
    SetFullscreen(bool),
    SetTheme(egui::ThemePreference),
}
