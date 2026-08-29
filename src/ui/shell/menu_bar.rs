use eframe::egui;

use crate::i18n::{Locale, LocalizationManager, TextKey};
use crate::settings::Theme;

use super::{UiAction, UiState, docking::DockPanel};

pub fn show(
    ui: &mut egui::Ui,
    state: &mut UiState,
    i18n: &LocalizationManager,
    actions: &mut Vec<UiAction>,
) {
    egui::Panel::top("menu_bar")
        .exact_size(28.0)
        .frame(egui::Frame::new().fill(ui.visuals().panel_fill))
        .show(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button(i18n.text(TextKey::MenuFile), |ui| {
                    if ui.button(i18n.text(TextKey::MenuExit)).clicked() {
                        actions.push(UiAction::Exit);
                        ui.close();
                    }
                });

                ui.menu_button(i18n.text(TextKey::MenuView), |ui| {
                    if ui
                        .checkbox(&mut state.fullscreen, i18n.text(TextKey::MenuFullscreen))
                        .changed()
                    {
                        actions.push(UiAction::SetFullscreen(state.fullscreen));
                        ui.close();
                    }

                    ui.menu_button(i18n.text(TextKey::MenuDocks), |ui| {
                        dock_option(ui, state, DockPanel::Scenes, i18n.text(TextKey::DockScenes));
                        dock_option(
                            ui,
                            state,
                            DockPanel::Sources,
                            i18n.text(TextKey::DockSources),
                        );
                        dock_option(
                            ui,
                            state,
                            DockPanel::Controls,
                            i18n.text(TextKey::DockControls),
                        );
                    });

                    ui.menu_button(i18n.text(TextKey::MenuTheme), |ui| {
                        theme_option(ui, actions, Theme::System, i18n.text(TextKey::ThemeSystem));
                        theme_option(ui, actions, Theme::Light, i18n.text(TextKey::ThemeLight));
                        theme_option(ui, actions, Theme::Dark, i18n.text(TextKey::ThemeDark));
                    });

                    ui.menu_button(i18n.text(TextKey::MenuLanguage), |ui| {
                        for locale in Locale::ALL {
                            let key = match locale {
                                Locale::EnUs => TextKey::LanguageEnglish,
                                Locale::KoKr => TextKey::LanguageKorean,
                            };
                            if ui
                                .selectable_label(i18n.locale() == locale, i18n.text(key))
                                .clicked()
                            {
                                actions.push(UiAction::SetLocale(locale));
                                ui.close();
                            }
                        }
                    });
                });

                ui.menu_button(i18n.text(TextKey::MenuHelp), |ui| {
                    if ui.button(i18n.text(TextKey::MenuAbout)).clicked() {
                        state.about_open = true;
                        ui.close();
                    }
                });
            });
        });
}

fn dock_option(
    ui: &mut egui::Ui,
    state: &mut UiState,
    panel: DockPanel,
    label: impl Into<egui::WidgetText>,
) {
    let mut open = state.dock_layout.is_open(panel);
    if ui.checkbox(&mut open, label).changed() {
        state.dock_layout.set_open(panel, open);
        ui.close();
    }
}

/// One theme entry, marked when it is the one in force.
///
/// The mark is read from egui rather than from any copy this module keeps.
/// `set_theme` writes exactly this, so it is the one answer that cannot drift
/// from what the window is actually drawing — which a second copy here did,
/// once the Settings dialog gained a way to change it too.
fn theme_option(
    ui: &mut egui::Ui,
    actions: &mut Vec<UiAction>,
    theme: Theme,
    label: impl Into<egui::WidgetText>,
) {
    let current: Theme = ui.ctx().options(|options| options.theme_preference).into();
    if ui.selectable_label(current == theme, label).clicked() {
        actions.push(UiAction::SetTheme(theme));
        ui.close();
    }
}

pub fn show_about(ui: &mut egui::Ui, state: &mut UiState, i18n: &LocalizationManager) {
    egui::Window::new(i18n.text(TextKey::MenuAbout))
        .open(&mut state.about_open)
        .collapsible(false)
        .resizable(false)
        .show(ui.ctx(), |ui| {
            ui.heading("obs-rs");
            ui.label(i18n.text(TextKey::AboutDescription));
        });
}
