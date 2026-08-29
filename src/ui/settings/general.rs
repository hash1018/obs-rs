//! The General page: the settings that are about the application rather than
//! about what it produces.
//!
//! Both are also reachable from the View menu, which changes them at once
//! where this page waits for Apply. Two ways to the same setting rather than
//! two copies of it: both write through `AppSettings`, and the mark the menu
//! draws is read from egui itself, so neither can be showing something the
//! other has already changed.

use eframe::egui;

use crate::i18n::{Locale, LocalizationManager, TextKey};
use crate::settings::{AppSettings, Theme};

pub(super) fn show(ui: &mut egui::Ui, draft: &mut AppSettings, i18n: &LocalizationManager) {
    egui::Grid::new("settings_general")
        .num_columns(2)
        .spacing([12.0, 8.0])
        .show(ui, |ui| {
            ui.label(i18n.text(TextKey::SettingsLanguage));
            // Each locale labelled in the language currently displayed, which
            // is what the menu bar already does — a list a reader cannot read
            // is no way out of the wrong language.
            egui::ComboBox::from_id_salt("settings_locale")
                .selected_text(i18n.text(locale_key(draft.locale)))
                .show_ui(ui, |ui| {
                    for locale in Locale::ALL {
                        ui.selectable_value(
                            &mut draft.locale,
                            locale,
                            i18n.text(locale_key(locale)),
                        );
                    }
                });
            ui.end_row();

            ui.label(i18n.text(TextKey::SettingsTheme));
            egui::ComboBox::from_id_salt("settings_theme")
                .selected_text(i18n.text(theme_key(draft.theme)))
                .show_ui(ui, |ui| {
                    for theme in Theme::ALL {
                        ui.selectable_value(&mut draft.theme, theme, i18n.text(theme_key(theme)));
                    }
                });
            ui.end_row();
        });
}

fn theme_key(theme: Theme) -> TextKey {
    match theme {
        Theme::System => TextKey::ThemeSystem,
        Theme::Light => TextKey::ThemeLight,
        Theme::Dark => TextKey::ThemeDark,
    }
}

fn locale_key(locale: Locale) -> TextKey {
    match locale {
        Locale::EnUs => TextKey::LanguageEnglish,
        Locale::KoKr => TextKey::LanguageKorean,
    }
}
