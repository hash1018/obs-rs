//! What the machine, the compositor and each output are doing.
//!
//! Laid out as OBS lays out its own, because that is the dock people already
//! know how to read: the machine and the rendering at the top, one row per
//! output below, a reset at the bottom. Under that, folded away, the table
//! this dock began as — one row per thing in the graph a person would act
//! on — which is where *which* Source or output is behind is answered.
//!
//! The status bar says *that* something is behind — see `engine::load`.
//! This says what it is costing, in the terms OBS uses for the same things.
//!
//! # Where these numbers differ from OBS's
//!
//! An output here waits rather than drops: its queue blocks, so the first
//! sign of an encoder or a disk falling behind is the compositor being held
//! up — ticks it never drew, counted as frames missed to rendering lag,
//! while the time a frame takes to draw stays short. Only a wait that runs
//! out loses a frame, and that is what is counted as skipped to encoding
//! lag. Read together, the two say which of drawing and writing is behind,
//! which is the question OBS's own figures leave to guesswork.
//!
//! # Why the details table has these columns
//!
//! `Rate` is whether it is moving at all. A Source at zero has stopped;
//! a recording at zero is not being written. Nothing else says that.
//!
//! `Busy` is what an output's branch costs the thread it runs on, which is
//! what encoding is taking. Only an output fills it in: everything else
//! here has a pacer below it, and a pacer's wait for a frame's timestamp to
//! come due is spent inside the call and counted as time busy — measured, a
//! healthy 30 fps clip read 15.0 s of busy in 15 s. A column that says a
//! Source costs everything would have someone turn off the cheapest thing
//! running, so it says nothing instead.
//!
//! `Queue` is how close an output is to blocking, which is where the
//! status bar's own reading comes from. It is shown per output rather
//! than as one number so that a full recording queue and a full broadcast
//! queue are told apart — the first is the disk or the encoder, the second
//! is usually the upstream link. A Source's queues are left out for the
//! same pacer: measured on the same clip, its decoded-frame queues sat at
//! 8 of 8 and 64 of 64 while nothing whatever was wrong.
//!
//! `Idle` is how long since anything passed through. A camera that was
//! unplugged reads the same as one pointed at a dark room on every other
//! column; this is the one that separates them.

use eframe::egui;

use crate::i18n::{LocalizationManager, TextKey};
use crate::snapshots::{OutputStats, Rendering, StatsRow, StatsSnapshot, StatusSnapshot, Subject};

/// What the dock keeps between frames: where it was last reset.
#[derive(Default)]
pub(in crate::ui) struct StatsPanelState {
    reset: Option<Baseline>,
}

/// The totals at the moment Reset was pressed, which later totals are
/// counted from.
///
/// Each carries the run it was taken from. A recording started after the
/// reset counts from its own zero — measured against the last one's totals
/// it would read as nothing lost and nothing written until it had caught
/// up with them.
#[derive(Clone, Copy)]
struct Baseline {
    rendering: Option<Rendering>,
    recording: Option<OutputStats>,
    broadcast: Option<OutputStats>,
}

pub(in crate::ui) fn show(
    ui: &mut egui::Ui,
    state: &mut StatsPanelState,
    stats: &StatsSnapshot,
    status: &StatusSnapshot,
    i18n: &LocalizationManager,
) {
    if stats.rows.is_empty() && stats.rendering.is_none() {
        // Before the first reading, and on a machine whose engine never
        // started. Both are "nothing to say yet" rather than "nothing is
        // happening", which is why this is the same line the other docks
        // use for an empty selection.
        ui.weak(i18n.text(TextKey::StatsNothingRunning));
        return;
    }

    // The button first, from the bottom up, so it is laid out against the
    // dock's own edge. Inside the scrolling area it sat at the right of the
    // content, which in a narrow dock is past the right of the view — the
    // one control here that had to be scrolled to.
    ui.with_layout(egui::Layout::bottom_up(egui::Align::Max), |ui| {
        if ui
            .button(i18n.text(TextKey::StatsReset))
            .on_hover_text(i18n.text(TextKey::StatsResetHint))
            .clicked()
        {
            state.reset = Some(Baseline {
                rendering: stats.rendering,
                recording: stats.recording,
                broadcast: stats.broadcast,
            });
        }
        ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
            egui::ScrollArea::both().show(ui, |ui| {
                summary(ui, state, stats, status, i18n);
                ui.separator();
                outputs(ui, state, stats, status, i18n);
                ui.add_space(4.0);
                egui::CollapsingHeader::new(i18n.text(TextKey::StatsDetails))
                    .id_salt("stats-details")
                    .show(ui, |ui| details(ui, stats, i18n));
            });
        });
    });
}

/// The machine on one side and the rendering on the other, as OBS has
/// them — side by side where the dock is wide enough, one after the other
/// where it is not.
fn summary(
    ui: &mut egui::Ui,
    state: &StatsPanelState,
    stats: &StatsSnapshot,
    status: &StatusSnapshot,
    i18n: &LocalizationManager,
) {
    let baseline = state.reset.as_ref();
    let recording_rate = stats.recording.and_then(|output| output.bitrate);

    let machine = [
        Figure::new(
            i18n.text(TextKey::StatsCpu),
            status
                .cpu_percent
                .map_or_else(dash, |percent| format!("{percent:.2}%")),
        ),
        Figure::new(
            i18n.text(TextKey::StatsDiskAvailable),
            stats.disk_available.map_or_else(dash, bytes),
        ),
        Figure::new(
            i18n.text(TextKey::StatsDiskFullIn),
            match (stats.disk_available, recording_rate) {
                (Some(free), Some(rate)) if rate > 0.0 => {
                    hours_minutes(free as f64 * 8.0 / rate, i18n)
                }
                // Only a running recording fills a disk, and only one that
                // has written for a whole reading has a rate to divide by.
                _ => dash(),
            },
        ),
        Figure::new(
            i18n.text(TextKey::StatsMemory),
            status
                .memory
                .map_or_else(dash, |memory| bytes(memory.resident_bytes)),
        ),
    ];

    let rendering = stats.rendering.map(|now| {
        let before = baseline.and_then(|baseline| baseline.rendering);
        let at = |pick: fn(&Rendering) -> u64| {
            since_reset(pick(&now), now.run, before.map(|was| (was.run, pick(&was))))
        };
        (at(|r| r.made), at(|r| r.missed), now.frame_time)
    });
    let (lost, frames) = [
        (stats.recording, baseline.and_then(|b| b.recording)),
        (stats.broadcast, baseline.and_then(|b| b.broadcast)),
    ]
    .into_iter()
    .filter_map(|(now, before)| Some(counted(now?, before)))
    .fold((0, 0), |(lost, frames), counted| {
        (lost + counted.lost, frames + counted.frames)
    });

    let drawing = [
        Figure::new(
            i18n.text(TextKey::StatsFps),
            status
                .active_fps
                .map_or_else(dash, |fps| format!("{fps:.2}")),
        ),
        Figure::new(
            i18n.text(TextKey::StatsFrameTime),
            rendering
                .and_then(|(_, _, frame_time)| frame_time)
                .map_or_else(dash, |time| {
                    format!("{:.1} ms", time.as_secs_f64() * 1000.0)
                }),
        ),
        Figure::new(
            i18n.text(TextKey::StatsMissedFrames),
            rendering.map_or_else(dash, |(made, missed, _)| fraction(missed, made + missed)),
        )
        .hint(i18n.text(TextKey::StatsMissedFramesHint)),
        Figure::new(
            i18n.text(TextKey::StatsSkippedFrames),
            fraction(lost, frames),
        )
        .hint(i18n.text(TextKey::StatsSkippedFramesHint)),
    ];

    // Wide enough for a label and a value on each side; below it, the two
    // halves stack rather than scroll sideways past what is being read.
    let side_by_side = ui.available_width() >= 560.0;
    egui::Grid::new("stats-summary")
        .num_columns(if side_by_side { 4 } else { 2 })
        .spacing([16.0, 4.0])
        .show(ui, |ui| {
            if side_by_side {
                for (left, right) in machine.iter().zip(&drawing) {
                    left.show(ui);
                    right.show(ui);
                    ui.end_row();
                }
            } else {
                for figure in machine.iter().chain(&drawing) {
                    figure.show(ui);
                    ui.end_row();
                }
            }
        });
}

/// One row per output, in OBS's order and with OBS's columns.
fn outputs(
    ui: &mut egui::Ui,
    state: &StatsPanelState,
    stats: &StatsSnapshot,
    status: &StatusSnapshot,
    i18n: &LocalizationManager,
) {
    let baseline = state.reset.as_ref();
    egui::Grid::new("stats-outputs")
        .num_columns(5)
        .spacing([16.0, 4.0])
        .striped(true)
        .show(ui, |ui| {
            for key in [
                TextKey::StatsOutput,
                TextKey::StatsStatus,
                TextKey::StatsLostFrames,
                TextKey::StatsDataOutput,
                TextKey::StatsBitrate,
            ] {
                ui.strong(i18n.text(key));
            }
            ui.end_row();

            let broadcast_status = if status.streaming_reconnecting {
                TextKey::StatsStatusReconnecting
            } else if status.streaming_elapsed.is_some() {
                TextKey::StatsStatusLive
            } else {
                TextKey::StatsStatusStopped
            };
            let recording_status = match (status.recording_elapsed, status.recording_paused) {
                (None, _) => TextKey::StatsStatusStopped,
                (Some(_), true) => TextKey::StatsStatusPaused,
                (Some(_), false) => TextKey::StatsStatusRecording,
            };
            for (name, status_key, now, before) in [
                (
                    TextKey::StatsBroadcast,
                    broadcast_status,
                    stats.broadcast,
                    baseline.and_then(|b| b.broadcast),
                ),
                (
                    TextKey::StatsRecording,
                    recording_status,
                    stats.recording,
                    baseline.and_then(|b| b.recording),
                ),
            ] {
                ui.label(i18n.text(name));
                let text = i18n.text(status_key);
                match status_key {
                    // Not stopped and not simply running: the two states
                    // someone should notice without reading the word.
                    TextKey::StatsStatusPaused | TextKey::StatsStatusReconnecting => {
                        ui.label(egui::RichText::new(text).color(ui.visuals().warn_fg_color));
                    }
                    _ => {
                        ui.label(text);
                    }
                }
                match now {
                    Some(now) => {
                        let counted = counted(now, before);
                        let lost = fraction(counted.lost, counted.frames);
                        if counted.lost > 0 {
                            ui.monospace(
                                egui::RichText::new(lost).color(ui.visuals().error_fg_color),
                            );
                        } else {
                            ui.monospace(lost);
                        }
                        ui.monospace(bytes(counted.bytes));
                        ui.monospace(now.bitrate.map_or_else(dash, bitrate));
                    }
                    // Not running, so there is nothing of its to count — the
                    // last run's figures went with it.
                    None => {
                        for _ in 0..3 {
                            ui.monospace(DASH);
                        }
                    }
                }
                ui.end_row();
            }
        });
}

/// One label and its value, with an explanation on hover where the label
/// alone would mislead.
struct Figure<'a> {
    label: std::borrow::Cow<'a, str>,
    value: String,
    hint: Option<std::borrow::Cow<'a, str>>,
}

impl<'a> Figure<'a> {
    fn new(label: std::borrow::Cow<'a, str>, value: String) -> Self {
        Self {
            label,
            value,
            hint: None,
        }
    }

    fn hint(self, hint: std::borrow::Cow<'a, str>) -> Self {
        Self {
            hint: Some(hint),
            ..self
        }
    }

    fn show(&self, ui: &mut egui::Ui) {
        let label = ui.label(self.label.as_ref());
        if let Some(hint) = &self.hint {
            label.on_hover_text(hint.as_ref());
        }
        ui.monospace(&self.value);
    }
}

/// An output's totals since the reset, where the reset was of this run.
struct Counted {
    frames: u64,
    lost: u64,
    bytes: u64,
}

fn counted(now: OutputStats, before: Option<OutputStats>) -> Counted {
    let at = |pick: fn(&OutputStats) -> u64| {
        since_reset(pick(&now), now.run, before.map(|was| (was.run, pick(&was))))
    };
    Counted {
        frames: at(|o| o.frames),
        lost: at(|o| o.lost),
        bytes: at(|o| o.bytes),
    }
}

/// A total counted from a reset — if the reset was taken of the same run.
/// A run started since counts from its own zero.
///
/// Any run identity will do; the dock's are the ids of the elements a run
/// was made of — see [`Rendering::run`].
fn since_reset<R: PartialEq>(now: u64, run: R, reset: Option<(R, u64)>) -> u64 {
    match reset {
        Some((was, at)) if was == run => now.saturating_sub(at),
        _ => now,
    }
}

/// `part / whole (share%)`, the way OBS writes a count of frames.
fn fraction(part: u64, whole: u64) -> String {
    let share = if whole == 0 {
        0.0
    } else {
        part as f64 * 100.0 / whole as f64
    };
    format!("{part} / {whole} ({share:.1}%)")
}

/// A size, in the units the status bar uses: powers of 1024, written `MB`
/// and `GB` because that is what a task manager shows beside it.
fn bytes(bytes: u64) -> String {
    let megabytes = bytes as f64 / (1024.0 * 1024.0);
    if megabytes >= 1024.0 {
        format!("{:.1} GB", megabytes / 1024.0)
    } else {
        format!("{megabytes:.1} MB")
    }
}

/// Bits a second, as kilobits — the unit a bit rate is set in.
fn bitrate(bits_per_second: f64) -> String {
    format!("{:.0} kb/s", bits_per_second / 1000.0)
}

/// A span of seconds as hours and minutes, which is all an estimate this
/// rough deserves.
fn hours_minutes(seconds: f64, i18n: &LocalizationManager) -> String {
    let minutes = (seconds / 60.0).floor() as u64;
    let (hours, minutes) = (minutes / 60, minutes % 60);
    let minutes_text = format!("{minutes}{}", i18n.text(TextKey::StatsMinutes));
    if hours == 0 {
        minutes_text
    } else {
        format!("{hours}{} {minutes_text}", i18n.text(TextKey::StatsHours))
    }
}

fn details(ui: &mut egui::Ui, stats: &StatsSnapshot, i18n: &LocalizationManager) {
    egui::Grid::new("stats")
        .num_columns(5)
        .spacing([10.0, 4.0])
        .striped(true)
        .show(ui, |ui| {
            for key in [
                TextKey::StatsSubject,
                TextKey::StatsRate,
                TextKey::StatsBusy,
                TextKey::StatsQueue,
                TextKey::StatsIdle,
            ] {
                ui.strong(i18n.text(key));
            }
            ui.end_row();

            for row in stats.rows.iter() {
                show_row(ui, row, stats.interval.as_secs_f32(), i18n);
            }
        });
}

fn show_row(ui: &mut egui::Ui, row: &StatsRow, interval: f32, i18n: &LocalizationManager) {
    ui.label(subject(&row.subject, i18n));

    // Per second rather than per reading, because a reading is an interval
    // nobody chose and a second is one everybody knows.
    ui.monospace(match row.buffers {
        Some(buffers) if interval > 0.0 => format!("{:.0}/s", buffers as f32 / interval),
        _ => DASH.to_owned(),
    });

    ui.monospace(match row.busy {
        Some(busy) => format!("{:.0}%", busy * 100.0),
        // Only an output has an honest answer here — see `StatsRow::busy`.
        None => DASH.to_owned(),
    });

    match row.queue {
        Some(share) => {
            let text = format!("{:.0}%", share * 100.0);
            // The same thresholds the status bar's own reading uses, so a
            // bar that has gone yellow is explained by a row that has.
            if share >= 0.5 {
                ui.monospace(egui::RichText::new(text).color(ui.visuals().error_fg_color));
            } else if share >= 0.25 {
                ui.monospace(egui::RichText::new(text).color(ui.visuals().warn_fg_color));
            } else {
                ui.monospace(text);
            }
        }
        None => {
            ui.monospace(DASH);
        }
    }

    ui.monospace(match row.idle_for {
        // Below a second is every healthy thing between two buffers, and
        // showing it would leave this column flickering on every row.
        Some(idle) if idle.as_secs_f32() >= 1.0 => format!("{:.0}s", idle.as_secs_f32()),
        Some(_) => DASH.to_owned(),
        None => DASH.to_owned(),
    });

    if row.errors > 0 {
        ui.label(
            egui::RichText::new(format!("{} ×", row.errors)).color(ui.visuals().error_fg_color),
        )
        .on_hover_text(i18n.text(TextKey::StatsErrors));
    }
    ui.end_row();
}

/// What a row is called, in the reader's own language.
///
/// A Source keeps the name it has in the Sources dock: that is what someone
/// reading this would go and change, and an internal element name would send
/// them looking for something that is not in the interface.
fn subject(subject: &Subject, i18n: &LocalizationManager) -> String {
    match subject {
        Subject::Compositor => i18n.text(TextKey::StatsCompositor).into_owned(),
        Subject::Recording => i18n.text(TextKey::StatsRecording).into_owned(),
        Subject::Broadcast => i18n.text(TextKey::StatsBroadcast).into_owned(),
        Subject::Source(name) => name.clone(),
    }
}

/// What a column shows when the row has nothing for it — a fixed width, so
/// a table of these does not shift as rows come and go.
const DASH: &str = "—";

fn dash() -> String {
    DASH.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Counted from the reset while it is the same run; from zero once a
    /// new one has started — measured against the last run's totals a new
    /// recording would read as nothing written until it caught up.
    #[test]
    fn a_total_counts_from_the_reset_only_for_the_run_it_was_taken_of() {
        assert_eq!(since_reset(900, 1, None), 900, "never reset");
        assert_eq!(since_reset(900, 1, Some((1, 300))), 600);
        assert_eq!(
            since_reset(5, 1, Some((1, 300))),
            0,
            "a total that went backwards is not a negative"
        );
    }

    /// A run started after the reset counts from its own zero, however far
    /// its totals have climbed by the time anyone looks.
    #[test]
    fn a_run_started_since_the_reset_counts_from_its_own_zero() {
        let (first, second) = (1, 2);
        assert_eq!(since_reset(40, second, Some((first, 300))), 40);
        assert_eq!(
            since_reset(4_000, second, Some((first, 300))),
            4_000,
            "not 3700: the reset was of a run that has since ended"
        );
        assert_eq!(since_reset(400, first, Some((first, 300))), 100);
    }

    #[test]
    fn a_count_of_frames_is_written_the_way_obs_writes_one() {
        assert_eq!(fraction(0, 877), "0 / 877 (0.0%)");
        assert_eq!(fraction(3, 400), "3 / 400 (0.8%)");
        assert_eq!(
            fraction(0, 0),
            "0 / 0 (0.0%)",
            "nothing handed over is nothing lost, not a division by zero"
        );
    }

    #[test]
    fn sizes_and_rates_are_written_in_the_units_they_are_set_in() {
        assert_eq!(bytes(0), "0.0 MB");
        assert_eq!(bytes(5 * 1024 * 1024 + 512 * 1024), "5.5 MB");
        assert_eq!(bytes(542_700_000_000), "505.4 GB");
        assert_eq!(bitrate(6_000_000.0), "6000 kb/s");
    }
}
