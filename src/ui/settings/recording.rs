//! The Recording page.
//!
//! Everything here is read when a recording starts, so a change lands on the
//! next one rather than the running one — an mp4's header is written before
//! its first frame and nothing in it can be renegotiated after. The page says
//! so while a recording is running rather than pretending otherwise.

use eframe::egui;
use time::OffsetDateTime;

use crate::i18n::{LocalizationManager, TextKey};
use crate::settings::{
    AUDIO_BIT_RATE_KBPS_RANGE, AppSettings, BIT_RATE_MBPS_RANGE, KEYFRAME_SECONDS_RANGE,
    RecordingAudioCodec, RecordingEncoder, RecordingFormat, RecordingSplit, SPLIT_MEGABYTES_RANGE,
    SPLIT_MINUTES_RANGE,
};

/// Room for "Browse…" in either language, fixed so the field beside it does
/// not change width when the language does.
const BROWSE_WIDTH: f32 = 84.0;

/// Returns whether the folder picker was asked for. Reported rather than
/// opened here, because opening one needs the dialog's own state — this page
/// is handed nothing but the draft.
pub(super) fn show(
    ui: &mut egui::Ui,
    draft: &mut AppSettings,
    recording: bool,
    picking: bool,
    encoders: &[RecordingEncoder],
    audio_codecs: &[RecordingAudioCodec],
    i18n: &LocalizationManager,
) -> bool {
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

    let mut browse = false;
    egui::Grid::new("settings_recording")
        .num_columns(2)
        .spacing([12.0, 8.0])
        .show(ui, |ui| {
            ui.label(i18n.text(TextKey::SettingsRecordingDirectory));
            ui.horizontal(|ui| {
                // The field is sized from what is left after the button
                // rather than allowed to grow, so the button cannot be pushed
                // off the end of the row by a long path.
                let field = ui.available_width() - BROWSE_WIDTH - ui.spacing().item_spacing.x;
                // Shown, not typed. A path is picked from the dialog beside
                // it, which is the only way to know the folder exists and can
                // be written before a recording depends on it — a typed one
                // is only found out about when the button is pressed.
                //
                // A `&str` buffer rather than a disabled field: egui treats
                // that as read-only and still lets the text be selected and
                // copied, which is most of what anyone wants a path for.
                //
                // What it shows is the folder in force, so an unset one reads
                // as the default it resolves to rather than as blank.
                let shown = draft.recording.directory_or_default();
                let mut shown = shown.display().to_string();
                let mut shown: &str = &mut shown;
                ui.add_sized(
                    [field.max(80.0), ui.spacing().interact_size.y],
                    egui::TextEdit::singleline(&mut shown),
                )
                .on_hover_text(shown);
                // Disabled while one is open: the desktop's picker is a
                // separate window, and a second would sit behind the first
                // with nothing to say it was there.
                if ui
                    .add_enabled_ui(!picking, |ui| {
                        ui.add_sized(
                            [BROWSE_WIDTH, ui.spacing().interact_size.y],
                            egui::Button::new(i18n.text(TextKey::ActionBrowse)),
                        )
                    })
                    .inner
                    .clicked()
                {
                    browse = true;
                }
            });
            ui.end_row();

            ui.label(i18n.text(TextKey::SettingsRecordingNamePrefix));
            ui.add(
                egui::TextEdit::singleline(&mut draft.recording.name_prefix)
                    .desired_width(f32::INFINITY),
            );
            ui.end_row();

            // Above the example rather than below it, because it is what
            // decides the extension the example ends in — and, for HLS, that
            // the example is a playlist inside a folder of its own.
            ui.label(i18n.text(TextKey::SettingsRecordingFormat));
            egui::ComboBox::from_id_salt("settings_recording_format")
                .selected_text(draft.recording.format.label())
                .show_ui(ui, |ui| {
                    for format in RecordingFormat::ALL {
                        ui.selectable_value(&mut draft.recording.format, format, format.label());
                    }
                });
            ui.end_row();

            ui.label(i18n.text(TextKey::SettingsRecordingSplit));
            // Disabled for a format that segments itself, which is what
            // `effective_split` already decides for the recording — the
            // control is greyed so the two cannot appear to disagree.
            let splittable = !draft.recording.format.segments_itself();
            ui.add_enabled_ui(splittable, |ui| {
                ui.horizontal(|ui| {
                    let combo = egui::ComboBox::from_id_salt("settings_recording_split")
                        .selected_text(i18n.text(split_key(draft.recording.split)))
                        .show_ui(ui, |ui| {
                            for split in RecordingSplit::ALL {
                                ui.selectable_value(
                                    &mut draft.recording.split,
                                    split,
                                    i18n.text(split_key(split)),
                                );
                            }
                        });
                    if !splittable {
                        combo
                            .response
                            .on_disabled_hover_text(i18n.text(TextKey::SettingsRecordingSplitHls));
                    }
                    // Only the figure the chosen policy actually reads. Two
                    // fields, one of which does nothing, is two things to
                    // check when a recording split somewhere unexpected.
                    match draft.recording.split {
                        RecordingSplit::Off => {}
                        RecordingSplit::Time => {
                            ui.add(
                                egui::DragValue::new(&mut draft.recording.split_minutes)
                                    .range(SPLIT_MINUTES_RANGE)
                                    .suffix(" min"),
                            );
                        }
                        RecordingSplit::Size => {
                            ui.add(
                                egui::DragValue::new(&mut draft.recording.split_megabytes)
                                    .range(SPLIT_MEGABYTES_RANGE)
                                    .suffix(" MB"),
                            );
                        }
                    }
                });
            });
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
            ui.add(egui::Label::new(egui::RichText::new(&example).monospace().weak()).truncate())
                .on_hover_text(&example);
            ui.end_row();

            ui.label(i18n.text(TextKey::SettingsRecordingEncoder));
            ui.vertical(|ui| {
                egui::ComboBox::from_id_salt("settings_encoder")
                    .selected_text(draft.recording.encoder.label())
                    .show_ui(ui, |ui| {
                        // Every encoder, not only the ones that opened —
                        // "libx264 is not in this build" is worth reading, and
                        // an entry that silently vanished would leave someone
                        // looking for it with nothing to find.
                        for encoder in RecordingEncoder::ALL {
                            let available = encoders.contains(&encoder);
                            ui.add_enabled_ui(available, |ui| {
                                let label = if available {
                                    encoder.label().to_owned()
                                } else {
                                    format!(
                                        "{} — {}",
                                        encoder.label(),
                                        i18n.text(TextKey::SettingsEncoderUnavailable)
                                    )
                                };
                                ui.selectable_value(&mut draft.recording.encoder, encoder, label);
                            });
                        }
                    });
                if draft.recording.encoder.is_software() {
                    ui.label(
                        egui::RichText::new(i18n.text(TextKey::SettingsEncoderSoftwareCost))
                            .color(ui.visuals().warn_fg_color)
                            .small(),
                    );
                }
            });
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

            ui.label(i18n.text(TextKey::SettingsRecordingAudioCodec));
            egui::ComboBox::from_id_salt("settings_audio_codec")
                .selected_text(draft.recording.audio_codec.label())
                .show_ui(ui, |ui| {
                    // Every codec, not only the ones that opened — the same
                    // reasoning as the video list above: "libopus is not in
                    // this build" is worth reading, and an entry that
                    // silently vanished leaves someone looking for it.
                    for codec in RecordingAudioCodec::ALL {
                        let available = audio_codecs.contains(&codec);
                        ui.add_enabled_ui(available, |ui| {
                            let label = if available {
                                codec.label().to_owned()
                            } else {
                                format!(
                                    "{} — {}",
                                    codec.label(),
                                    i18n.text(TextKey::SettingsEncoderUnavailable)
                                )
                            };
                            ui.selectable_value(&mut draft.recording.audio_codec, codec, label);
                        });
                    }
                });
            ui.end_row();

            ui.label(i18n.text(TextKey::SettingsRecordingAudioBitRate));
            ui.add(
                egui::DragValue::new(&mut draft.recording.audio_bit_rate_kbps)
                    .range(AUDIO_BIT_RATE_KBPS_RANGE)
                    .suffix(" kbps"),
            );
            ui.end_row();
        });
    browse
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
        settings.format,
    )
    .display()
    .to_string()
}

/// What to call a split policy in the list.
fn split_key(split: RecordingSplit) -> TextKey {
    match split {
        RecordingSplit::Off => TextKey::SettingsRecordingSplitOff,
        RecordingSplit::Time => TextKey::SettingsRecordingSplitTime,
        RecordingSplit::Size => TextKey::SettingsRecordingSplitSize,
    }
}
