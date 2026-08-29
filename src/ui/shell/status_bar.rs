use std::time::Duration;

use eframe::egui;

use crate::i18n::{LocalizationManager, TextKey};
use crate::resources::{GpuScope, GpuUsage};
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
                ui.label(i18n.text(TextKey::StatusReady));
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
                    ui.monospace(format_recording_time(status.recording_elapsed));
                });
            });
        });
}

/// The gap either side of a separator in the status bar. What tells two
/// readings apart is the separator between them, so this only has to keep
/// them off it.
const SEGMENT_GAP: f32 = 3.0;

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

        assert_eq!(format_optional_percent("CPU", None).len(), 10);
        assert_eq!(format_optional_percent("CPU", Some(2.1)).len(), 10);
        assert_eq!(format_optional_percent("CPU", Some(82.5)).len(), 10);
    }
}
