//! The dock the recording and settings buttons live in.
//!
//! Presentation only for now. Neither button has anything behind it yet — one
//! wants an encoder branch on the engine's pipeline, the other a settings
//! dialog — so neither emits a `UiAction`, and a hover says so rather than
//! leaving a click that looks like it worked. They start emitting in the same
//! change that gives them something to emit to.

use eframe::egui;

use crate::i18n::{LocalizationManager, TextKey};

/// Tall enough to read as a primary control rather than a list row, which is
/// what separates this dock from the Scenes and Sources lists beside it.
const BUTTON_HEIGHT: f32 = 30.0;
const BUTTON_SPACING: f32 = 6.0;

pub(in crate::ui) fn show(ui: &mut egui::Ui, i18n: &LocalizationManager) {
    button(ui, i18n, TextKey::ControlStartRecording);
    ui.add_space(BUTTON_SPACING);
    button(ui, i18n, TextKey::ControlSettings);
}

/// One full-width button.
///
/// Full width rather than laid out in a row: the dock is a narrow column, and
/// a button that fills it stays legible at every width the splitter allows.
/// `add_sized` rather than a `min_size`, because that is what centres the
/// label in a button wider than its text.
fn button(ui: &mut egui::Ui, i18n: &LocalizationManager, label: TextKey) {
    ui.add_sized(
        [ui.available_width(), BUTTON_HEIGHT],
        egui::Button::new(i18n.text(label)),
    )
    .on_hover_text(i18n.text(TextKey::ControlUnavailable));
}
