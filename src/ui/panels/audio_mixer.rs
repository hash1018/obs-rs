//! The dock the audio sources' faders live in.
//!
//! Lists what the project holds rather than what the selected Scene does:
//! audio is global here, so this dock does not change when Scenes do.
//!
//! # Why the channels stand up
//!
//! A fader is read against a scale, and a level is read against a scale. Both
//! run from silence to unity here, so standing them side by side puts them on
//! one axis with one set of numbers between them — which is how every mixer is
//! built, and why a horizontal row ends up with a meter that has no scale
//! beside it at all. See [`crate::domain::MAX_GAIN_DB`] for why the fader
//! stops where the meter does.
//!
//! It also costs a fixed width per source rather than a fixed height, so a
//! dock with room shows more sources instead of more padding.

use eframe::egui;

use crate::capture::AudioDeviceTarget;
use crate::domain::{AudioSourceKind, MAX_GAIN_DB, MIN_GAIN_DB};
use crate::i18n::{LocalizationManager, TextKey};
use crate::project::{AudioCommand, ProjectCommand};
use crate::snapshots::{AudioSnapshot, AudioSourceSnapshot};

use super::super::UiAction;

/// One source's column: a fader, a meter, the scale between them, and a name
/// wide enough not to wrap onto a second line — which would push the channel
/// down and leave two columns disagreeing about where their meters start. The
/// dock shows as many as it has room for and scrolls past the rest.
const SOURCE_WIDTH: f32 = 112.0;
const METER_WIDTH: f32 = 9.0;
/// Room for "-60", the longest label on the scale.
const SCALE_WIDTH: f32 = 22.0;
/// What a column spends on everything that is not the channel: the name, the
/// readout, the mute button, and the gaps between them.
const FIXED_ROW_HEIGHT: f32 = 76.0;
/// The shortest a channel may be squeezed to. Below this the scale's labels
/// collide and the fader stops being worth dragging.
const MIN_CHANNEL_HEIGHT: f32 = 96.0;
const MUTE_HEIGHT: f32 = 22.0;

/// Every label on the scale, in decibels. Every 12 rather than the 6 a large
/// mixer uses: this dock is drawn at whatever height it is given, and half as
/// many marks stay legible when that is not much.
const SCALE_MARKS: [f32; 6] = [0.0, -12.0, -24.0, -36.0, -48.0, -60.0];

/// Where the meter stops being green, and where it starts warning of a clip.
const WARN_DB: f32 = -20.0;
const CLIP_DB: f32 = -9.0;

pub(in crate::ui) fn show(
    ui: &mut egui::Ui,
    snapshot: &AudioSnapshot,
    devices: &[AudioDeviceTarget],
    i18n: &LocalizationManager,
    actions: &mut Vec<UiAction>,
) {
    if snapshot.items.is_empty() {
        ui.centered_and_justified(|ui| {
            ui.weak(i18n.text(TextKey::AudioEmpty));
        });
        return;
    }

    // Given its own `Ui`, bounded and clipped to this pane. A scroll area
    // told not to auto-shrink takes the whole height of what it is handed,
    // and in a squeezed pane that is more than the pane has — so it believed
    // everything fit, showed no scrollbar, and let the dock's clipping cut a
    // channel off mid-fader. Handing it the real rectangle is what makes it
    // scroll instead.
    let viewport = ui.available_rect_before_wrap().intersect(ui.max_rect());
    let mut area = ui.new_child(
        egui::UiBuilder::new()
            .id_salt("audio_mixer_viewport")
            .max_rect(viewport)
            .layout(egui::Layout::top_down(egui::Align::LEFT)),
    );
    area.set_clip_rect(viewport);
    let ui = &mut area;
    // A solid bar rather than egui's default floating one, which is drawn
    // over the content only while the pointer is inside it. In a dock this
    // narrow that means a channel visibly cut off with nothing to say it can
    // be scrolled to — it was scrolling all along and looked like it could
    // not.
    ui.spacing_mut().scroll = egui::style::ScrollStyle::solid();

    // Whatever the pane has left after the labels above and the button
    // below, so a taller dock is a taller meter rather than a taller gap —
    // and floored, so a shorter one scrolls instead of squeezing the scale
    // until its labels collide.
    let channel_height = (ui.available_height() - FIXED_ROW_HEIGHT).max(MIN_CHANNEL_HEIGHT);

    // Both directions. Sideways is for more sources than fit; downwards is
    // for a dock shorter than one channel's floor, where the alternative is
    // a fader cut off halfway with no way to reach its mute button.
    egui::ScrollArea::both()
        .id_salt("audio_mixer_channels")
        .auto_shrink([false, true])
        .max_height(viewport.height())
        // Always drawn, not only under the pointer. egui's default bars
        // float into view on hover, which for a squeezed dock means a
        // channel that is visibly cut off with nothing to say it can be
        // scrolled to — the reason this looked like it had no scrolling at
        // all rather than like it had some.
        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
        .show(ui, |ui| {
            ui.horizontal_top(|ui| {
                for source in &snapshot.items {
                    // Scoped per source: every column holds the same widgets,
                    // so without this they would collide on ids derived from
                    // their position alone.
                    ui.push_id(source.id.0, |ui| {
                        show_channel(ui, source, devices, channel_height, i18n, actions);
                    });
                }
            });
        });
}

fn show_channel(
    ui: &mut egui::Ui,
    source: &AudioSourceSnapshot,
    devices: &[AudioDeviceTarget],
    channel_height: f32,
    i18n: &LocalizationManager,
    actions: &mut Vec<UiAction>,
) {
    let size = egui::vec2(SOURCE_WIDTH, channel_height + FIXED_ROW_HEIGHT);
    ui.allocate_ui(size, |ui| {
        ui.vertical(|ui| {
            ui.set_width(SOURCE_WIDTH);
            show_name(ui, source, devices, i18n, actions);
            ui.monospace(format_gain(source.gain_db));
            ui.horizontal(|ui| {
                show_fader(ui, source, actions);
                show_meter(ui, source, channel_height);
                show_scale(ui, channel_height);
            });
            show_mute(ui, source, i18n, actions);
        });
    });
}

/// The name, which is also where the device is chosen.
///
/// A menu rather than a combo box: a column this narrow has no room for a
/// device name, and the name of the source is already the thing a pointer
/// goes to. What it is currently listening to is on the hover, and ticked in
/// the menu.
fn show_name(
    ui: &mut egui::Ui,
    source: &AudioSourceSnapshot,
    devices: &[AudioDeviceTarget],
    i18n: &LocalizationManager,
    actions: &mut Vec<UiAction>,
) {
    let kind = i18n.text(match source.kind {
        AudioSourceKind::Output => TextKey::AudioKindOutput,
        AudioSourceKind::Input => TextKey::AudioKindInput,
    });
    let default_label = i18n.text(TextKey::AudioDeviceDefault);
    // The stored id is what a device is known by, but the picker shows names
    // — so an endpoint that has since gone shows its id rather than becoming
    // an empty row that says nothing.
    let listening = source.device.as_deref().map_or_else(
        || default_label.as_ref().to_owned(),
        |id| {
            devices
                .iter()
                .find(|device| device.id == id)
                .map_or_else(|| id.to_owned(), |device| device.name.clone())
        },
    );

    let menu = ui.menu_button(
        egui::RichText::new(format!("{} ⏷", source.name)).strong(),
        |ui| {
            if ui
                .selectable_label(source.device.is_none(), default_label.as_ref())
                .clicked()
            {
                actions.push(audio_action(AudioCommand::SetDevice(source.id, None)));
                ui.close();
            }
            ui.separator();
            // Only the endpoints of this source's own kind: a microphone
            // cannot be captured as desktop audio, and offering it would be
            // offering a choice that cannot work.
            let mut listed = false;
            for device in devices.iter().filter(|device| device.kind == source.kind) {
                listed = true;
                let label = if device.is_default {
                    format!("{} ({default_label})", device.name)
                } else {
                    device.name.clone()
                };
                let chosen = source.device.as_deref() == Some(device.id.as_str());
                if ui.selectable_label(chosen, label).clicked() {
                    actions.push(audio_action(AudioCommand::SetDevice(
                        source.id,
                        Some(device.id.clone()),
                    )));
                    ui.close();
                }
            }
            if !listed {
                ui.weak(i18n.text(TextKey::AudioNoDevices));
            }
        },
    );
    menu.response.on_hover_text(format!("{kind} · {listening}"));
}

/// The fader stays live while muted rather than greying out: muting is not
/// meant to lose the level somebody set, and a fader that cannot be moved
/// until unmuted makes setting it a two-step job.
fn show_fader(ui: &mut egui::Ui, source: &AudioSourceSnapshot, actions: &mut Vec<UiAction>) {
    let mut gain_db = source.gain_db;
    let fader = ui.add(
        egui::Slider::new(&mut gain_db, MIN_GAIN_DB..=MAX_GAIN_DB)
            .vertical()
            .show_value(false),
    );
    // On release, not on every pixel of the drag: a gesture is one edit, the
    // same rule the Preview's own drag follows.
    if fader.drag_stopped() || (fader.changed() && !fader.dragged()) {
        actions.push(audio_action(AudioCommand::SetGainDb(source.id, gain_db)));
    }
}

/// The level channel.
///
/// Drawn at the size it will keep even with nothing behind it: what arrives
/// later is a number, not a layout. An unmeasured source shows the empty
/// channel, which is also what silence looks like — the two are
/// indistinguishable here, and saying so is [`AudioSourceSnapshot::peak_db`]'s
/// job rather than this one's.
fn show_meter(ui: &mut egui::Ui, source: &AudioSourceSnapshot, height: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(METER_WIDTH, height), egui::Sense::hover());
    let painter = ui.painter();
    let rounding = egui::CornerRadius::same(1);
    painter.rect_filled(rect, rounding, ui.visuals().extreme_bg_color);
    // Outlined, or an empty channel is the same tone as the dock behind it
    // and reads as a gap rather than as a meter with nothing in it.
    painter.rect_stroke(
        rect,
        rounding,
        ui.visuals().widgets.noninteractive.bg_stroke,
        egui::StrokeKind::Inside,
    );

    // Muted is silence whatever the source is doing, so the channel says so
    // rather than showing a level that is not reaching anything.
    let Some(peak_db) = source.peak_db.filter(|_| !source.muted) else {
        return;
    };
    let reached = rect.bottom() - rect.height() * fraction_of_scale(peak_db);
    for (from_db, to_db, color) in [
        (MIN_GAIN_DB, WARN_DB, egui::Color32::from_rgb(60, 180, 75)),
        (WARN_DB, CLIP_DB, egui::Color32::from_rgb(220, 190, 60)),
        (CLIP_DB, 0.0, egui::Color32::from_rgb(215, 70, 60)),
    ] {
        let band_bottom = rect.bottom() - rect.height() * fraction_of_scale(from_db);
        let band_top = rect.bottom() - rect.height() * fraction_of_scale(to_db);
        // Only as far up as the level actually reached.
        let top = band_top.max(reached);
        if top < band_bottom {
            let band = egui::Rect::from_x_y_ranges(rect.x_range(), top..=band_bottom);
            painter.rect_filled(band, 0.0, color);
        }
    }
}

/// The numbers beside the channel, which are what make it and the fader
/// readable as levels rather than as proportions.
fn show_scale(ui: &mut egui::Ui, height: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(SCALE_WIDTH, height), egui::Sense::hover());
    let painter = ui.painter();
    let font = egui::FontId::proportional(9.0);
    let color = ui.visuals().weak_text_color();
    for mark in SCALE_MARKS {
        let y = rect.bottom() - rect.height() * fraction_of_scale(mark);
        // Clamped inward so the end labels stay inside the channel they
        // belong to rather than overhanging it.
        let y = y.clamp(rect.top() + 5.0, rect.bottom() - 5.0);
        painter.text(
            egui::pos2(rect.left() + 2.0, y),
            egui::Align2::LEFT_CENTER,
            format!("{mark:.0}"),
            font.clone(),
            color,
        );
    }
}

fn show_mute(
    ui: &mut egui::Ui,
    source: &AudioSourceSnapshot,
    i18n: &LocalizationManager,
    actions: &mut Vec<UiAction>,
) {
    let label = if source.muted {
        TextKey::AudioUnmute
    } else {
        TextKey::AudioMute
    };
    let button = egui::Button::new(i18n.text(label)).selected(source.muted);
    if ui.add_sized([SOURCE_WIDTH, MUTE_HEIGHT], button).clicked() {
        actions.push(audio_action(AudioCommand::SetMuted(
            source.id,
            !source.muted,
        )));
    }
}

/// Where a decibel value sits on the channel: `0.0` at the bottom, `1.0` at
/// the top. Linear in decibels, which is what the scale's evenly spaced marks
/// promise.
fn fraction_of_scale(db: f32) -> f32 {
    ((db - MIN_GAIN_DB) / -MIN_GAIN_DB).clamp(0.0, 1.0)
}

fn format_gain(gain_db: f32) -> String {
    if gain_db <= MIN_GAIN_DB {
        return "-inf dB".to_owned();
    }
    format!("{gain_db:.1} dB")
}

fn audio_action(command: AudioCommand) -> UiAction {
    UiAction::Project(ProjectCommand::Audio(command))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_quietest_the_fader_goes_reads_as_silence() {
        assert_eq!(format_gain(MIN_GAIN_DB), "-inf dB");
        assert_eq!(format_gain(0.0), "0.0 dB");
        assert_eq!(format_gain(-12.5), "-12.5 dB");
    }

    /// The scale's marks and the meter's fill have to agree about where a
    /// decibel value is, or a level would be read against numbers it does not
    /// line up with.
    #[test]
    fn the_scale_runs_from_silence_at_the_bottom_to_full_at_the_top() {
        assert_eq!(fraction_of_scale(MIN_GAIN_DB), 0.0);
        assert_eq!(fraction_of_scale(0.0), 1.0);
        assert_eq!(fraction_of_scale(-30.0), 0.5);
        // Boosting does not go past full scale: the channel is what a level is
        // measured against, and nothing is louder than the top of it.
        assert_eq!(fraction_of_scale(MAX_GAIN_DB), 1.0);
        assert_eq!(fraction_of_scale(-100.0), 0.0);
    }
}
