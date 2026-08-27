use eframe::egui;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiAction {
    Exit,
    SetFullscreen(bool),
    SetTheme(egui::ThemePreference),
}
