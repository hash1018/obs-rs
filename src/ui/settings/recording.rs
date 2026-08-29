//! The Recording page.
//!
//! Everything here is read when a recording starts, so a change lands on the
//! next one rather than the running one — an mp4's header is written before
//! its first frame and nothing in it can be renegotiated after. The page says
//! so while a recording is running rather than pretending otherwise.

use eframe::egui;
use time::OffsetDateTime;

use crate::i18n::{LocalizationManager, TextKey};
use crate::settings::{AppSettings, BIT_RATE_MBPS_RANGE, KEYFRAME_SECONDS_RANGE};

pub(super) fn show(
    ui: &mut egui::Ui,
    draft: &mut AppSettings,
    recording: bool,
    i18n: &LocalizationManager,
) {
    if recording {
        // Stated, not enforced by disabling the fields: the settings are for
        // the *next* recording, so editing them now is a reasonable thing to
        // be doing. What would be unreasonable is expecting the running file
        // to change.
        ui.label(
            egui::RichText::new(i18n.text(TextKey::SettingsRecordingWhileRunning))
                .color(ui.visuals().warn_fg_color),
        );
        ui.add_space(8.0);
    }

    egui::Grid::new("settings_recording")
        .num_columns(2)
        .spacing([12.0, 8.0])
        .show(ui, |ui| {
            ui.label(i18n.text(TextKey::SettingsRecordingDirectory));
            ui.add(
                egui::TextEdit::singleline(&mut draft.recording.directory)
                    .desired_width(f32::INFINITY)
                    .hint_text(i18n.text(TextKey::SettingsRecordingDirectoryHint)),
            );
            ui.end_row();

            ui.label(i18n.text(TextKey::SettingsRecordingNamePrefix));
            ui.add(
                egui::TextEdit::singleline(&mut draft.recording.name_prefix)
                    .desired_width(f32::INFINITY),
            );
            ui.end_row();

            // What the two fields above actually produce. A prefix and a
            // timestamp format described in prose would leave the reader to
            // imagine the result; this is the result.
            ui.label(i18n.text(TextKey::SettingsRecordingNameExample));
            // Elided rather than wrapped, with the whole of it on hover — the
            // same way the status bar shows a long failure. A path is read
            // from its end, and one that reflowed the grid as it was typed
            // would move every row under it.
            let example = example_path(&draft.recording);
            ui.add(
                egui::Label::new(egui::RichText::new(&example).monospace().weak())
                    .truncate(),
            )
            .on_hover_text(&example);
            ui.end_row();

            ui.label(i18n.text(TextKey::SettingsRecordingBitRate));
            ui.add(
                egui::DragValue::new(&mut draft.recording.bit_rate_mbps)
                    .range(BIT_RATE_MBPS_RANGE)
                    .suffix(" Mbps"),
            );
            ui.end_row();

            ui.label(i18n.text(TextKey::SettingsRecordingKeyframes));
            ui.add(
                egui::DragValue::new(&mut draft.recording.keyframe_seconds)
                    .range(KEYFRAME_SECONDS_RANGE)
                    .suffix(" s"),
            );
            ui.end_row();
        });
}

/// The file the next recording would be written to, as a whole path.
///
/// Built through the same function the engine uses, so the example cannot
/// drift from what actually happens — including the fallbacks an empty field
/// takes.
fn example_path(settings: &crate::settings::RecordingSettings) -> String {
    let started = OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc());
    crate::paths::recording_file_in(
        &settings.directory_or_default(),
        settings.prefix_or_default(),
        started,
    )
    .display()
    .to_string()
}
