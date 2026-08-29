//! The dock the recording and settings buttons live in.

use eframe::egui;

use crate::i18n::{LocalizationManager, TextKey};
use crate::snapshots::StatusSnapshot;

use super::super::UiAction;
use super::toolbar;

/// Tall enough to read as a primary control rather than a list row, which is
/// what separates this dock from the Scenes and Sources lists beside it.
const BUTTON_HEIGHT: f32 = 30.0;
const BUTTON_SPACING: f32 = 6.0;

pub(in crate::ui) fn show(
    ui: &mut egui::Ui,
    status: &StatusSnapshot,
    i18n: &LocalizationManager,
    actions: &mut Vec<UiAction>,
) {
    // Scrolls like every other dock here, but with nothing reserved below
    // it: the buttons *are* this dock's content, so there is no strip to
    // keep them out of. Without it a squeezed pane simply clipped the lower
    // ones away — and it grew a third the day recording learned to pause.
    toolbar::scroll_content(ui, "controls_buttons", |ui| {
        show_buttons(ui, status, i18n, actions);
    });
}

fn show_buttons(
    ui: &mut egui::Ui,
    status: &StatusSnapshot,
    i18n: &LocalizationManager,
    actions: &mut Vec<UiAction>,
) {
    // The engine's own answer, not a flag this dock keeps: a recording that
    // failed to start never sets it, and a button reading "Stop Recording"
    // over a recording that is not running would be worse than a click that
    // did nothing.
    let recording = status.recording_elapsed.is_some();
    let label = if recording {
        TextKey::ControlStopRecording
    } else {
        TextKey::ControlStartRecording
    };
    if button(ui, i18n, label).clicked() {
        actions.push(if recording {
            UiAction::StopRecording
        } else {
            UiAction::StartRecording
        });
    }

    // Only while one is running: a pause button over no recording has
    // nothing to pause, and one that is always there but usually dead is
    // worse than one that appears when it means something.
    if recording {
        ui.add_space(BUTTON_SPACING);
        let label = if status.recording_paused {
            TextKey::ControlResumeRecording
        } else {
            TextKey::ControlPauseRecording
        };
        if button(ui, i18n, label).clicked() {
            actions.push(UiAction::SetRecordingPaused(!status.recording_paused));
        }
    }

    ui.add_space(BUTTON_SPACING);
    if button(ui, i18n, TextKey::ControlSettings).clicked() {
        actions.push(UiAction::OpenSettings);
    }
}

/// One full-width button.
///
/// Full width rather than laid out in a row: the dock is a narrow column, and
/// a button that fills it stays legible at every width the splitter allows.
/// `add_sized` rather than a `min_size`, because that is what centres the
/// label in a button wider than its text.
fn button(ui: &mut egui::Ui, i18n: &LocalizationManager, label: TextKey) -> egui::Response {
    ui.add_sized(
        [ui.available_width(), BUTTON_HEIGHT],
        egui::Button::new(i18n.text(label)),
    )
}
