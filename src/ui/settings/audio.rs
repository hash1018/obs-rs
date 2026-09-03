//! The Audio page: what the mixer sums into.
//!
//! Not a recording setting, which is why it is not on that page: the mixer
//! runs whether or not anything is recording, and the level meters in the
//! Audio Mixer dock are reading its output either way.
//!
//! # The format is locked while recording, monitoring is not
//!
//! Unlike the Recording page, the format here does not wait for the next
//! file. The mixer takes a new one at its next tick, and the running
//! recording's audio encoder was opened for the old one — a sample rate it is
//! not expecting, or a channel count it cannot lay out. So those controls are
//! disabled while one runs, the same as the frame rate and for the same
//! reason.
//!
//! The monitoring endpoint is not, because it reaches no encoder. What the
//! person at the keyboard is listening to is theirs to change while they
//! work, recording or not.

use eframe::egui;

use crate::capture::AudioDeviceTarget;
use crate::domain::AudioSourceKind;
use crate::i18n::{LocalizationManager, TextKey};
use crate::settings::{AppSettings, CHANNEL_CHOICES, SAMPLE_RATE_CHOICES};
use crate::snapshots::AudioSnapshot;

/// The page's own `Ui` runs left to right — the dialog puts the page list and
/// the page beside each other, and a page inherits that. A `Grid` lays itself
/// out either way, but a plain label after one would be placed to the *right*
/// of it rather than under it, which is where the warnings below were
/// landing. So the page is a column of its own.
pub(super) fn show(
    ui: &mut egui::Ui,
    draft: &mut AppSettings,
    recording: bool,
    devices: &[AudioDeviceTarget],
    audio: &AudioSnapshot,
    i18n: &LocalizationManager,
) {
    ui.vertical(|ui| page(ui, draft, recording, devices, audio, i18n));
}

fn page(
    ui: &mut egui::Ui,
    draft: &mut AppSettings,
    recording: bool,
    devices: &[AudioDeviceTarget],
    audio: &AudioSnapshot,
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

            ui.label(i18n.text(TextKey::SettingsAudioMonitorDevice));
            let none = i18n.text(TextKey::SettingsAudioMonitorNone).into_owned();
            let selected = draft
                .audio
                .monitor_device
                .as_deref()
                .and_then(|chosen| playback(devices).find(|device| device.id == chosen))
                .map(|device| device.name.clone())
                // A stored endpoint that is no longer here still shows its own
                // id rather than reading as "None": it is what the setting
                // says, it comes back when the device is plugged in again, and
                // showing nothing would invite someone to set it a second time.
                .or_else(|| draft.audio.monitor_device.clone())
                .unwrap_or_else(|| none.clone());
            egui::ComboBox::from_id_salt("settings_monitor_device")
                .selected_text(selected)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut draft.audio.monitor_device, None, none);
                    for device in playback(devices) {
                        ui.selectable_value(
                            &mut draft.audio.monitor_device,
                            Some(device.id.clone()),
                            &device.name,
                        );
                    }
                });
            ui.end_row();
        });

    // The one mistake this setting exists to prevent, said where it is being
    // made. Desktop Audio is captured by listening to a playback endpoint, so
    // monitoring into that same endpoint hands the capture its own output —
    // and every pass round adds delay and gain.
    if let Some(chosen) = draft.audio.monitor_device.as_deref()
        && captures_endpoint(audio, devices, chosen)
    {
        warning(ui, i18n.text(TextKey::SettingsAudioMonitorFeedback));
    }

    // Only where it actually bites. `libopus` takes 48/24/16/12/8 kHz and
    // nothing else, so at 44.1 it drops out of the Recording page's encoder
    // list — which is a confusing thing to discover on a different page
    // without having been told here.
    if !SAMPLE_RATE_CHOICES
        .iter()
        .take(1)
        .any(|rate| *rate == draft.audio.sample_rate)
    {
        warning(ui, i18n.text(TextKey::SettingsAudioOpusNeeds48k));
    }
}

/// A line under the controls, in the theme's warning colour.
///
/// Bounded to the page's own width so a sentence longer than that wraps
/// instead of widening the dialog around it. The column [`show`] puts the
/// page in inherits the room the page was allocated, but a scroll area does
/// not clip what overflows it sideways.
fn warning(ui: &mut egui::Ui, text: std::borrow::Cow<'_, str>) {
    ui.add_space(8.0);
    ui.scope(|ui| {
        ui.set_max_width(super::PAGE_WIDTH);
        ui.label(
            egui::RichText::new(text)
                .color(ui.visuals().warn_fg_color)
                .small(),
        );
    });
}

/// The endpoints something can be played to.
fn playback(devices: &[AudioDeviceTarget]) -> impl Iterator<Item = &AudioDeviceTarget> {
    devices
        .iter()
        .filter(|device| device.kind == AudioSourceKind::Output)
}

/// Whether any running Desktop Audio source is listening to `endpoint`.
///
/// A source that stored no endpoint follows the system default, so that
/// counts too — which is exactly the case somebody who never opened the
/// picker is in, and therefore the one most likely to be walked into.
fn captures_endpoint(audio: &AudioSnapshot, devices: &[AudioDeviceTarget], endpoint: &str) -> bool {
    let default = playback(devices)
        .find(|device| device.is_default)
        .map(|device| device.id.as_str());
    audio
        .items
        .iter()
        .filter(|source| source.kind == AudioSourceKind::Output)
        .any(|source| source.device.as_deref().or(default) == Some(endpoint))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::AudioSourceId;
    use crate::snapshots::AudioSourceSnapshot;

    fn device(id: &str, kind: AudioSourceKind, is_default: bool) -> AudioDeviceTarget {
        AudioDeviceTarget {
            id: id.to_owned(),
            name: id.to_owned(),
            kind,
            is_default,
        }
    }

    fn desktop_audio(listening_to: Option<&str>) -> AudioSnapshot {
        AudioSnapshot {
            items: vec![AudioSourceSnapshot {
                id: AudioSourceId(1),
                name: "Desktop Audio".to_owned(),
                kind: AudioSourceKind::Output,
                device: listening_to.map(str::to_owned),
                gain_db: 0.0,
                muted: false,
                monitored: false,
                peak_db: None,
                running: true,
            }],
        }
    }

    /// The case the warning is really for. A Desktop Audio source that was
    /// never pointed at anything follows the system default, so choosing that
    /// default to monitor through is the loop — and it is the choice somebody
    /// who has not thought about this makes.
    #[test]
    fn a_source_following_the_default_still_claims_that_endpoint() {
        let devices = [
            device("speakers", AudioSourceKind::Output, true),
            device("headphones", AudioSourceKind::Output, false),
        ];

        assert!(captures_endpoint(
            &desktop_audio(None),
            &devices,
            "speakers"
        ));
        assert!(!captures_endpoint(
            &desktop_audio(None),
            &devices,
            "headphones"
        ));
    }

    /// And one that names an endpoint claims that one instead, which is what
    /// makes monitoring through the default safe again.
    #[test]
    fn a_source_that_named_an_endpoint_claims_only_that_one() {
        let devices = [
            device("speakers", AudioSourceKind::Output, true),
            device("headphones", AudioSourceKind::Output, false),
        ];

        assert!(captures_endpoint(
            &desktop_audio(Some("headphones")),
            &devices,
            "headphones"
        ));
        assert!(!captures_endpoint(
            &desktop_audio(Some("headphones")),
            &devices,
            "speakers"
        ));
    }

    /// A microphone is opened on the device itself rather than by listening
    /// to what it plays, so monitoring through it is not a loop and must not
    /// be warned about.
    #[test]
    fn an_input_source_claims_no_playback_endpoint() {
        let devices = [device("headset", AudioSourceKind::Input, true)];
        let mut audio = desktop_audio(Some("headset"));
        audio.items[0].kind = AudioSourceKind::Input;

        assert!(!captures_endpoint(&audio, &devices, "headset"));
    }
}
