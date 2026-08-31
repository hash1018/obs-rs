//! What a key binding is, and what this application binds.
//!
//! Separate from both halves that use it: [`crate::settings`] stores these,
//! `ui::shell::hotkeys` acts on them, and neither is where the meaning of
//! `Ctrl+Shift+F9` belongs.
//!
//! # One spelling, read loosely and written strictly
//!
//! A binding is stored as the string a person would write — `Ctrl+R`, `F11`,
//! `Ctrl+,` — because the settings file is something they can open and edit.
//! Reading is forgiving about case and about which spelling of a key it is
//! given, since egui answers to both `Comma` and `,`; writing always produces
//! the short form, so a file edited by hand comes back tidy.
//!
//! The key names are egui's own rather than a table kept here. A table would
//! be one more thing to keep in step with the keys egui actually reports, and
//! this has no reason to disagree with it.

use std::fmt;
use std::str::FromStr;

use eframe::egui::{Key, Modifiers};
use serde::{Deserialize, Serialize};

/// One key and the modifiers held with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chord {
    pub key: Key,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

impl Chord {
    /// A chord with no modifiers, for the keys that need none.
    pub const fn plain(key: Key) -> Self {
        Self {
            key,
            ctrl: false,
            shift: false,
            alt: false,
        }
    }

    pub const fn ctrl(key: Key) -> Self {
        Self {
            key,
            ctrl: true,
            shift: false,
            alt: false,
        }
    }

    /// What was actually pressed, or `None` for a press this cannot bind.
    ///
    /// A modifier alone is not a chord — holding Ctrl while choosing a
    /// binding is how the *next* key gets its modifier, not a binding of its
    /// own — and Escape is how a caller says never mind.
    pub fn from_press(key: Key, modifiers: Modifiers) -> Option<Self> {
        if key == Key::Escape {
            return None;
        }
        Some(Self {
            key,
            ctrl: modifiers.ctrl || modifiers.command,
            shift: modifiers.shift,
            alt: modifiers.alt,
        })
    }

    /// The modifiers this chord requires, as a pattern to match against.
    ///
    /// `command` is left unset even for a chord that wants Ctrl. It is
    /// egui's name for "the platform's own modifier", and a pattern asking
    /// for both would only match an event carrying both — where asking for
    /// `ctrl` alone matches whether or not the backend also set `command`,
    /// which is the rule egui documents and the one a binding wants.
    pub fn modifiers(self) -> Modifiers {
        Modifiers {
            ctrl: self.ctrl,
            shift: self.shift,
            alt: self.alt,
            command: false,
            mac_cmd: false,
        }
    }
}

impl fmt::Display for Chord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Ctrl, Alt, Shift, in the order every platform writes them.
        for (held, name) in [
            (self.ctrl, "Ctrl"),
            (self.alt, "Alt"),
            (self.shift, "Shift"),
        ] {
            if held {
                write!(formatter, "{name}+")?;
            }
        }
        write!(formatter, "{}", self.key.symbol_or_name())
    }
}

/// Why a written binding could not be read.
///
/// Spelled out by hand rather than derived: this crate has no error-derive
/// dependency, and three variants do not justify one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChordError {
    NoKey,
    UnknownKey(String),
    UnknownModifier(String),
}

impl fmt::Display for ChordError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoKey => write!(formatter, "a binding needs a key, not only modifiers"),
            Self::UnknownKey(name) => write!(formatter, "`{name}` is not a key this can bind"),
            Self::UnknownModifier(name) => write!(formatter, "`{name}` is not a modifier"),
        }
    }
}

impl std::error::Error for ChordError {}

impl FromStr for Chord {
    type Err = ChordError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let mut parts: Vec<&str> = text.split('+').map(str::trim).collect();
        // The key is whatever is last — except that the separator is also a
        // key. `Ctrl++` splits into three parts ending in two empty ones,
        // where `Ctrl+` ends in one and is missing its key entirely; the
        // second empty part is what tells those apart.
        let last = parts.pop();
        let name = match last {
            Some(last) if !last.is_empty() => last,
            Some(_) if parts.last().is_some_and(|part| part.is_empty()) => {
                parts.pop();
                "+"
            }
            _ => return Err(ChordError::NoKey),
        };
        let key = Key::from_name(name)
            .or_else(|| Key::from_name(&name.to_ascii_uppercase()))
            .ok_or_else(|| ChordError::UnknownKey(name.to_owned()))?;

        let mut chord = Self::plain(key);
        for part in parts {
            match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" | "cmd" | "command" => chord.ctrl = true,
                "shift" => chord.shift = true,
                "alt" | "option" => chord.alt = true,
                _ => return Err(ChordError::UnknownModifier(part.to_owned())),
            }
        }
        Ok(chord)
    }
}

/// A binding, which may be nothing.
///
/// Nothing has to be storable: a user who clears a binding means it, and a
/// setting left out of the file is one the defaults put straight back. So an
/// empty string is written, and read as "bound to nothing".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Binding(pub Option<Chord>);

impl From<Chord> for Binding {
    fn from(chord: Chord) -> Self {
        Self(Some(chord))
    }
}

impl TryFrom<String> for Binding {
    type Error = ChordError;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        if text.trim().is_empty() {
            return Ok(Self(None));
        }
        text.parse().map(|chord| Self(Some(chord)))
    }
}

impl From<Binding> for String {
    fn from(binding: Binding) -> Self {
        binding.0.map(|chord| chord.to_string()).unwrap_or_default()
    }
}

/// Everything a key can be bound to.
///
/// Switching Scenes is deliberately not here. `Ctrl+1` through `Ctrl+9`
/// select by *position* in the list, which is a convention rather than a
/// binding: a per-Scene key — the model OBS uses, and the one that survives
/// reordering — belongs with per-Source bindings, and neither is this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyAction {
    ToggleRecording,
    TogglePause,
    Fullscreen,
    OpenSettings,
}

impl HotkeyAction {
    /// Every action, in the order the settings page lists them.
    pub const ALL: [Self; 4] = [
        Self::ToggleRecording,
        Self::TogglePause,
        Self::Fullscreen,
        Self::OpenSettings,
    ];
}

/// What each action is bound to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct HotkeySettings {
    pub toggle_recording: Binding,
    pub toggle_pause: Binding,
    pub fullscreen: Binding,
    pub open_settings: Binding,
}

impl Default for HotkeySettings {
    fn default() -> Self {
        Self {
            toggle_recording: Chord::ctrl(Key::R).into(),
            toggle_pause: Chord::ctrl(Key::P).into(),
            fullscreen: Chord::plain(Key::F11).into(),
            open_settings: Chord::ctrl(Key::Comma).into(),
        }
    }
}

impl HotkeySettings {
    pub fn binding(&self, action: HotkeyAction) -> Option<Chord> {
        match action {
            HotkeyAction::ToggleRecording => self.toggle_recording.0,
            HotkeyAction::TogglePause => self.toggle_pause.0,
            HotkeyAction::Fullscreen => self.fullscreen.0,
            HotkeyAction::OpenSettings => self.open_settings.0,
        }
    }

    pub fn set(&mut self, action: HotkeyAction, chord: Option<Chord>) {
        let binding = Binding(chord);
        match action {
            HotkeyAction::ToggleRecording => self.toggle_recording = binding,
            HotkeyAction::TogglePause => self.toggle_pause = binding,
            HotkeyAction::Fullscreen => self.fullscreen = binding,
            HotkeyAction::OpenSettings => self.open_settings = binding,
        }
    }

    /// Which *other* action already holds `chord`.
    ///
    /// Said rather than prevented. Refusing the assignment would leave the
    /// user holding a key they cannot use and no way to see why; the page
    /// takes it and names the other action beside it, which is the same
    /// information without the dead end. What actually happens if one is left
    /// standing is that the first action listed takes the key and the second
    /// never sees it — which is exactly what the warning is about.
    pub fn conflict(&self, action: HotkeyAction, chord: Chord) -> Option<HotkeyAction> {
        HotkeyAction::ALL
            .into_iter()
            .find(|&other| other != action && self.binding(other) == Some(chord))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_binding_survives_being_written_down_and_read_back() {
        for text in ["Ctrl+R", "F11", "Ctrl+,", "Ctrl+Alt+Shift+F9", "Space"] {
            let chord: Chord = text.parse().expect("a chord this writes must parse");
            assert_eq!(chord.to_string(), text, "round trip of {text}");
        }
    }

    /// A file somebody edited by hand, in the spellings they would use.
    #[test]
    fn a_binding_is_read_loosely() {
        assert_eq!("ctrl+r".parse::<Chord>(), Ok(Chord::ctrl(Key::R)));
        assert_eq!("CTRL + R".parse::<Chord>(), Ok(Chord::ctrl(Key::R)));
        assert_eq!("Ctrl+Comma".parse::<Chord>(), Ok(Chord::ctrl(Key::Comma)));
        assert_eq!("Command+R".parse::<Chord>(), Ok(Chord::ctrl(Key::R)));
        // The key is whatever is last, so a bound plus key is not an empty
        // part at the end.
        assert_eq!("Ctrl++".parse::<Chord>(), Ok(Chord::ctrl(Key::Plus)));
    }

    #[test]
    fn what_cannot_be_read_says_why() {
        assert_eq!("Ctrl+".parse::<Chord>(), Err(ChordError::NoKey));
        assert_eq!(
            "Ctrl+Nonsense".parse::<Chord>(),
            Err(ChordError::UnknownKey("Nonsense".to_owned()))
        );
        assert_eq!(
            "Hyper+R".parse::<Chord>(),
            Err(ChordError::UnknownModifier("Hyper".to_owned()))
        );
    }

    /// Cleared is a value, not an absence — see [`Binding`].
    #[test]
    fn an_empty_binding_round_trips_as_nothing() {
        let cleared = Binding::try_from(String::new()).expect("empty is not an error");
        assert_eq!(cleared, Binding(None));
        assert_eq!(String::from(cleared), "");
        assert_eq!(
            Binding::try_from("   ".to_owned()),
            Ok(Binding(None)),
            "a line left blank is cleared, not malformed"
        );
    }

    #[test]
    fn a_chord_another_action_holds_is_a_conflict() {
        let mut settings = HotkeySettings::default();
        assert_eq!(
            settings.conflict(HotkeyAction::TogglePause, Chord::ctrl(Key::R)),
            Some(HotkeyAction::ToggleRecording)
        );
        // Its own binding is not a conflict with itself.
        assert_eq!(
            settings.conflict(HotkeyAction::ToggleRecording, Chord::ctrl(Key::R)),
            None
        );
        settings.set(HotkeyAction::ToggleRecording, None);
        assert_eq!(
            settings.conflict(HotkeyAction::TogglePause, Chord::ctrl(Key::R)),
            None,
            "nothing holds it once it is cleared"
        );
    }

    /// Escape is how the capture widget is dismissed, so it can never be a
    /// binding — otherwise there would be no way out of choosing one.
    #[test]
    fn escape_is_not_a_binding() {
        assert_eq!(Chord::from_press(Key::Escape, Modifiers::NONE), None);
        assert_eq!(
            Chord::from_press(Key::R, Modifiers::CTRL),
            Some(Chord::ctrl(Key::R))
        );
    }
}
