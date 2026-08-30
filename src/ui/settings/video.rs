//! The Video page: what the compositor makes, and what size a recording is
//! written at.
//!
//! The two are one page because the second is expressed relative to the
//! first — an output resolution is a fraction of the Scene Canvas, not a free
//! pair of numbers — and reading them apart would leave the choices here
//! looking arbitrary.
//!
//! # The canvas is shown, not offered
//!
//! `SceneCanvas::DEFAULT` is a constant. It is on this page anyway because it
//! is what the output resolution below is measured against, and a page
//! offering "1280x720" without saying what that is smaller *than* is asking
//! the reader to already know.

use eframe::egui;

use crate::domain::SceneCanvas;
use crate::i18n::{LocalizationManager, TextKey};
use crate::settings::{AppSettings, FPS_CHOICES, output_heights};

pub(super) fn show(
    ui: &mut egui::Ui,
    draft: &mut AppSettings,
    recording: bool,
    i18n: &LocalizationManager,
) {
    let canvas = SceneCanvas::DEFAULT;
    let canvas_size = [canvas.width as u32, canvas.height as u32];

    egui::Grid::new("settings_video")
        .num_columns(2)
        .spacing([12.0, 8.0])
        .show(ui, |ui| {
            ui.label(i18n.text(TextKey::SettingsVideoCanvas));
            ui.label(
                egui::RichText::new(format!("{} × {}", canvas_size[0], canvas_size[1])).monospace(),
            )
            .on_hover_text(i18n.text(TextKey::SettingsVideoCanvasFixed));
            ui.end_row();

            ui.label(i18n.text(TextKey::SettingsVideoOutput));
            let [output_width, output_height] = draft.recording.output_size(canvas_size);
            egui::ComboBox::from_id_salt("settings_output_size")
                .selected_text(format!("{output_width} × {output_height}"))
                .show_ui(ui, |ui| {
                    // Derived from the canvas rather than a fixed list, so
                    // every entry is a size the compositor can actually be
                    // scaled to — see `settings::output_heights`.
                    for height in output_heights(canvas_size[1]) {
                        let [width, height] = with_height(draft, canvas_size, height);
                        ui.selectable_value(
                            &mut draft.recording.output_height,
                            height,
                            format!("{width} × {height}"),
                        );
                    }
                });
            ui.end_row();

            ui.label(i18n.text(TextKey::SettingsVideoFps));
            // Disabled while a recording runs. Unlike a size, which only the
            // next file is written at, this one is the compositor's own rate
            // and takes immediately — which the running file's encoder was
            // not configured for.
            ui.add_enabled_ui(!recording, |ui| {
                let combo = egui::ComboBox::from_id_salt("settings_fps")
                    .selected_text(format!("{} fps", draft.recording.fps))
                    .show_ui(ui, |ui| {
                        // A list rather than a free number: the compositor is
                        // built to produce one of these, and an encoder is
                        // configured for exactly what it is given.
                        for fps in FPS_CHOICES {
                            ui.selectable_value(
                                &mut draft.recording.fps,
                                fps,
                                format!("{fps} fps"),
                            );
                        }
                    });
                if recording {
                    combo
                        .response
                        .on_disabled_hover_text(i18n.text(TextKey::SettingsFpsWhileRecording));
                }
            });
            ui.end_row();
        });
}

/// What `height` would come out as, so an entry is labelled with the size it
/// would actually produce rather than with the height alone.
///
/// Asked of the draft with the height substituted, so the rounding this
/// answers with is the same rounding the recording will use — two places
/// deciding a width is two places to disagree about one.
fn with_height(draft: &AppSettings, canvas: [u32; 2], height: u32) -> [u32; 2] {
    let candidate = crate::settings::RecordingSettings {
        output_height: height,
        ..draft.recording.clone()
    };
    candidate.output_size(canvas)
}
