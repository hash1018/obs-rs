use eframe::egui;

use super::docking::DockLayout;
use super::editor::SceneEditorState;
use super::panels::scenes::ScenesPanelState;
use super::panels::sources::SourcesPanelState;
use super::preview::PreviewViewState;
use super::settings::SettingsDialogState;

pub struct UiState {
    pub(super) about_open: bool,
    pub(super) dock_layout: DockLayout,
    pub(super) fullscreen: bool,
    pub(super) scenes: ScenesPanelState,
    pub(super) editor: SceneEditorState,
    pub(super) sources: SourcesPanelState,
    pub(super) preview: PreviewViewState,
    pub(super) settings: SettingsDialogState,
    pub(super) theme: egui::ThemePreference,
}

impl UiState {
    pub fn theme(&self) -> egui::ThemePreference {
        self.theme
    }

    /// Opens the Settings dialog on a copy of what is currently set.
    ///
    /// The draft is seeded by the caller's settings rather than read from
    /// anywhere in here: this module holds no copy of them, and one taken
    /// from a stale place would quietly put old values back on Apply.
    pub fn open_settings(&mut self, settings: &crate::settings::AppSettings) {
        self.settings.open_with(settings);
    }
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            about_open: false,
            dock_layout: DockLayout::default(),
            fullscreen: false,
            scenes: ScenesPanelState::default(),
            editor: SceneEditorState::default(),
            sources: SourcesPanelState::default(),
            preview: PreviewViewState::default(),
            settings: SettingsDialogState::default(),
            theme: egui::ThemePreference::Dark,
        }
    }
}
