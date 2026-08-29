//! The General page.
//!
//! Language only, for now. Theme belongs here too by rights, but it is not
//! part of [`AppSettings`] — it lives in the UI's own state and is not
//! persisted at all, so a page that offered it would be offering something
//! that does not survive a restart. Persisting it is its own change; until
//! then the View menu is where it is set.

use eframe::egui;

use crate::i18n::{Locale, LocalizationManager, TextKey};
use crate::settings::AppSettings;

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
        });
}

fn locale_key(locale: Locale) -> TextKey {
    match locale {
        Locale::EnUs => TextKey::LanguageEnglish,
        Locale::KoKr => TextKey::LanguageKorean,
    }
}
