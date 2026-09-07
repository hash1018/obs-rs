//! The Streaming page.
//!
//! Read when a broadcast starts, so a change lands on the next one — the FLV
//! header goes out before the first frame and nothing in it can be
//! renegotiated after. The Recording page says the same thing about itself
//! and for the same reason.
//!
//! # The stream key
//!
//! Masked by default and revealed by asking, which is the whole of what this
//! page can do about it: the key is stored in plain text, as OBS stores its
//! own, and there is no keychain here to do better with. What the mask is
//! actually for is the ordinary case of someone screen-sharing or recording
//! their desktop with this window open — which, for this application in
//! particular, is not a remote possibility.

use eframe::egui;

use crate::i18n::{LocalizationManager, TextKey};
use crate::settings::{
    AUDIO_BIT_RATE_KBPS_RANGE, AppSettings, BIT_RATE_MBPS_RANGE, KEYFRAME_SECONDS_RANGE,
    RecordingAudioCodec, RecordingEncoder, STREAM_RECONNECT_CHOICES,
};

/// How long the key field stays revealed once the eye is clicked.
///
/// Revealed until it is clicked again, in fact — but the page is redrawn from
/// scratch whenever the dialog closes, so a key shown here is hidden again
/// the next time the dialog is opened. That is deliberate: an unmasked key
/// that survived a session would defeat the mask entirely.
#[derive(Default)]
pub(super) struct StreamingPageState {
    pub(super) key_revealed: bool,
}

/// Room for the reveal button, fixed so the field beside it does not change
/// width when the label does.
const REVEAL_WIDTH: f32 = 36.0;

pub(super) fn show(
    ui: &mut egui::Ui,
    draft: &mut AppSettings,
    state: &mut StreamingPageState,
    streaming: bool,
    encoders: &[RecordingEncoder],
    audio_codecs: &[RecordingAudioCodec],
    i18n: &LocalizationManager,
) {
    if streaming {
        // Stated rather than enforced by disabling the fields, as the
        // Recording page does: editing settings for the *next* broadcast
        // while one runs is a reasonable thing to be doing.
        ui.label(
            egui::RichText::new(i18n.text(TextKey::SettingsStreamingWhileRunning))
                .color(ui.visuals().warn_fg_color),
        );
        ui.add_space(8.0);
    }

    egui::Grid::new("settings_streaming")
        .num_columns(2)
        .spacing([12.0, 8.0])
        .show(ui, |ui| {
            ui.label(i18n.text(TextKey::SettingsStreamingServer));
            ui.add(
                egui::TextEdit::singleline(&mut draft.streaming.server)
                    .desired_width(f32::INFINITY)
                    .hint_text("rtmp://live.twitch.tv/app"),
            );
            ui.end_row();

            ui.label(i18n.text(TextKey::SettingsStreamingKey));
            ui.horizontal(|ui| {
                let width = (ui.available_width() - REVEAL_WIDTH).max(80.0);
                ui.add(
                    egui::TextEdit::singleline(&mut draft.streaming.stream_key)
                        .desired_width(width)
                        .password(!state.key_revealed),
                );
                let reveal = if state.key_revealed {
                    TextKey::SettingsStreamingKeyHide
                } else {
                    TextKey::SettingsStreamingKeyShow
                };
                if ui.button(i18n.text(reveal)).clicked() {
                    state.key_revealed = !state.key_revealed;
                }
            });
            ui.end_row();

            // Where the key ends up, said plainly rather than left to be
            // discovered. Someone deciding whether to paste a key into this
            // field is entitled to know that before they do, not after.
            ui.label("");
            ui.label(
                egui::RichText::new(i18n.text(TextKey::SettingsStreamingKeyStored))
                    .weak()
                    .small(),
            );
            ui.end_row();

            ui.label(i18n.text(TextKey::SettingsStreamingEncoder));
            ui.vertical(|ui| {
                egui::ComboBox::from_id_salt("settings_stream_encoder")
                    .selected_text(draft.streaming.encoder.label())
                    .show_ui(ui, |ui| {
                        // Every encoder, including the ones that would not
                        // open here — the Recording page's list explains why
                        // a missing entry is worse than a disabled one.
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
                                ui.selectable_value(&mut draft.streaming.encoder, encoder, label);
                            });
                        }
                    });
                if draft.streaming.encoder.is_software() {
                    ui.label(
                        egui::RichText::new(i18n.text(TextKey::SettingsEncoderSoftwareCost))
                            .color(ui.visuals().warn_fg_color)
                            .small(),
                    );
                }
            });
            ui.end_row();

            ui.label(i18n.text(TextKey::SettingsStreamingBitRate));
            ui.vertical(|ui| {
                ui.add(
                    egui::DragValue::new(&mut draft.streaming.bit_rate_mbps)
                        .range(BIT_RATE_MBPS_RANGE)
                        .suffix(" Mbps"),
                );
                // A recording is limited by the disk; a broadcast is limited
                // by the upstream link, which is the smaller of the two on
                // almost every connection and the one nobody measures.
                ui.label(
                    egui::RichText::new(i18n.text(TextKey::SettingsStreamingBitRateNote))
                        .weak()
                        .small(),
                );
            });
            ui.end_row();

            ui.label(i18n.text(TextKey::SettingsStreamingKeyframes));
            ui.add(
                egui::DragValue::new(&mut draft.streaming.keyframe_seconds)
                    .range(KEYFRAME_SECONDS_RANGE)
                    .suffix(" s"),
            );
            ui.end_row();

            ui.label(i18n.text(TextKey::SettingsStreamingAudioCodec));
            egui::ComboBox::from_id_salt("settings_stream_audio_codec")
                .selected_text(draft.streaming.audio_codec.label())
                .show_ui(ui, |ui| {
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
                            ui.selectable_value(&mut draft.streaming.audio_codec, codec, label);
                        });
                    }
                });
            ui.end_row();

            ui.label(i18n.text(TextKey::SettingsStreamingAudioBitRate));
            ui.add(
                egui::DragValue::new(&mut draft.streaming.audio_bit_rate_kbps)
                    .range(AUDIO_BIT_RATE_KBPS_RANGE)
                    .suffix(" kbps"),
            );
            ui.end_row();
            ui.label(i18n.text(TextKey::SettingsStreamingReconnect));
            egui::ComboBox::from_id_salt("settings_stream_reconnect")
                .selected_text(reconnect_label(draft.streaming.reconnect_seconds, i18n))
                .show_ui(ui, |ui| {
                    for choice in STREAM_RECONNECT_CHOICES {
                        let label = reconnect_label(choice, i18n);
                        ui.selectable_value(&mut draft.streaming.reconnect_seconds, choice, label);
                    }
                });
            ui.end_row();
        });
}

/// How the reconnect choices read. `None` is not "zero seconds" but "leave
/// it to me", so it is worded rather than numbered.
fn reconnect_label(seconds: Option<u32>, i18n: &LocalizationManager) -> String {
    match seconds {
        None => i18n
            .text(TextKey::SettingsStreamingReconnectNever)
            .into_owned(),
        Some(seconds) => {
            let mut args = fluent_bundle::FluentArgs::new();
            args.set("seconds", seconds);
            i18n.text_with(TextKey::SettingsStreamingReconnectAfter, &args)
                .into_owned()
        }
    }
}
