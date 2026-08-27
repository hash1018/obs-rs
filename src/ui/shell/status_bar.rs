use std::time::Duration;

use eframe::egui;

use crate::snapshots::StatusSnapshot;

pub fn show(ui: &mut egui::Ui, status: &StatusSnapshot) {
    egui::Panel::bottom("status_bar")
        .exact_size(26.0)
        .frame(
            egui::Frame::new()
                .fill(ui.visuals().panel_fill)
                .inner_margin(egui::Margin::symmetric(8, 0)),
        )
        .show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                ui.label("Ready");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.monospace(format_fps(status.active_fps, status.target_fps));
                    ui.separator();
                    ui.monospace(format_optional_percent("GPU", status.gpu_percent));
                    ui.separator();
                    ui.monospace(format_optional_percent("CPU", status.cpu_percent));
                    ui.separator();
                    ui.monospace(format_recording_time(status.recording_elapsed));
                });
            });
        });
}

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

fn format_optional_percent(label: &str, value: Option<f32>) -> String {
    value.map_or_else(
        || format!("{label}     --"),
        |value| format!("{label} {value:5.1}%"),
    )
}

fn format_fps(active: Option<f32>, target: Option<f32>) -> String {
    let active = active.map_or_else(|| "    --".to_owned(), |value| format!("{value:6.2}"));
    let target = target.map_or_else(|| "    --".to_owned(), |value| format!("{value:6.2}"));
    format!("{active} / {target} FPS")
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
        assert_eq!(format_fps(None, None).len(), 19);
        assert_eq!(format_fps(Some(9.9), Some(60.0)).len(), 19);
        assert_eq!(format_fps(Some(60.0), Some(60.0)).len(), 19);

        assert_eq!(format_optional_percent("CPU", None).len(), 10);
        assert_eq!(format_optional_percent("CPU", Some(2.1)).len(), 10);
        assert_eq!(format_optional_percent("CPU", Some(82.5)).len(), 10);
    }
}
