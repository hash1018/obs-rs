//! What the pipelines are doing, a row at a time.
//!
//! The status bar says *that* something is behind — see `engine::load`.
//! This says *which* and *why*, which is the only question left once the
//! first one has been answered and the only one a person can act on.
//!
//! # Why these columns
//!
//! Each answers a different thing to do about it.
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
use crate::snapshots::{StatsRow, StatsSnapshot, Subject};

pub(in crate::ui) fn show(ui: &mut egui::Ui, stats: &StatsSnapshot, i18n: &LocalizationManager) {
    if stats.rows.is_empty() {
        // Before the first reading, and on a machine whose engine never
        // started. Both are "nothing to say yet" rather than "nothing is
        // happening", which is why this is the same line the other docks
        // use for an empty selection.
        ui.weak(i18n.text(TextKey::StatsNothingRunning));
        return;
    }

    egui::ScrollArea::vertical().show(ui, |ui| {
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
