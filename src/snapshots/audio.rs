use crate::domain::{AudioSourceId, AudioSourceKind};

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
    /// The loudest sample seen since the last update, in decibels below full
    /// scale, or `None` when nothing is measuring yet.
    ///
    /// `None` is what the mixer shows today: the meter is drawn but has
    /// nothing behind it until an audio pipeline exists to report from. It is
    /// here rather than added later so the dock's layout is the one it will
    /// keep, and so the value has somewhere to arrive.
    pub peak_db: Option<f32>,
}
