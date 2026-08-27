use eframe::egui;

use crate::i18n::Locale;
use crate::project::ProjectCommand;

#[derive(Debug, Clone, PartialEq)]
pub enum UiAction {
    Exit,
    Project(ProjectCommand),
    SetFullscreen(bool),
    SetTheme(egui::ThemePreference),
    SetLocale(Locale),
}
