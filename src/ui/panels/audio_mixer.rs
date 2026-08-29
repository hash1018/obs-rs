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
use super::toolbar;

/// One source's column: a fader, a meter, the scale between them, and a name
/// wide enough not to wrap onto a second line — which would push the channel
/// down and leave two columns disagreeing about where their meters start. The
/// dock shows as many as it has room for and scrolls past the rest.
const SOURCE_WIDTH: f32 = 112.0;
const METER_WIDTH: f32 = 9.0;
/// Room for "-60", the longest label on the scale.
const SCALE_WIDTH: f32 = 22.0;
/// What a column spends above the channel: the name and the gain readout,
/// with the gaps around them. The mute button is not in here — it lives in
/// the strip below, outside the scrolling half.
const FIXED_ROW_HEIGHT: f32 = 50.0;
/// Between two columns, agreed on by the channels and the strip of mute
/// buttons under them so that a button stays beneath its own channel.
const COLUMN_GAP: f32 = 4.0;
/// Left below the channels so scrolling to the end stops short of the mute
/// strip rather than against it.
const CHANNEL_BOTTOM_GAP: f32 = 8.0;
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

    // The same split every dock here makes: what can overflow scrolls, and
    // the buttons sit in a strip of their own below it, always reachable.
    // `reserve_list` is what bounds the scrolling half — see its own docs on
    // why a scroll area cannot be trusted to stay inside the space left for
    // it.
    let mut channels = toolbar::reserve_list(ui, "audio_mixer_channels_area");
    let channel_height = (channels.available_height() - FIXED_ROW_HEIGHT).max(MIN_CHANNEL_HEIGHT);

    let scrolled = egui::ScrollArea::both()
        .id_salt("audio_mixer_channels")
        // Zero, not egui's default of 64 — see `toolbar::list_scroll` for
        // what that default does to a dock squeezed below it.
        .min_scrolled_height(0.0)
        .min_scrolled_width(0.0)
        .auto_shrink([false, true])
        .show(&mut channels, |ui| {
            ui.spacing_mut().item_spacing.x = COLUMN_GAP;
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
            // Scrolled to the end, the quietest mark on the scale would
            // otherwise sit directly on the mute strip's edge. The same gap
            // the channel already has above it when nothing is scrolled.
            ui.add_space(CHANNEL_BOTTOM_GAP);
        });

    show_mute_strip(ui, snapshot, scrolled.state.offset.x, i18n, actions);
}

/// The mute buttons, in a strip below the channels rather than inside them.
///
/// Outside the scrolling half so they stay reachable however short the dock
/// is, which is what the Scenes and Sources docks already do with their own
/// toolbars.
///
/// Placed by arithmetic rather than laid out, because each one has to stay
/// under its own channel: the strip is given the channels' horizontal scroll
/// offset and puts every button where its column is, which is also why the
/// two agree on [`COLUMN_GAP`] instead of taking whatever spacing their own
/// `Ui` happened to have.
fn show_mute_strip(
    ui: &mut egui::Ui,
    snapshot: &AudioSnapshot,
    offset_x: f32,
    i18n: &LocalizationManager,
    actions: &mut Vec<UiAction>,
) {
    toolbar::strip(ui, "audio_mixer_mutes", |ui| {
        let origin = ui.max_rect();
        for (index, source) in snapshot.items.iter().enumerate() {
            let left = origin.left() + index as f32 * (SOURCE_WIDTH + COLUMN_GAP) - offset_x;
            let rect = egui::Rect::from_min_size(
                egui::pos2(left, origin.center().y - MUTE_HEIGHT / 2.0),
                egui::vec2(SOURCE_WIDTH, MUTE_HEIGHT),
            );
            // A column scrolled out of view has no button to draw; the strip
            // clips anything that only half is, so a partly visible one is
            // still correct.
            if rect.right() < origin.left() || rect.left() > origin.right() {
                continue;
            }
            show_mute(ui, rect, source, i18n, actions);
        }
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

/// A speaker, crossed out when muted.
///
/// An icon rather than the word, for the same reason the dock's other
/// buttons are: a channel is a narrow column, and "Unmute" in it is most of
/// the width spent saying what a struck-through speaker says at a glance.
/// Drawn from geometry like `toolbar`'s own, so it needs no asset and follows
/// the theme's interaction colours.
///
/// The word is still there on hover, which is also what makes the button say
/// which way it goes: the icon shows the state, the tooltip names the action.
fn show_mute(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    source: &AudioSourceSnapshot,
    i18n: &LocalizationManager,
    actions: &mut Vec<UiAction>,
) {
    let label = if source.muted {
        TextKey::AudioUnmute
    } else {
        TextKey::AudioMute
    };
    let button = egui::Button::new("").selected(source.muted);
    let response = ui.put(rect, button);
    paint_speaker(ui, &response, source.muted);
    if response.on_hover_text(i18n.text(label)).clicked() {
        actions.push(audio_action(AudioCommand::SetMuted(
            source.id,
            !source.muted,
        )));
    }
}

fn paint_speaker(ui: &egui::Ui, response: &egui::Response, muted: bool) {
    let center = response.rect.center();
    // A muted channel is a state to notice rather than a control to read, so
    // it takes the theme's error colour instead of its text one.
    let stroke = if muted {
        egui::Stroke::new(1.5, ui.visuals().error_fg_color)
    } else {
        ui.style().interact(response).fg_stroke
    };
    let painter = ui.painter();

    // The body: a small box with a cone opening to the right.
    let box_left = center.x - 6.0;
    let box_right = center.x - 3.0;
    painter.rect_filled(
        egui::Rect::from_min_max(
            egui::pos2(box_left, center.y - 2.0),
            egui::pos2(box_right, center.y + 2.0),
        ),
        0.0,
        stroke.color,
    );
    painter.add(egui::Shape::convex_polygon(
        vec![
            egui::pos2(box_right, center.y - 2.0),
            egui::pos2(center.x + 1.0, center.y - 5.0),
            egui::pos2(center.x + 1.0, center.y + 5.0),
            egui::pos2(box_right, center.y + 2.0),
        ],
        stroke.color,
        egui::Stroke::NONE,
    ));

    if muted {
        // Struck through rather than simply missing its waves: an icon that
        // differs from the other state only by what is absent is one nobody
        // can read without seeing both.
        painter.line_segment(
            [
                center + egui::vec2(3.0, -4.0),
                center + egui::vec2(8.0, 4.0),
            ],
            stroke,
        );
        return;
    }
    // Two arcs standing in for sound leaving it, drawn as short strokes
    // because a real arc at this size reads as a smudge.
    for (offset, height) in [(4.0, 2.5), (7.0, 4.5)] {
        painter.line_segment(
            [
                center + egui::vec2(offset, -height),
                center + egui::vec2(offset, height),
            ],
            stroke,
        );
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
