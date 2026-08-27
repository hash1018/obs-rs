use eframe::egui;

pub struct UiState {
    pub(super) about_open: bool,
    pub(super) fullscreen: bool,
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
            fullscreen: false,
            theme: egui::ThemePreference::Dark,
        }
    }
}
