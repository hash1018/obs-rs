use std::collections::HashMap;

/// Why a Source that is in the Scene is drawing nothing.
///
/// Two states rather than one flag, because they are not the same news and
/// do not offer the same thing to do about them. A disconnected Source is
/// something that went wrong or went away and can be asked for again; a
/// finished file did exactly what it was told to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceStatus {
    /// Not running, and nothing will open it again without being asked.
    Disconnected,
    /// A media file that reached the end and was not looping.
    Ended,
}
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
    /// The SceneItems whose Source is not running, and why, from the engine.
    ///
    /// The Sources list says so beside them, which is the only thing that
    /// explains a Source that is there and drawing nothing. `Arc` because it
    /// is read on every pass and changes about as often as a window closes.
    pub source_status: Option<Arc<HashMap<crate::domain::SceneItemId, SourceStatus>>>,
    pub cpu_percent: Option<f32>,
    pub gpu: Option<GpuUsage>,
    /// What is in memory for this process, and what it has claimed — see
    /// [`crate::resources::MemoryUsage`], which explains why those differ by
    /// a factor of three.
    pub memory: Option<crate::resources::MemoryUsage>,
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
