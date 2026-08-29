/// One of the audio sources the mixer holds.
///
/// Global rather than a `SceneItem`: a microphone belongs to the person
/// broadcasting, not to whichever Scene happens to be showing. Switching
/// Scenes must not cut the audio, and adding the same microphone to every
/// Scene to avoid that is the arrangement this exists to prevent — the same
/// split OBS makes between its Audio Mixer and its Sources list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AudioSourceId(pub i64);

/// Which side of the sound card a source takes its audio from.
///
/// The two are different captures, not a label: an output is recorded by
/// listening to what is being played (WASAPI loopback, a PipeWire monitor),
/// and an input by opening the device itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioSourceKind {
    /// What the machine is playing — "Desktop Audio".
    Output,
    /// What a microphone or line input hears.
    Input,
}

impl AudioSourceKind {
    pub(crate) fn from_storage_name(name: &str) -> Option<Self> {
        match name {
            "output" => Some(Self::Output),
            "input" => Some(Self::Input),
            _ => None,
        }
    }
}

/// An audio source as the project holds it.
#[derive(Debug, Clone)]
pub struct AudioSource {
    pub id: AudioSourceId,
    pub name: String,
    pub kind: AudioSourceKind,
    /// The device this listens to, or `None` for whichever the system calls
    /// its default. `None` is not "unset": it follows the user changing their
    /// default device, which is what somebody who never opened the picker
    /// expects.
    pub device: Option<String>,
    /// Gain in decibels, where `0.0` is unchanged. Decibels rather than a
    /// linear factor because that is what a fader is marked in and what
    /// `AudioVolume::set_gain_db` takes.
    pub gain_db: f32,
    pub muted: bool,
}

/// The quietest a fader goes before it means silence.
///
/// Chosen to match what the mixer's own scale shows: below this a fader is
/// off, and clamping here rather than at some smaller number keeps the two
/// from disagreeing about where that is.
pub const MIN_GAIN_DB: f32 = -60.0;

/// The loudest a fader goes: unity, where the source is passed through
/// untouched.
///
/// Not a boost range, so that the fader and the level meter beside it share
/// one scale — a fader whose top was +20 would line up with the meter's 0 and
/// mean something else there. Amplifying a quiet microphone past unity is a
/// real want, and it arrives with the control that can show what it is doing
/// rather than by stretching this one.
pub const MAX_GAIN_DB: f32 = 0.0;
