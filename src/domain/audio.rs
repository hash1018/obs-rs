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
    /// Whether this is played back to the person running obs-rs, on the
    /// endpoint chosen in Settings.
    ///
    /// It is recorded either way, and that is a decision rather than an
    /// omission. Being heard and being recorded look like two questions —
    /// OBS answers them with three states, off, heard, heard-and-recorded —
    /// but they collapse into one here, because obs-rs monitors by *playing*
    /// and captures the desktop by *listening to what is played*. A source
    /// kept out of the recording and sent to the speakers arrives back
    /// through Desktop Audio anyway, late and through a speaker. The third
    /// state is only true where the monitoring endpoint is one nothing
    /// captures, which is not the machine most people have.
    ///
    /// Per source rather than one switch for the mixer: a microphone wants
    /// this off — a voice back in your ears forty milliseconds late is hard
    /// to talk over — while the media file beside it wants it on.
    pub monitored: bool,
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
