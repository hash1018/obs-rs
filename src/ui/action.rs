use eframe::egui;

use crate::i18n::Locale;
use crate::{domain::SceneId, project::ProjectCommand};

#[derive(Debug, Clone, PartialEq)]
pub enum UiAction {
    Exit,
    Project(ProjectCommand),
    OpenSystemDisplayPicker(SceneId),
    SetFullscreen(bool),
    SetTheme(egui::ThemePreference),
    SetLocale(Locale),
}
