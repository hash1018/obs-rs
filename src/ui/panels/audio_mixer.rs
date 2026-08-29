//! The dock the audio sources' faders live in.
//!
//! Lists what the project holds rather than what the selected Scene does:
//! audio is global here, so this dock does not change when Scenes do.

use eframe::egui;

use crate::domain::{AudioSourceKind, MAX_GAIN_DB, MIN_GAIN_DB};
use crate::i18n::{LocalizationManager, TextKey};
use crate::project::{AudioCommand, ProjectCommand};
use crate::snapshots::{AudioSnapshot, AudioSourceSnapshot};

use super::super::UiAction;

/// The meter's own height. Thin, because it is read as a level rather than
/// looked at — the fader below it is what the pointer wants.
const METER_HEIGHT: f32 = 8.0;
const ROW_SPACING: f32 = 10.0;

pub(in crate::ui) fn show(
    ui: &mut egui::Ui,
    snapshot: &AudioSnapshot,
    i18n: &LocalizationManager,
    actions: &mut Vec<UiAction>,
) {
    if snapshot.items.is_empty() {
        ui.centered_and_justified(|ui| {
            ui.weak(i18n.text(TextKey::AudioEmpty));
        });
        return;
    }

    egui::ScrollArea::vertical()
        .id_salt("audio_mixer_list")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for (index, source) in snapshot.items.iter().enumerate() {
                if index > 0 {
                    ui.add_space(ROW_SPACING);
                }
                // Scoped per source rather than per widget: every row holds
                // the same three controls, so without this they would collide
                // on ids derived from their position alone.
                ui.push_id(source.id.0, |ui| show_source(ui, source, i18n, actions));
            }
        });
}

fn show_source(
    ui: &mut egui::Ui,
    source: &AudioSourceSnapshot,
    i18n: &LocalizationManager,
    actions: &mut Vec<UiAction>,
) {
    ui.horizontal(|ui| {
        // What this row is listening to, on the name's own line rather than
        // under it: four docks share this column, and a line per source is
        // one the mixer cannot spare. "Default" is not nothing — it says the
        // row follows whatever the system calls its default rather than
        // being pointed at one device.
        let kind = i18n.text(match source.kind {
            AudioSourceKind::Output => TextKey::AudioKindOutput,
            AudioSourceKind::Input => TextKey::AudioKindInput,
        });
        let device = source
            .device
            .as_deref()
            .map(str::to_owned)
            .unwrap_or_else(|| i18n.text(TextKey::AudioDeviceDefault).into_owned());
        ui.strong(&source.name)
            .on_hover_text(format!("{kind} · {device}"));

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let label = if source.muted {
                TextKey::AudioUnmute
            } else {
                TextKey::AudioMute
            };
            if ui.button(i18n.text(label)).clicked() {
                actions.push(audio_action(AudioCommand::SetMuted(
                    source.id,
                    !source.muted,
                )));
            }
            ui.monospace(format_gain(source.gain_db));
        });
    });

    show_meter(ui, source);

    // The fader stays live while muted rather than greying out: muting is not
    // meant to lose the level somebody set, and a fader that cannot be moved
    // until unmuted makes setting it a two-step job.
    let mut gain_db = source.gain_db;
    let fader =
        ui.add(egui::Slider::new(&mut gain_db, MIN_GAIN_DB..=MAX_GAIN_DB).show_value(false));
    // On release, not on every pixel of the drag: a gesture is one edit, the
    // same rule the Preview's own drag follows.
    if fader.drag_stopped() || (fader.changed() && !fader.dragged()) {
        actions.push(audio_action(AudioCommand::SetGainDb(source.id, gain_db)));
    }
}

/// The level bar.
///
/// Drawn even with nothing behind it, at the width and height it will keep:
/// what arrives later is a number, not a layout. An unmeasured source shows
/// the empty channel rather than a full one, which is also what silence looks
/// like — the two are indistinguishable here, and saying so is
/// [`AudioSourceSnapshot::peak_db`]'s job rather than this one's.
fn show_meter(ui: &mut egui::Ui, source: &AudioSourceSnapshot) {
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), METER_HEIGHT),
        egui::Sense::hover(),
    );
    let painter = ui.painter();
    let rounding = egui::CornerRadius::same(2);
    painter.rect_filled(rect, rounding, ui.visuals().extreme_bg_color);
    // Outlined, or an empty channel is the same tone as the dock behind it
    // and reads as a divider rather than as a meter with nothing in it.
    painter.rect_stroke(
        rect,
        rounding,
        ui.visuals().widgets.noninteractive.bg_stroke,
        egui::StrokeKind::Inside,
    );

    let filled = match source.peak_db {
        // Muted is silence whatever the source is doing, so the meter says so
        // rather than showing a level that is not reaching anything.
        Some(_) if source.muted => 0.0,
        Some(peak_db) => ((peak_db - MIN_GAIN_DB) / -MIN_GAIN_DB).clamp(0.0, 1.0),
        None => 0.0,
    };
    if filled > 0.0 {
        let mut bar = rect;
        bar.set_width(rect.width() * filled);
        painter.rect_filled(bar, rounding, ui.visuals().selection.bg_fill);
    }
}

/// Signed, so that boosting is visibly different from cutting at a glance,
/// and fixed-width so the row does not reflow as the fader moves.
fn format_gain(gain_db: f32) -> String {
    if gain_db <= MIN_GAIN_DB {
        return format!("{:>7}", "-∞ dB");
    }
    format!("{gain_db:>+5.1} dB")
}

fn audio_action(command: AudioCommand) -> UiAction {
    UiAction::Project(ProjectCommand::Audio(command))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_quietest_the_fader_goes_reads_as_silence() {
        assert_eq!(format_gain(MIN_GAIN_DB).trim(), "-∞ dB");
        assert_eq!(format_gain(0.0).trim(), "+0.0 dB");
        assert_eq!(format_gain(-12.5).trim(), "-12.5 dB");
    }
}
