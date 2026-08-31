use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use crate::resources::GpuUsage;

#[derive(Default)]
pub struct StatusSnapshot {
    pub recording_elapsed: Option<Duration>,
    /// Whether the running recording is paused. Its clock stops with it —
    /// what that counts is how long the file is.
    pub recording_paused: bool,
    /// Why the last attempt to start a recording failed, if it did.
    ///
    /// Kept until the next attempt rather than cleared on a timer: a
    /// recording that did not start leaves nothing else behind — no file, no
    /// running clock — so this is the only thing that says it was tried.
    /// `Arc` because it is read on every pass and rarely changes.
    pub recording_error: Option<Arc<String>>,
    /// The SceneItems whose Source is not running, from the engine.
    ///
    /// The Sources list says so beside them, which is the only thing that
    /// explains a Source that is there and drawing nothing. `Arc` because it
    /// is read on every pass and changes about as often as a window closes.
    pub disconnected_sources: Option<Arc<HashSet<crate::domain::SceneItemId>>>,
    pub cpu_percent: Option<f32>,
    pub gpu: Option<GpuUsage>,
    /// System memory this process has to itself — see
    /// [`crate::resources::ResourceUsage::memory_bytes`] for what that does
    /// and does not count.
    pub memory_bytes: Option<u64>,
    /// Actual compositor output rate, not the egui repaint rate.
    pub active_fps: Option<f32>,
    pub target_fps: Option<f32>,
    /// Which H.264 encoders this machine can record with. Empty until the
    /// engine has probed, which the Settings dialog shows as such rather than
    /// as "none".
    pub encoders: Vec<crate::settings::RecordingEncoder>,
    /// Which audio codecs this build can record with. Empty until the engine
    /// has probed, on the same terms as `encoders` above.
    pub audio_codecs: Vec<crate::settings::RecordingAudioCodec>,
}
