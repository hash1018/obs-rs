use std::time::Duration;

use eframe::egui;

use crate::i18n::{LocalizationManager, TextKey};
use crate::resources::{GpuScope, GpuUsage, MemoryUsage};
use crate::snapshots::StatusSnapshot;

pub fn show(ui: &mut egui::Ui, status: &StatusSnapshot, i18n: &LocalizationManager) {
    egui::Panel::bottom("status_bar")
        .exact_size(26.0)
        .frame(
            egui::Frame::new()
                .fill(ui.visuals().panel_fill)
                .inner_margin(egui::Margin::symmetric(8, 0)),
        )
        .show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                show_state(ui, status, i18n);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // Tighter than the default 8, which spent 22 points on
                    // every divider — 8 before, the separator's own 6, 8 after
                    // — to draw a line one point wide. Three of those was most
                    // of the empty space in the bar. The readings are what it
                    // is for; the gaps between them are not.
                    //
                    // Only the gaps move. Each reading is a fixed-width
                    // monospace string (see `format_gpu` and the width test),
                    // which is what keeps a label from shifting as its value
                    // changes — tightening the layout must not cost that.
                    ui.spacing_mut().item_spacing.x = SEGMENT_GAP;
                    ui.monospace(format_fps(status.active_fps, status.target_fps));
                    ui.separator();
                    ui.monospace(format_gpu(status.gpu))
                        .on_hover_text(i18n.text(gpu_tooltip(status.gpu)));
                    ui.separator();
                    ui.monospace(format_optional_percent("CPU", status.cpu_percent));
                    ui.separator();
                    ui.monospace(format_memory(status.memory))
                        .on_hover_text(memory_tooltip(status.memory, i18n));
                    ui.separator();
                    let clock = format_recording_time(status.recording_elapsed);
                    match recording_mark(ui.visuals(), status) {
                        Some((colour, reason)) => {
                            ui.monospace(egui::RichText::new(clock).color(colour))
                                .on_hover_text(i18n.text(reason));
                        }
                        None => {
                            ui.monospace(clock);
                        }
                    }
                });
            });
        });
}

/// What the recording clock is coloured, and the line that says why.
///
/// `None` while nothing is recording: the clock then reads `REC --:--:--`,
/// which is a placeholder holding its own width rather than a state to
/// announce, and colouring it would say something is happening.
///
/// Red while it runs, which is the one convention every recorder in the world
/// shares — a running clock in the same grey as the CPU reading beside it is
/// a number nobody's eye is drawn to. Paused keeps the warning colour it
/// already had: a still figure and a stalled application look alike for the
/// first few seconds, so the difference has to be marked rather than left to
/// a clock that has merely stopped moving.
fn recording_mark(
    visuals: &egui::Visuals,
    status: &StatusSnapshot,
) -> Option<(egui::Color32, TextKey)> {
    status.recording_elapsed?;
    Some(if status.recording_paused {
        (visuals.warn_fg_color, TextKey::StatusRecordingPaused)
    } else {
        (RECORDING_COLOR, TextKey::StatusRecording)
    })
}

/// The recording red, and the same one the mixer paints a clipped channel
/// with — one red in the application rather than one per dock. Light enough
/// to read on the dark panel fill and dark enough to read on the light one,
/// which is why it is a literal rather than either theme's own.
const RECORDING_COLOR: egui::Color32 = egui::Color32::from_rgb(230, 60, 50);

/// The gap either side of a separator in the status bar. What tells two
/// readings apart is the separator between them, so this only has to keep
/// them off it.
const SEGMENT_GAP: f32 = 3.0;

/// The bar's left half: what the application is doing, or the last thing that
/// went wrong trying to.
///
/// A recording that fails to start is otherwise invisible — no file appears,
/// no clock runs, and the button goes back to what it said — so this is where
/// the reason surfaces. In the error colour, and it stays until the next
/// attempt rather than fading, because a message that clears itself is one
/// somebody looking away from the bottom of the window never sees.
///
/// Truncated to the width the bar has, with the whole of it on hover: the
/// readings on the right are what this bar exists for, and a long message
/// from a driver must not push them off the end.
fn show_state(ui: &mut egui::Ui, status: &StatusSnapshot, i18n: &LocalizationManager) {
    let Some(error) = &status.recording_error else {
        ui.label(i18n.text(TextKey::StatusReady));
        return;
    };
    let mut args = fluent_bundle::FluentArgs::new();
    args.set("reason", error.as_str());
    let message = i18n.text_with(TextKey::StatusRecordingFailed, &args);
    ui.scope(|ui| {
        ui.set_max_width(ui.available_width() * ERROR_WIDTH_SHARE);
        ui.label(
            egui::RichText::new(message.as_ref())
                .color(ui.visuals().error_fg_color)
                .strong(),
        )
        .on_hover_text(message.as_ref());
    });
}

/// How much of the bar an error message may take before it is elided. The
/// readings on the right need the rest, and they are fixed-width.
const ERROR_WIDTH_SHARE: f32 = 0.55;

fn format_recording_time(elapsed: Option<Duration>) -> String {
    let Some(elapsed) = elapsed else {
        return "REC --:--:--".to_owned();
    };
    let seconds = elapsed.as_secs();
    format!(
        "REC {:02}:{:02}:{:02}",
        seconds / 3600,
        (seconds % 3600) / 60,
        seconds % 60
    )
}

/// The GPU reading, with a trailing `*` when it covers the whole adapter
/// rather than this process.
///
/// The marker lives inside a fixed-width label so the segment does not change
/// width when the scope changes, which would jitter the whole status bar.
fn format_gpu(usage: Option<GpuUsage>) -> String {
    let label = match usage.map(|usage| usage.scope) {
        Some(GpuScope::Device) => "GPU*",
        Some(GpuScope::Process) | None => "GPU ",
    };
    usage.map_or_else(
        || format!("{label}     --"),
        |usage| format!("{label} {:5.1}%", usage.percent),
    )
}

fn gpu_tooltip(usage: Option<GpuUsage>) -> TextKey {
    match usage.map(|usage| usage.scope) {
        Some(GpuScope::Process) => TextKey::StatusGpuProcess,
        Some(GpuScope::Device) => TextKey::StatusGpuDevice,
        None => TextKey::StatusGpuUnavailable,
    }
}

/// The memory reading, in whole mebibytes.
///
/// The resident figure, because that is the one a task manager shows and so
/// the one anybody will check this against — see [`MemoryUsage`], and the
/// tooltip below for the other.
///
/// Whole megabytes, because the digit after the point is the allocator's
/// business and changes every second without meaning anything; five columns
/// wide, which holds more memory than any machine this runs on has, so the
/// segment never changes width. `MB` rather than `MiB` because that is what
/// every task manager this will be compared against calls it.
fn format_memory(memory: Option<MemoryUsage>) -> String {
    memory.map_or_else(
        || format!("MEM {:>5} MB", "--"),
        |memory| format!("MEM {:>5} MB", megabytes(memory.resident_bytes)),
    )
}

fn megabytes(bytes: u64) -> u64 {
    bytes / (1024 * 1024)
}

/// Both figures, named, because the difference between them is the question
/// this tooltip exists to answer.
fn memory_tooltip(memory: Option<MemoryUsage>, i18n: &LocalizationManager) -> String {
    let Some(memory) = memory else {
        return i18n.text(TextKey::StatusMemory).into_owned();
    };
    let mut args = fluent_bundle::FluentArgs::new();
    args.set("resident", megabytes(memory.resident_bytes) as i64);
    let Some(committed) = memory.committed_bytes else {
        return i18n
            .text_with(TextKey::StatusMemoryResident, &args)
            .into_owned();
    };
    args.set("committed", megabytes(committed) as i64);
    i18n.text_with(TextKey::StatusMemoryBoth, &args)
        .into_owned()
}

fn format_optional_percent(label: &str, value: Option<f32>) -> String {
    value.map_or_else(
        || format!("{label}     --"),
        |value| format!("{label} {value:5.1}%"),
    )
}

/// The two rates, in a field wide enough for the one that was configured.
///
/// `60.00` is five characters, and a sixth reserved for a rate nobody is
/// running was a gap in every frame of the common case. So the width comes
/// from the *target*: five below a hundred, six at or above it, and six when
/// there is no target to ask, which is what this always did.
///
/// Sizing it from the target rather than from the reading is what keeps the
/// labels still. A configured rate changes when someone changes a setting,
/// never frame to frame — where a width taken from the measured rate would
/// shift the whole bar the moment it crossed a hundred, which is exactly the
/// jitter the fixed widths here exist to prevent. Both fields take that one
/// width, so they also stay aligned with each other.
fn format_fps(active: Option<f32>, target: Option<f32>) -> String {
    let width = match target {
        Some(target) if target < 100.0 => 5,
        _ => 6,
    };
    let rate = |value: Option<f32>| {
        value.map_or_else(
            || format!("{:>width$}", "--"),
            |value| format!("{value:>width$.2}"),
        )
    };
    format!("{} / {} FPS", rate(active), rate(target))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(elapsed: Option<Duration>, paused: bool) -> StatusSnapshot {
        StatusSnapshot {
            recording_elapsed: elapsed,
            recording_paused: paused,
            ..StatusSnapshot::default()
        }
    }

    /// Three states, and the middle one is the reason this is not a boolean:
    /// a paused recording is still a recording, and marking it in the running
    /// colour would say it is still writing.
    #[test]
    fn the_clock_is_red_while_it_runs_and_only_while_it_runs() {
        let visuals = egui::Visuals::dark();
        let running = status(Some(Duration::from_secs(5)), false);
        let paused = status(Some(Duration::from_secs(5)), true);

        assert_eq!(
            recording_mark(&visuals, &running),
            Some((RECORDING_COLOR, TextKey::StatusRecording))
        );
        assert_eq!(
            recording_mark(&visuals, &paused),
            Some((visuals.warn_fg_color, TextKey::StatusRecordingPaused))
        );
        assert_eq!(
            recording_mark(&visuals, &status(None, false)),
            None,
            "an idle bar shows a placeholder, which is not a state to colour"
        );
    }

    #[test]
    fn recording_time_is_zero_padded() {
        assert_eq!(
            format_recording_time(Some(Duration::from_secs(3_725))),
            "REC 01:02:05"
        );
    }

    #[test]
    fn changing_metrics_keep_a_stable_text_width() {
        // One width per configured rate, and every reading at that rate takes
        // it — a stalled compositor must not move the labels.
        assert_eq!(format_fps(Some(9.9), Some(60.0)).len(), 17);
        assert_eq!(format_fps(Some(60.0), Some(60.0)).len(), 17);
        assert_eq!(format_fps(None, Some(60.0)).len(), 17);

        // A rate that needs the extra column gets it, for both fields at once.
        assert_eq!(format_fps(Some(99.9), Some(120.0)).len(), 19);
        assert_eq!(format_fps(Some(120.0), Some(120.0)).len(), 19);

        // No target is nothing to size from, so it keeps the wider field.
        assert_eq!(format_fps(None, None).len(), 19);

        // The scope marker must not change the segment's width.
        assert_eq!(format_gpu(None).len(), 11);
        assert_eq!(
            format_gpu(Some(GpuUsage {
                percent: 7.5,
                scope: GpuScope::Process
            }))
            .len(),
            11
        );
        assert_eq!(
            format_gpu(Some(GpuUsage {
                percent: 100.0,
                scope: GpuScope::Device
            }))
            .len(),
            11
        );

        let resident = |bytes| {
            Some(MemoryUsage {
                resident_bytes: bytes,
                committed_bytes: None,
            })
        };
        assert_eq!(format_memory(None).len(), 12);
        assert_eq!(format_memory(resident(312 * 1024 * 1024)).len(), 12);
        assert_eq!(format_memory(resident(8 * 1024 * 1024 * 1024)).len(), 12);

        assert_eq!(format_optional_percent("CPU", None).len(), 10);
        assert_eq!(format_optional_percent("CPU", Some(2.1)).len(), 10);
        assert_eq!(format_optional_percent("CPU", Some(82.5)).len(), 10);
    }
}
