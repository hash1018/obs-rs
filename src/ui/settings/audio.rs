//! The Audio page: what the mixer sums into.
//!
//! Not a recording setting, which is why it is not on that page: the mixer
//! runs whether or not anything is recording, and the level meters in the
//! Audio Mixer dock are reading its output either way.
//!
//! # Both are locked while recording
//!
//! Unlike the Recording page, nothing here waits for the next file. The mixer
//! takes a new format at its next tick, and the running recording's audio
//! encoder was opened for the old one — a sample rate it is not expecting, or
//! a channel count it cannot lay out. So the controls are disabled while one
//! runs, the same as the frame rate and for the same reason.

use eframe::egui;

use crate::i18n::{LocalizationManager, TextKey};
use crate::settings::{AppSettings, CHANNEL_CHOICES, SAMPLE_RATE_CHOICES};

pub(super) fn show(
    ui: &mut egui::Ui,
    draft: &mut AppSettings,
    recording: bool,
    i18n: &LocalizationManager,
) {
    egui::Grid::new("settings_audio")
        .num_columns(2)
        .spacing([12.0, 8.0])
        .show(ui, |ui| {
            ui.label(i18n.text(TextKey::SettingsAudioSampleRate));
            ui.add_enabled_ui(!recording, |ui| {
                let combo = egui::ComboBox::from_id_salt("settings_sample_rate")
                    .selected_text(sample_rate_label(draft.audio.sample_rate))
                    .show_ui(ui, |ui| {
                        for rate in SAMPLE_RATE_CHOICES {
                            ui.selectable_value(
                                &mut draft.audio.sample_rate,
                                rate,
                                sample_rate_label(rate),
                            );
                        }
                    });
                if recording {
                    combo
                        .response
                        .on_disabled_hover_text(i18n.text(TextKey::SettingsAudioWhileRecording));
                }
            });
            ui.end_row();

            ui.label(i18n.text(TextKey::SettingsAudioChannels));
            ui.add_enabled_ui(!recording, |ui| {
                let combo = egui::ComboBox::from_id_salt("settings_channels")
                    .selected_text(i18n.text(channel_key(draft.audio.channels)))
                    .show_ui(ui, |ui| {
                        for channels in CHANNEL_CHOICES {
                            ui.selectable_value(
                                &mut draft.audio.channels,
                                channels,
                                i18n.text(channel_key(channels)),
                            );
                        }
                    });
                if recording {
                    combo
                        .response
                        .on_disabled_hover_text(i18n.text(TextKey::SettingsAudioWhileRecording));
                }
            });
            ui.end_row();
        });

    // Only where it actually bites. `libopus` takes 48/24/16/12/8 kHz and
    // nothing else, so at 44.1 it drops out of the Recording page's encoder
    // list — which is a confusing thing to discover on a different page
    // without having been told here.
    if !SAMPLE_RATE_CHOICES
        .iter()
        .take(1)
        .any(|rate| *rate == draft.audio.sample_rate)
    {
        ui.add_space(8.0);
        ui.label(
            egui::RichText::new(i18n.text(TextKey::SettingsAudioOpusNeeds48k))
                .color(ui.visuals().warn_fg_color)
                .small(),
        );
    }
}

/// Kilohertz with one decimal where it has one — 44.1, but 48 rather than
/// 48.0.
fn sample_rate_label(rate: u32) -> String {
    if rate.is_multiple_of(1000) {
        format!("{} kHz", rate / 1000)
    } else {
        format!("{:.1} kHz", rate as f32 / 1000.0)
    }
}

fn channel_key(channels: u16) -> TextKey {
    match channels {
        1 => TextKey::SettingsAudioMono,
        _ => TextKey::SettingsAudioStereo,
    }
}
