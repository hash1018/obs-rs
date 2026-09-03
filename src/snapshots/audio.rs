use crate::domain::{AudioSourceId, AudioSourceKind, MonitorMode};

/// What the audio mixer dock draws.
///
/// Global rather than per-Scene: these do not change when the selected Scene
/// does, which is the whole reason audio is modelled apart from SceneItems.
#[derive(Clone, Default)]
pub struct AudioSnapshot {
    pub items: Vec<AudioSourceSnapshot>,
}

#[derive(Clone)]
pub struct AudioSourceSnapshot {
    pub id: AudioSourceId,
    pub name: String,
    pub kind: AudioSourceKind,
    /// The device this listens to, or `None` for whichever the system calls
    /// its default.
    pub device: Option<String>,
    pub gain_db: f32,
    pub muted: bool,
    /// Whether this source is played back, and whether it still reaches the
    /// recording — see [`MonitorMode`].
    pub monitor: MonitorMode,
    /// The loudest sample seen since the last update, in decibels relative to
    /// full scale, or `None` when nothing is measuring yet.
    ///
    /// Positive above full scale rather than clamped at it: a fader that
    /// boosts can push a source past full scale, and a meter that cannot tell
    /// a level which merely reached it from one well over it cannot report a
    /// clip. Pinning the bar at the top of its scale is the dock's own
    /// business.
    ///
    /// `None` is what the mixer shows today: the meter is drawn but has
    /// nothing behind it until an audio pipeline exists to report from. It is
    /// here rather than added later so the dock's layout is the one it will
    /// keep, and so the value has somewhere to arrive.
    pub peak_db: Option<f32>,
    /// Whether a capture is running behind this source.
    ///
    /// `false` means the machine has no endpoint it could be opened on — an
    /// unplugged microphone, or a kind of device this machine has none of.
    /// The mixer dock leaves such a source out entirely rather than drawing a
    /// channel that can never move, and puts it back when the device arrives.
    ///
    /// The project still holds it either way: this is about what is running,
    /// not about what the user asked for. It defaults to shown, so a source
    /// is hidden only once something has positively said it is not running.
    pub running: bool,
}
