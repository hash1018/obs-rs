use eframe::egui;

use crate::scene::SceneAction;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiAction {
    Exit,
    Scene(SceneAction),
    SetFullscreen(bool),
    SetTheme(egui::ThemePreference),
}
