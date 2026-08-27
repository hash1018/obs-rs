use std::time::Duration;

#[derive(Default)]
pub struct StatusSnapshot {
    pub recording_elapsed: Option<Duration>,
    pub cpu_percent: Option<f32>,
    pub gpu_percent: Option<f32>,
    /// Actual compositor output rate, not the egui repaint rate.
    pub active_fps: Option<f32>,
    pub target_fps: Option<f32>,
}
