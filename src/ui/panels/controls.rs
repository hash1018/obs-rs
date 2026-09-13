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

/// `replay_save_key` is the key bound to saving a replay, as it is written,
/// or `None` when there is none.
pub(in crate::ui) fn show(
    ui: &mut egui::Ui,
    status: &StatusSnapshot,
    replay_save_key: Option<&str>,
    i18n: &LocalizationManager,
    actions: &mut Vec<UiAction>,
) {
    // Scrolls like every other dock here, but with nothing reserved below
    // it: the buttons *are* this dock's content, so there is no strip to
    // keep them out of. Without it a squeezed pane simply clipped the lower
    // ones away — and it grew a third the day recording learned to pause.
    toolbar::scroll_content(ui, "controls_buttons", |ui| {
        show_buttons(ui, status, replay_save_key, i18n, actions);
    });
}

fn show_buttons(
    ui: &mut egui::Ui,
    status: &StatusSnapshot,
    replay_save_key: Option<&str>,
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

    // Above the recording buttons rather than below: a broadcast is the
    // thing a viewer is waiting for, and its button is the one to reach for
    // first. Both can run at once — they are separate branches off the same
    // two `Tee`s — so this is not a mode the recording buttons switch out of.
    ui.add_space(BUTTON_SPACING);
    let streaming = status.streaming_elapsed.is_some();
    let label = if streaming {
        TextKey::ControlStopStreaming
    } else {
        TextKey::ControlStartStreaming
    };
    if button(ui, i18n, label).clicked() {
        actions.push(if streaming {
            UiAction::StopStreaming
        } else {
            UiAction::StartStreaming
        });
    }

    // Only where it has been asked for in Settings — a buffer that is
    // running costs an encoder, and a button for it on every installation
    // would be a cost most people never meant to pay. Kept while one is
    // running even so, since that is the button that stops it.
    if status.replay_enabled || status.replay.is_some() {
        ui.add_space(BUTTON_SPACING);
        show_replay(ui, status, replay_save_key, i18n, actions);
    }

    ui.add_space(BUTTON_SPACING);
    if button(ui, i18n, TextKey::ControlSettings).clicked() {
        actions.push(UiAction::OpenSettings);
    }
}

/// The replay buffer's buttons: one to start it, and once it is running one
/// to stop it above the one that saves.
///
/// Stacked, like every other button here, rather than side by side: the
/// dock is a narrow column, and two labels this long in one row pushed the
/// second out of it. The key that saves is named under them, or its absence
/// is, since saving from a key inside a game is what a replay buffer is
/// mostly for.
fn show_replay(
    ui: &mut egui::Ui,
    status: &StatusSnapshot,
    replay_save_key: Option<&str>,
    i18n: &LocalizationManager,
    actions: &mut Vec<UiAction>,
) {
    let Some(fill) = status.replay else {
        if button(ui, i18n, TextKey::ControlStartReplay).clicked() {
            actions.push(UiAction::StartReplayBuffer);
        }
        return;
    };
    if button(ui, i18n, TextKey::ControlStopReplay).clicked() {
        actions.push(UiAction::StopReplayBuffer);
    }
    ui.add_space(BUTTON_SPACING);
    // Greyed until there is a clip worth saving — see
    // `ReplayFill::SAVEABLE_AFTER` — rather than answering a press with an
    // error or a file of one frame.
    let save = ui
        .add_enabled_ui(fill.saveable(), |ui| {
            button(ui, i18n, TextKey::ControlSaveReplay)
        })
        .inner;
    if save.clicked() {
        actions.push(UiAction::SaveReplay);
    }
    save.on_disabled_hover_text(i18n.text(TextKey::ControlReplayFilling));
    let hint = match replay_save_key {
        Some(key) => {
            let mut args = fluent_bundle::FluentArgs::new();
            args.set("key", key.to_owned());
            ui.add(
                egui::Label::new(
                    egui::RichText::new(i18n.text_with(TextKey::ControlReplayKey, &args))
                        .small()
                        .weak(),
                )
                .wrap(),
            );
            return;
        }
        None => i18n.text(TextKey::ControlReplayNoKey),
    };
    // Something to click rather than a sentence: the one thing to do about a
    // missing key is to go and set one. A wrapping label in the link colour
    // rather than a `Link`, which keeps to one line and ran out of the dock.
    let link = egui::Label::new(
        egui::RichText::new(hint)
            .small()
            .color(ui.visuals().hyperlink_color),
    )
    .wrap()
    .sense(egui::Sense::click());
    if ui
        .add(link)
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
    {
        actions.push(UiAction::OpenHotkeySettings);
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
