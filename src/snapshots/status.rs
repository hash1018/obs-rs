use std::collections::HashMap;

use std::sync::Arc;

/// Why a Source that is in the Scene is drawing nothing.
///
/// Not one flag, because these are not the same news and do not offer the
/// same thing to do about them. A Source that could not be opened, or went
/// away, can be asked for again; a finished file did exactly what it was
/// told to.
///
/// The reasons are the engine's own sentences, in English — what a device,
/// a server or FFmpeg said. They are shown as they are, beside a translated
/// word that says which of these it is; translating every error a codec can
/// raise is not a thing this could keep up with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceStatus {
    /// Opening it failed, and it has not been opened since.
    Failed(Arc<str>),
    /// Not running: gone, not there yet, or waiting to be asked for — and
    /// why, where that is known.
    Disconnected(Option<Arc<str>>),
    /// A media file that reached the end and was not looping.
    Ended,
}

/// The last screenshot: when it was asked for and what came of it.
///
/// The instant is what lets the status bar say "saved" for a few seconds and
/// then let it go — a file that was written needs no more than a glance — while
/// a failure is kept in view until the next attempt, as a recording's is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenshotReport {
    pub at: std::time::Instant,
    pub outcome: Result<std::path::PathBuf, String>,
}

impl SourceStatus {
    /// Why, in the engine's words, where it said.
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Failed(reason) => Some(reason),
            Self::Disconnected(reason) => reason.as_deref(),
            Self::Ended => None,
        }
    }
}
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
    /// How long the broadcast has been live, or `None` when none is.
    ///
    /// Its own field beside the recording's, not a variant of it: both can
    /// be running at once, and each has a clock of its own.
    pub streaming_elapsed: Option<Duration>,
    /// Why the last attempt to start a broadcast failed, if it did — kept
    /// for the reason `recording_error` is.
    pub streaming_error: Option<Arc<String>>,
    /// The last screenshot, once one has been taken.
    pub screenshot: Option<Arc<ScreenshotReport>>,
    /// Whether a dropped broadcast is waiting to be tried again.
    ///
    /// Beside the clock rather than in it: what the bar has to say is that
    /// this is not simply off, and the elapsed time it would otherwise show
    /// is a broadcast that is not happening.
    pub streaming_reconnecting: bool,
    /// What share of the last second the outputs spent holding up whatever
    /// feeds them — see `engine::load`. Zero on a machine doing neither.
    pub output_load: f32,
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
