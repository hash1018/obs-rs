use eframe::egui;

use super::{UiAction, UiState};

pub fn show(ui: &mut egui::Ui, state: &mut UiState, actions: &mut Vec<UiAction>) {
    egui::Panel::top("menu_bar")
        .exact_size(28.0)
        .frame(egui::Frame::new().fill(ui.visuals().panel_fill))
        .show(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("Exit").clicked() {
                        actions.push(UiAction::Exit);
                        ui.close();
                    }
                });

                ui.menu_button("View", |ui| {
                    if ui.checkbox(&mut state.fullscreen, "Fullscreen").changed() {
                        actions.push(UiAction::SetFullscreen(state.fullscreen));
                        ui.close();
                    }

                    ui.menu_button("Theme", |ui| {
                        theme_option(ui, state, actions, egui::ThemePreference::System, "System");
                        theme_option(ui, state, actions, egui::ThemePreference::Light, "Light");
                        theme_option(ui, state, actions, egui::ThemePreference::Dark, "Dark");
                    });
                });

                ui.menu_button("Help", |ui| {
                    if ui.button("About obs-rs").clicked() {
                        state.about_open = true;
                        ui.close();
                    }
                });
            });
        });
}

fn theme_option(
    ui: &mut egui::Ui,
    state: &mut UiState,
    actions: &mut Vec<UiAction>,
    theme: egui::ThemePreference,
    label: &str,
) {
    if ui
        .selectable_value(&mut state.theme, theme, label)
        .changed()
    {
        actions.push(UiAction::SetTheme(theme));
        ui.close();
    }
}

pub fn show_about(ui: &mut egui::Ui, state: &mut UiState) {
    egui::Window::new("About obs-rs")
        .open(&mut state.about_open)
        .collapsible(false)
        .resizable(false)
        .show(ui.ctx(), |ui| {
            ui.heading("obs-rs");
            ui.label("Live capture and recording, built with media-pp.");
        });
}
