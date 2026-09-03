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
    /// Whether playing this back could tell anybody anything.
    ///
    /// An output is captured by listening to what is *already* being played
    /// on it, so it is audible before obs-rs touches it — monitoring one
    /// would be hearing the same sound a second time, late. What that leaves
    /// of the three modes is "recorded or not", which the mute button
    /// already is, so the control is not offered at all rather than offered
    /// and made to mean less than it says elsewhere.
    ///
    /// An input is the opposite: a microphone is a sound nothing else in the
    /// room is making audible.
    pub fn can_be_monitored(self) -> bool {
        matches!(self, Self::Input)
    }

    pub(crate) fn from_storage_name(name: &str) -> Option<Self> {
        match name {
            "output" => Some(Self::Output),
            "input" => Some(Self::Input),
            _ => None,
        }
    }
}

/// Whether a source is played back to the person running obs-rs, and whether
/// it still reaches the recording.
///
/// Two questions rather than one, which is why this is not a checkbox: what
/// you want to hear and what you want recorded are independent. Your own
/// microphone is the clearest case — you want it recorded and you very much
/// do not want to hear it, because a voice coming back even a few tens of
/// milliseconds late makes speaking difficult.
///
/// The same three states OBS has, for the same reasons.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MonitorMode {
    /// Recorded, not played back. What every source starts as.
    #[default]
    Off,
    /// Played back, and left out of the recording — a clip being cued up, or
    /// a call you need to hear but have no right to record.
    MonitorOnly,
    /// Both. What a media file playing to an audience usually wants.
    MonitorAndOutput,
}

impl MonitorMode {
    /// Whether the monitor mix takes this source.
    pub fn is_monitored(self) -> bool {
        matches!(self, Self::MonitorOnly | Self::MonitorAndOutput)
    }

    /// Whether the recording takes this source.
    ///
    /// `MonitorOnly` is the one that answers `false`, and it is the whole
    /// reason this is three states and not two.
    pub fn reaches_output(self) -> bool {
        matches!(self, Self::Off | Self::MonitorAndOutput)
    }

    pub(crate) fn storage_name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::MonitorOnly => "only",
            Self::MonitorAndOutput => "both",
        }
    }

    pub(crate) fn from_storage_name(name: &str) -> Option<Self> {
        match name {
            "off" => Some(Self::Off),
            "only" => Some(Self::MonitorOnly),
            "both" => Some(Self::MonitorAndOutput),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The desktop is captured by listening to what is already playing on
    /// it, so it needs no playing back. An input is the case the control
    /// exists for.
    #[test]
    fn only_what_cannot_already_be_heard_is_worth_monitoring() {
        assert!(AudioSourceKind::Input.can_be_monitored());
        assert!(!AudioSourceKind::Output.can_be_monitored());
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
    /// Whether this is played back, and whether it still reaches the
    /// recording — see [`MonitorMode`].
    ///
    /// Stored per source rather than as one switch for the mixer, because the
    /// answer differs source by source: a microphone wants `Off` while the
    /// media file beside it wants `MonitorAndOutput`.
    pub monitor: MonitorMode,
}

/// The quietest a fader goes before it means silence.
///
/// Chosen to match what the mixer's own scale shows: below this a fader is
/// off, and clamping here rather than at some smaller number keeps the two
/// from disagreeing about where that is.
pub const MIN_GAIN_DB: f32 = -60.0;

/// The loudest a fader goes, which is above unity: a quiet source is a real
/// thing and turning it up is what a fader is for.
///
/// It used to stop at unity so that the fader and the level meter beside it
/// could share one set of numbers. They no longer do — a fader is what you
/// ask for and a meter is what you got, and every physical mixer draws them
/// as two scales for exactly that reason. What replaces the shared axis is a
/// unity mark on the fader itself, so 0 dB is somewhere you can find rather
/// than somewhere you have to read the number to know you are at.
///
/// Twelve because the meter is what says whether a boost was too much, and it
/// only has 60 dB of scale to say it in. A range large enough to bury the
/// meter would be a control whose effect you cannot see.
pub const MAX_GAIN_DB: f32 = 12.0;
