//! The dock the audio sources' faders live in.
//!
//! Lists what the project holds rather than what the selected Scene does:
//! audio is global here, so this dock does not change when Scenes do.
//!
//! # Why the channels stand up
//!
//! A fader is read against a scale, and a level is read against a scale, and
//! standing them side by side is how every mixer is built — a horizontal row
//! ends up with a meter that has no scale beside it at all. It also costs a
//! fixed width per source rather than a fixed height, so a dock with room
//! shows more sources instead of more padding.
//!
//! The two scales are not the same one. The fader runs to
//! [`crate::domain::MAX_GAIN_DB`], which is above unity, while the meter
//! stops at full scale and cannot go past it — a fader is what was asked for
//! and a meter is what came back. The numbers down the side belong to the
//! meter; what the fader has instead is a unity mark of its own.

use eframe::egui;

use crate::capture::AudioDeviceTarget;
use crate::domain::{AudioSourceKind, MAX_GAIN_DB, MIN_GAIN_DB};
use crate::i18n::{LocalizationManager, TextKey};
use crate::project::{AudioCommand, ProjectCommand};
use crate::snapshots::{AudioSnapshot, SourceStatus, SourcesSnapshot};

/// One column in the dock.
///
/// The dock draws a superset of what the audio thread runs: the global
/// devices, and the audio-bearing Sources in the Scene being shown. Those are
/// two different things in the project — a device belongs to the person
/// broadcasting, a file's sound to a SceneItem — and they are brought
/// together here, where they are drawn, rather than being made one thing in
/// the project to save this.
///
/// "Global" was always a claim about the *devices*, not about the dock. A
/// microphone must not stop when the Scene changes; a file's sound has no
/// business playing from a Scene nobody is looking at.
struct Channel<'a> {
    id: ChannelId,
    name: &'a str,
    gain_db: f32,
    muted: bool,
    peak_db: Option<f32>,
    /// Set for a device channel. A media file has no endpoint to choose, so
    /// its name is a label rather than a picker.
    device: Option<Device<'a>>,
    /// Whether this channel is played back — `None` for a channel there is
    /// no point monitoring.
    ///
    /// Which today means the desktop: an output is captured by listening to
    /// what is already being played on it, so it is audible before obs-rs
    /// touches it. See [`AudioSourceKind::can_be_monitored`].
    monitored: Option<bool>,
}

/// What a device channel picks from, and what it is picking for.
struct Device<'a> {
    source: crate::domain::AudioSourceId,
    kind: AudioSourceKind,
    /// The stored endpoint id, or `None` for whichever is the system default.
    id: Option<&'a str>,
}

/// Which of the two things a column stands for, and therefore where its
/// fader and its mute button are recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum ChannelId {
    Device(crate::domain::AudioSourceId),
    SceneItem(crate::domain::SceneItemId),
}

/// The columns to draw, devices first.
///
/// Devices lead because their set does not move: a dock whose first column
/// changed with the Scene would make the fader somebody reaches for depend on
/// what they were last looking at.
fn channels<'a>(
    audio: &'a AudioSnapshot,
    sources: &'a SourcesSnapshot,
    status: Option<&std::collections::HashMap<crate::domain::SceneItemId, SourceStatus>>,
) -> Vec<Channel<'a>> {
    // Only what is actually running. A source whose device is not plugged in
    // has no channel here at all rather than a dead one — see
    // `AudioSourceSnapshot::running`.
    let mut channels: Vec<Channel<'a>> = audio
        .items
        .iter()
        .filter(|source| source.running)
        .map(|source| Channel {
            id: ChannelId::Device(source.id),
            name: &source.name,
            gain_db: source.gain_db,
            muted: source.muted,
            peak_db: source.peak_db,
            device: Some(Device {
                source: source.id,
                kind: source.kind,
                id: source.device.as_deref(),
            }),
            monitored: source.kind.can_be_monitored().then_some(source.monitored),
        })
        .collect();

    channels.extend(sources.items.iter().filter_map(|item| {
        let crate::domain::SourceSettings::MediaFile(settings) = &item.settings else {
            return None;
        };
        // Three ways to have no column, and they are all the same statement:
        // there is no sound coming from this Source to fade. A hidden item is
        // silenced (see `engine::source::muted`), and a file that played out
        // or never opened has nothing running behind it.
        if !settings.has_audio || !item.visible || status.is_some_and(|s| s.contains_key(&item.id))
        {
            return None;
        }
        Some(Channel {
            id: ChannelId::SceneItem(item.id),
            name: &item.name,
            gain_db: settings.gain_db,
            muted: settings.muted,
            // A paused file is making no sound. The peak is the last reading
            // rather than a decaying one, so left alone the meter would sit
            // at whatever was playing when the pause landed and say the clip
            // was still going.
            peak_db: item.peak_db.filter(|_| !settings.paused),
            device: None,
            // Always, and this is the channel the control was really wanted
            // for: a file's sound exists nowhere but inside obs-rs, so with
            // this off there is no way at all to hear what you have added.
            monitored: Some(settings.monitored),
        })
    }));
    channels
}

/// The fader's live value, which goes to whichever graph is carrying this
/// channel rather than to the project.
fn gain_drag(id: ChannelId, gain_db: f32) -> UiAction {
    match id {
        ChannelId::Device(id) => UiAction::DragAudioGain(id, gain_db),
        ChannelId::SceneItem(id) => UiAction::DragMediaGain(id, gain_db),
    }
}

/// The one edit that is recorded, when the gesture ends.
fn gain_command(id: ChannelId, gain_db: f32) -> UiAction {
    match id {
        ChannelId::Device(id) => audio_action(AudioCommand::SetGainDb(id, gain_db)),
        ChannelId::SceneItem(id) => UiAction::Project(ProjectCommand::Source(
            crate::project::SourceCommand::SetMediaGain(id, gain_db),
        )),
    }
}

fn mute_command(id: ChannelId, muted: bool) -> UiAction {
    match id {
        ChannelId::Device(id) => audio_action(AudioCommand::SetMuted(id, muted)),
        ChannelId::SceneItem(id) => UiAction::Project(ProjectCommand::Source(
            crate::project::SourceCommand::SetMediaMuted(id, muted),
        )),
    }
}

fn monitor_command(id: ChannelId, monitored: bool) -> UiAction {
    match id {
        ChannelId::Device(id) => audio_action(AudioCommand::SetMonitored(id, monitored)),
        ChannelId::SceneItem(id) => UiAction::Project(ProjectCommand::Source(
            crate::project::SourceCommand::SetMediaMonitored(id, monitored),
        )),
    }
}

use super::super::UiAction;
use super::elide;
use super::toolbar;

/// One source's column: a fader, a meter, the scale between them, and a name
/// wide enough not to wrap onto a second line — which would push the channel
/// down and leave two columns disagreeing about where their meters start. The
/// dock shows as many as it has room for and scrolls past the rest.
const SOURCE_WIDTH: f32 = 112.0;
const METER_WIDTH: f32 = 9.0;
/// Room for "-60", the longest label on the scale, and no more than that.
///
/// The slack matters now that the row is centred: space allocated here but
/// never drawn into is space the whole row is pushed left by, and it shows.
const SCALE_WIDTH: f32 = 18.0;
/// What a column spends above the channel: the name and the gain readout,
/// with the gaps around them. The mute button is not in here — it lives in
/// the strip below, outside the scrolling half.
const FIXED_ROW_HEIGHT: f32 = 50.0;
/// Between two columns, agreed on by the channels and the strip of mute
/// buttons under them so that a button stays beneath its own channel.
const COLUMN_GAP: f32 = 4.0;
/// The shortest a channel may be squeezed to. Below this the scale's labels
/// collide and the fader stops being worth dragging.
const MIN_CHANNEL_HEIGHT: f32 = 96.0;
const MUTE_HEIGHT: f32 = 22.0;

/// How much of a column's button strip the monitor button takes, and the gap
/// between them.
///
/// One width for both, and the pair centred under its column rather than
/// filling it — see [`show_buttons`].
const BUTTON_WIDTH: f32 = 46.0;
const BUTTON_GAP: f32 = 6.0;

/// Every label on the scale, in decibels. Every 12 rather than the 6 a large
/// mixer uses: this dock is drawn at whatever height it is given, and half as
/// many marks stay legible when that is not much.
const SCALE_MARKS: [f32; 6] = [0.0, -12.0, -24.0, -36.0, -48.0, -60.0];

/// Where the meter stops being green, and where it starts warning of a clip.
const WARN_DB: f32 = -20.0;
const CLIP_DB: f32 = -9.0;

/// How long the clip lamp stays lit after the peak that lit it.
///
/// A clip is one buffer wide and gone before anyone has looked up. Two
/// seconds is long enough to catch out of the corner of an eye and short
/// enough that it is reporting the take you are on rather than one from
/// earlier.
const CLIP_HOLD_SECONDS: f64 = 2.0;

// A dock's `show` takes what it draws, and this one draws two kinds of
// channel out of two snapshots. Bundling them into a struct would be a name
// for "the arguments of this function" rather than for anything.
#[allow(clippy::too_many_arguments)]
pub(in crate::ui) fn show(
    ui: &mut egui::Ui,
    snapshot: &AudioSnapshot,
    sources: &SourcesSnapshot,
    status: Option<&std::collections::HashMap<crate::domain::SceneItemId, SourceStatus>>,
    devices: &[AudioDeviceTarget],
    monitoring: bool,
    i18n: &LocalizationManager,
    actions: &mut Vec<UiAction>,
) {
    // Collected once because the mute strip below places its buttons by
    // column index, so the two have to be walking the same sequence.
    let channels = channels(snapshot, sources, status);
    if channels.is_empty() {
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
    let mut list = toolbar::reserve_list(ui, "audio_mixer_channels_area");
    let channel_height = (list.available_height() - FIXED_ROW_HEIGHT).max(MIN_CHANNEL_HEIGHT);

    // The same settings every other dock scrolls with — only the axes differ,
    // because this is the one whose content can also outgrow its width.
    let scrolled = toolbar::scrolls_like_a_dock(
        &mut list,
        egui::ScrollArea::both().id_salt("audio_mixer_channels"),
    )
    .auto_shrink([false, true])
    .show(&mut list, |ui| {
        ui.spacing_mut().item_spacing.x = COLUMN_GAP;
        ui.horizontal_top(|ui| {
            for channel in &channels {
                // Scoped per channel: every column holds the same widgets,
                // so without this they would collide on ids derived from
                // their position alone.
                ui.push_id(channel.id, |ui| {
                    show_channel(ui, channel, devices, channel_height, i18n, actions);
                });
            }
        });
        // Scrolled to the end, the quietest mark on the scale would
        // otherwise sit directly on the mute strip's edge. The same gap
        // the channel already has above it when nothing is scrolled.
        ui.add_space(toolbar::BOTTOM_GAP);
    });

    show_button_strip(
        ui,
        &channels,
        scrolled.state.offset.x,
        monitoring,
        i18n,
        actions,
    );
}

/// The mute and monitor buttons, in a strip below the channels rather than
/// inside them.
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
fn show_button_strip(
    ui: &mut egui::Ui,
    channels: &[Channel<'_>],
    offset_x: f32,
    monitoring: bool,
    i18n: &LocalizationManager,
    actions: &mut Vec<UiAction>,
) {
    toolbar::strip(ui, "audio_mixer_mutes", |ui| {
        let origin = ui.max_rect();
        for (index, channel) in channels.iter().enumerate() {
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
            show_buttons(ui, rect, channel, monitoring, i18n, actions);
        }
    });
}

fn show_channel(
    ui: &mut egui::Ui,
    channel: &Channel<'_>,
    devices: &[AudioDeviceTarget],
    channel_height: f32,
    i18n: &LocalizationManager,
    actions: &mut Vec<UiAction>,
) {
    let size = egui::vec2(SOURCE_WIDTH, channel_height + FIXED_ROW_HEIGHT);
    ui.allocate_ui(size, |ui| {
        ui.vertical(|ui| {
            ui.set_width(SOURCE_WIDTH);
            show_name(ui, channel, devices, i18n, actions);
            // One indent for both rows rather than centring each: the readout
            // is a number whose width changes with its own value, and a
            // centred one would slide left and right as a fader is moved.
            // Sharing the gauges' indent keeps it still, and over the fader it
            // is reporting — which is where a mixer puts it anyway.
            let indent = gauge_indent(ui);
            ui.horizontal(|ui| {
                ui.add_space(indent);
                ui.monospace(format_gain(channel.gain_db));
                show_clip_lamp(ui, channel, i18n);
            });
            ui.horizontal(|ui| {
                ui.add_space(indent);
                show_fader(ui, channel, channel_height, actions);
                show_meter(ui, channel, channel_height);
                show_scale(ui, channel_height);
            });
        });
    });
}

/// Where a channel's contents start, so the fader, its gauge and its readout
/// sit in the middle of the column instead of against its left edge.
///
/// The column is as wide as the widest *name* — a device's is a menu with an
/// endpoint behind it — and everything below the name is far narrower than
/// that. Left alone they hug one edge and leave the rest of the column empty.
///
/// The fader's own width is asked of the same two things egui asks (see
/// `Slider::add_contents`) rather than assumed: a theme with a larger
/// interaction size or a bigger body font moves it, and a hardcoded number
/// would quietly stop being the middle.
fn gauge_indent(ui: &egui::Ui) -> f32 {
    let fader = ui
        .text_style_height(&egui::TextStyle::Body)
        .max(ui.spacing().interact_size.y);
    let gauges = fader + METER_WIDTH + SCALE_WIDTH + ui.spacing().item_spacing.x * 2.0;
    ((SOURCE_WIDTH - gauges) / 2.0).max(0.0)
}

/// The name, which is also where the device is chosen.
///
/// A menu rather than a combo box: a column this narrow has no room for a
/// device name, and the name of the source is already the thing a pointer
/// goes to. What it is currently listening to is on the hover, and ticked in
/// the menu.
fn show_name(
    ui: &mut egui::Ui,
    channel: &Channel<'_>,
    devices: &[AudioDeviceTarget],
    i18n: &LocalizationManager,
    actions: &mut Vec<UiAction>,
) {
    let Some(source) = &channel.device else {
        // A media file has no endpoint to choose, so its name is a label
        // rather than a menu. It is renamed where it lives, in the Sources
        // dock, and this follows.
        //
        // Painted from a galley rather than added as a `Label`, for the two
        // things a Source name needs that a label in a column this narrow
        // does not give: it is centred over the fader like everything else
        // below it, and a name too long for the column is cut to one row
        // instead of wrapping — which would push this channel's gauges down
        // past its neighbours' and leave the row ragged.
        let galley = elide::one_row(ui, channel.name, SOURCE_WIDTH, &egui::TextStyle::Body);
        let elided = galley.elided;
        let (rect, response) = ui.allocate_exact_size(
            egui::vec2(SOURCE_WIDTH, galley.size().y),
            egui::Sense::hover(),
        );
        let left = rect.center().x - galley.size().x / 2.0;
        ui.painter().galley(
            egui::pos2(left, rect.top()),
            galley,
            ui.visuals().strong_text_color(),
        );
        let kind = i18n.text(TextKey::AudioKindMediaFile);
        response.on_hover_text(if elided {
            format!("{kind} · {}", channel.name)
        } else {
            kind.into_owned()
        });
        return;
    };
    let kind = i18n.text(match source.kind {
        AudioSourceKind::Output => TextKey::AudioKindOutput,
        AudioSourceKind::Input => TextKey::AudioKindInput,
    });
    let default_label = i18n.text(TextKey::AudioDeviceDefault);
    // The stored id is what a device is known by, but the picker shows names
    // — so an endpoint that has since gone shows its id rather than becoming
    // an empty row that says nothing.
    let listening = source.id.map_or_else(
        || default_label.as_ref().to_owned(),
        |id| {
            devices
                .iter()
                .find(|device| device.id == id)
                .map_or_else(|| id.to_owned(), |device| device.name.clone())
        },
    );

    // Centred for the same reason the gauges below it are: the column is as
    // wide as the widest name, and this one is usually narrower than that.
    let menu = ui.vertical_centered(|ui| {
        ui.menu_button(
            egui::RichText::new(format!("{} ⏷", channel.name)).strong(),
            |ui| {
                if ui
                    .selectable_label(source.id.is_none(), default_label.as_ref())
                    .clicked()
                {
                    actions.push(audio_action(AudioCommand::SetDevice(source.source, None)));
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
                    let chosen = source.id == Some(device.id.as_str());
                    if ui.selectable_label(chosen, label).clicked() {
                        actions.push(audio_action(AudioCommand::SetDevice(
                            source.source,
                            Some(device.id.clone()),
                        )));
                        ui.close();
                    }
                }
                if !listed {
                    ui.weak(i18n.text(TextKey::AudioNoDevices));
                }
            },
        )
    });
    menu.inner
        .response
        .on_hover_text(format!("{kind} · {listening}"));
}

/// The fader stays live while muted rather than greying out: muting is not
/// meant to lose the level somebody set, and a fader that cannot be moved
/// until unmuted makes setting it a two-step job.
fn show_fader(ui: &mut egui::Ui, channel: &Channel<'_>, height: f32, actions: &mut Vec<UiAction>) {
    // A vertical `Slider` takes its length from `slider_width` — the name is
    // the horizontal case's. Without this it stays at egui's default while
    // the meter and the scale beside it grow with the dock, and a channel
    // made taller ends up with a fader half the height of its own gauge.
    ui.spacing_mut().slider_width = height;
    let mut gain_db = channel.gain_db;
    let fader = ui.add(
        egui::Slider::new(&mut gain_db, MIN_GAIN_DB..=MAX_GAIN_DB)
            .vertical()
            .show_value(false),
    );
    paint_unity_mark(ui, fader.rect);
    // Two destinations, because they answer different questions. What is
    // heard follows the pointer, so every frame of the drag goes to the audio
    // graph; the project hears one edit, when the gesture ends. The same
    // split the Preview's own drag makes between the compositor and the
    // project — a fader is a thing somebody moves while listening to it, and
    // a level that only arrived on release would make it guesswork.
    if fader.dragged() && fader.changed() {
        actions.push(gain_drag(channel.id, gain_db));
    }
    if fader.drag_stopped() || (fader.changed() && !fader.dragged()) {
        actions.push(gain_command(channel.id, gain_db));
    }
}

/// The level channel.
///
/// Drawn at the size it will keep even with nothing behind it: what arrives
/// later is a number, not a layout. An unmeasured source shows the empty
/// channel, which is also what silence looks like — the two are
/// indistinguishable here, and saying so is [`crate::snapshots::AudioSourceSnapshot::peak_db`]'s
/// job rather than this one's.
fn show_meter(ui: &mut egui::Ui, channel: &Channel<'_>, height: f32) {
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
    let Some(peak_db) = channel.peak_db.filter(|_| !channel.muted) else {
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

/// One column's buttons: the same size as each other, centred under it.
///
/// Centred rather than spread across the column, because how many there are
/// varies. A channel that cannot be monitored has only its mute button, and
/// a lone control belongs in the middle of the space it is given — holding
/// it to the left so that the mute buttons line up between columns would put
/// every one of them off-centre to say something about the columns beside
/// it.
///
/// The same width for both, because neither is the more important: one is
/// what a hand reaches for in the middle of a take and the other is set once
/// and left, and a size difference would be reading that as a ranking.
fn show_buttons(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    channel: &Channel<'_>,
    monitoring: bool,
    i18n: &LocalizationManager,
    actions: &mut Vec<UiAction>,
) {
    let buttons = if channel.monitored.is_some() {
        2.0
    } else {
        1.0
    };
    let width = buttons * BUTTON_WIDTH + (buttons - 1.0) * BUTTON_GAP;
    let left = rect.center().x - width / 2.0;
    let button = |index: f32| {
        egui::Rect::from_min_size(
            egui::pos2(left + index * (BUTTON_WIDTH + BUTTON_GAP), rect.top()),
            egui::vec2(BUTTON_WIDTH, rect.height()),
        )
    };
    show_mute(ui, button(0.0), channel, i18n, actions);
    // Draws nothing for a channel there is no point monitoring, which is
    // also the case `buttons` counted above.
    show_monitor(ui, button(1.0), channel, monitoring, i18n, actions);
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
    channel: &Channel<'_>,
    i18n: &LocalizationManager,
    actions: &mut Vec<UiAction>,
) {
    let label = if channel.muted {
        TextKey::AudioUnmute
    } else {
        TextKey::AudioMute
    };
    let button = egui::Button::new("").selected(channel.muted);
    let response = ui.put(rect, button);
    paint_speaker(ui, &response, channel.muted);
    if response.on_hover_text(i18n.text(label)).clicked() {
        actions.push(mute_command(channel.id, !channel.muted));
    }
}

/// Headphones, lit when this channel is played back.
///
/// On or off, switched by clicking: everything monitored is recorded, so
/// what this decides is your ears rather than the file. Which it is in is
/// carried by the button's own selected background and by the icon's colour,
/// and named in full on hover: the same division the status bar's recording
/// clock makes, so no state rests on a colour alone.
///
/// Disabled until a monitoring endpoint has been chosen, because until then
/// there is nowhere for it to play. The hover says so and where to set it,
/// which is the only place the two settings are connected on screen.
fn show_monitor(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    channel: &Channel<'_>,
    monitoring: bool,
    i18n: &LocalizationManager,
    actions: &mut Vec<UiAction>,
) {
    let Some(monitored) = channel.monitored else {
        return;
    };
    let response = ui
        .add_enabled_ui(monitoring, |ui| {
            let button = egui::Button::new("").selected(monitored);
            let response = ui.put(rect, button);
            paint_headphones(ui, &response);
            response
        })
        .inner;
    let response = if monitoring {
        response.on_hover_text(i18n.text(match monitored {
            false => TextKey::AudioMonitorOff,
            true => TextKey::AudioMonitorOn,
        }))
    } else {
        response.on_disabled_hover_text(i18n.text(TextKey::AudioMonitorUnavailable))
    };
    if response.clicked() {
        actions.push(monitor_command(channel.id, !monitored));
    }
}

fn paint_headphones(ui: &egui::Ui, response: &egui::Response) {
    let stroke = ui.style().interact(response).fg_stroke;
    let center = response.rect.center();
    let painter = ui.painter();

    // The headband, as four short chords rather than an arc: a real curve at
    // this size reads as a smudge, which is the same reason the speaker's
    // waves are drawn as strokes.
    let band = [
        egui::pos2(center.x - 6.0, center.y + 1.0),
        egui::pos2(center.x - 6.0, center.y - 3.0),
        egui::pos2(center.x - 2.5, center.y - 6.0),
        egui::pos2(center.x + 2.5, center.y - 6.0),
        egui::pos2(center.x + 6.0, center.y - 3.0),
        egui::pos2(center.x + 6.0, center.y + 1.0),
    ];
    for pair in band.windows(2) {
        painter.line_segment([pair[0], pair[1]], stroke);
    }

    // The earcups, filled so the icon has weight at the size it is drawn.
    for side in [-1.0, 1.0] {
        let x = center.x + side * 6.0;
        painter.rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(x - 1.8, center.y),
                egui::pos2(x + 1.8, center.y + 5.0),
            ),
            1.5,
            stroke.color,
        );
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
/// Where unity sits along a vertical fader's travel, as a fraction from the
/// bottom.
///
/// Its own function because the fader's range is not the meter's: this one
/// spans [`MIN_GAIN_DB`] to [`MAX_GAIN_DB`], which reaches past 0 dB, while
/// [`fraction_of_scale`] stops there.
fn unity_fraction() -> f32 {
    (0.0 - MIN_GAIN_DB) / (MAX_GAIN_DB - MIN_GAIN_DB)
}

/// Marks 0 dB on a fader that now runs past it.
///
/// Drawn rather than left to the readout, because a number tells you where
/// you are and a mark tells you where to aim — and unity is the position
/// anyone setting a level is looking for first.
///
/// The travel is the rect inset by the handle, not the rect: egui shrinks a
/// slider's position range by the handle's own radius at each end so the
/// handle stays inside its widget, and a mark placed on the raw rect would
/// sit above the handle it is meant to line up with. The shape is read from
/// the style rather than assumed, because it is what decides that radius.
fn paint_unity_mark(ui: &egui::Ui, rect: egui::Rect) {
    let radius = rect.width() / 2.5;
    let radius = match ui.visuals().handle_shape {
        egui::style::HandleShape::Circle => radius,
        egui::style::HandleShape::Rect { aspect_ratio } => radius * aspect_ratio,
    };
    let travel = rect.y_range().shrink(radius);
    let y = travel.max - travel.span() * unity_fraction();
    // Short, and only on the left: a line across the fader would be read as
    // part of the handle when the handle is near it.
    ui.painter().hline(
        rect.left()..=rect.left() + rect.width() * 0.4,
        y,
        ui.visuals().widgets.noninteractive.fg_stroke,
    );
}

/// Whether the clip lamp should be lit, and when it was last lit.
///
/// Pure so it can be tested: everything else about a lamp is a timestamp in
/// egui's memory and a red rectangle.
///
/// Latched rather than shown live. A clip is one buffer wide — at 48 kHz and
/// a few hundred samples a buffer, it is gone in the time it takes a frame to
/// be drawn — so a lamp that only followed the level would be a lamp that is
/// never seen on. What it reports is "this clipped", not "this is clipping".
fn clip_lamp(last_clip: f64, now: f64, peak_db: Option<f32>) -> (f64, bool) {
    // At full scale, not merely near it: `CLIP_DB` is where the meter turns
    // red to warn, and warning is not the same as having gone over.
    let last_clip = match peak_db {
        Some(peak) if peak >= 0.0 => now,
        _ => last_clip,
    };
    (last_clip, now - last_clip < CLIP_HOLD_SECONDS)
}

/// The lamp above each channel's readout.
///
/// Beside the gain rather than on the meter: the meter has no room above full
/// scale to put it, and a channel that clipped is nearly always a channel
/// somebody boosted — so the number that caused it and the lamp that reports
/// it are read together.
fn show_clip_lamp(ui: &mut egui::Ui, channel: &Channel<'_>, i18n: &LocalizationManager) {
    let key = egui::Id::new(("audio-clip-lamp", channel.id));
    let now = ui.input(|input| input.time);
    let was = ui
        .data(|data| data.get_temp::<f64>(key))
        .unwrap_or(f64::MIN);
    // Muted is silence whatever the source is doing, the same reading the
    // meter takes — a muted channel cannot be clipping anything.
    let peak = channel.peak_db.filter(|_| !channel.muted);
    let (last_clip, lit) = clip_lamp(was, now, peak);
    if last_clip != was {
        ui.data_mut(|data| data.insert_temp(key, last_clip));
    }

    let (rect, response) = ui.allocate_exact_size(egui::vec2(9.0, 9.0), egui::Sense::hover());
    let rounding = egui::CornerRadius::same(2);
    if lit {
        ui.painter()
            .rect_filled(rect, rounding, egui::Color32::from_rgb(230, 60, 50));
        // Nothing else here would ask for a frame once the audio stops
        // changing, and a lamp that stayed lit until something did would be
        // reporting a clip that had long since aged out.
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(100));
        response.on_hover_text(i18n.text(TextKey::AudioClipped));
    } else {
        // Drawn dark rather than left out, so the lamp has a resting place
        // and lighting up is a change in one thing instead of a new thing
        // appearing and shifting the row.
        ui.painter().rect_stroke(
            rect,
            rounding,
            ui.visuals().widgets.noninteractive.bg_stroke,
            egui::StrokeKind::Inside,
        );
    }
}

/// Where a decibel value sits on the *meter's* scale: silence at the bottom,
/// full scale at the top. Linear in decibels, which is what the scale's evenly
/// spaced marks promise.
///
/// Not the fader's, which reaches past 0 dB — see [`unity_fraction`]. A level
/// has nowhere above full scale to be shown, so a boosted source pins the
/// meter rather than running off it.
/// promise.
fn fraction_of_scale(db: f32) -> f32 {
    ((db - MIN_GAIN_DB) / -MIN_GAIN_DB).clamp(0.0, 1.0)
}

/// The readout above a channel.
///
/// A boost carries its sign, because "6.0 dB" and "-6.0 dB" are one character
/// apart and opposite instructions — and the whole point of a fader that goes
/// above unity is being able to tell at a glance that one has.
fn format_gain(gain_db: f32) -> String {
    if gain_db <= MIN_GAIN_DB {
        return "-inf dB".to_owned();
    }
    if gain_db > 0.0 {
        return format!("+{gain_db:.1} dB");
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

    /// Unity is a position on the fader, not its top, and the mark has to be
    /// at the same place the handle stops at 0 dB or it points at the wrong
    /// level.
    #[test]
    fn unity_sits_below_the_top_of_a_fader_that_boosts() {
        let fraction = unity_fraction();

        assert!(
            (fraction - (-MIN_GAIN_DB / (MAX_GAIN_DB - MIN_GAIN_DB))).abs() < f32::EPSILON,
            "the mark must divide the fader where 0 dB divides its range"
        );
        assert!(
            fraction < 1.0,
            "a fader that stops at unity needs no mark; this one must leave \
             room above it, got {fraction}"
        );
        assert!(
            fraction > 0.5,
            "and most of the travel is still attenuation"
        );
    }

    /// The sign is the whole readout for anyone glancing at it: without it a
    /// boosted channel and a cut one differ by one character.
    #[test]
    fn a_boosted_readout_says_so() {
        assert_eq!(format_gain(6.0), "+6.0 dB");
        assert_eq!(format_gain(MAX_GAIN_DB), "+12.0 dB");
        assert_eq!(format_gain(0.0), "0.0 dB");
        assert_eq!(format_gain(-6.0), "-6.0 dB");
    }

    /// A clip lights the lamp and it stays lit, because the buffer that
    /// clipped is gone long before anyone looks.
    #[test]
    fn the_clip_lamp_latches_and_then_lets_go() {
        let never = f64::MIN;

        // Below full scale, however loud: the meter's red band is a warning,
        // not a clip.
        let (last, lit) = clip_lamp(never, 100.0, Some(CLIP_DB / 2.0));
        assert_eq!(last, never);
        assert!(!lit, "a hot level that did not go over must not report one");

        // At it, and the lamp takes the time.
        let (clipped_at, lit) = clip_lamp(never, 100.0, Some(0.0));
        assert_eq!(clipped_at, 100.0);
        assert!(lit);

        // Silence right after does not put it out.
        let (held, lit) = clip_lamp(clipped_at, 100.5, Some(MIN_GAIN_DB));
        assert_eq!(held, clipped_at, "a later quiet frame must not re-time it");
        assert!(lit, "the lamp has to outlast the buffer that lit it");

        // The hold runs out.
        let (_, lit) = clip_lamp(clipped_at, 100.0 + CLIP_HOLD_SECONDS, None);
        assert!(!lit);
    }

    /// A muted channel is silence whatever is behind it, so it cannot be the
    /// one clipping — the same reading the meter takes.
    #[test]
    fn a_muted_channel_has_nothing_to_report() {
        let (last, lit) = clip_lamp(f64::MIN, 100.0, None);

        assert_eq!(last, f64::MIN);
        assert!(!lit);
    }
}
