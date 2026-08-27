use eframe::egui;

use super::docking::DockLayout;
use super::panels::scenes::ScenesPanelState;

pub struct UiState {
    pub(super) about_open: bool,
    pub(super) dock_layout: DockLayout,
    pub(super) fullscreen: bool,
    pub(super) scenes: ScenesPanelState,
    pub(super) theme: egui::ThemePreference,
}

impl UiState {
    pub fn theme(&self) -> egui::ThemePreference {
        self.theme
    }
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            about_open: false,
            dock_layout: DockLayout::default(),
            fullscreen: false,
            scenes: ScenesPanelState::default(),
            theme: egui::ThemePreference::Dark,
        }
    }
}
