//! Which hotkeys are down, from which keys are.
//!
//! One rule for every source of key state — the global listener asking the
//! operating system, and the window asking egui — so a key means the same
//! thing whichever of them saw it.
//!
//! # Edges from states
//!
//! Asked for what is down rather than told what was pressed, because the
//! global listener has only the first to go on: the operating system answers
//! "is this key down now", not "what happened". Going down is a press and
//! coming up a release, which is also what makes a held key one press rather
//! than a stream of repeats.
//!
//! # Exact to start, loose to keep
//!
//! A hotkey goes down only when its key does with exactly its modifiers —
//! `Ctrl+R` is not `Ctrl+Shift+R`, or the second could never be bound to
//! anything else. Once down it stays down for as long as its *key* is held,
//! whatever the modifiers do. That is for push-to-talk: a player holding `V`
//! to talk who presses Shift to run must not be cut off mid-sentence.

use std::collections::HashSet;

use eframe::egui::Key;

use super::{Chord, Hotkey};

/// What a keyboard says about its keys right now.
pub trait Keyboard {
    fn is_down(&self, key: Key) -> bool;
    /// Ctrl, Shift and Alt, as held right now.
    fn modifiers(&self) -> (bool, bool, bool);
}

/// One hotkey going down or coming back up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Edge {
    pub hotkey: Hotkey,
    pub pressed: bool,
}

/// The hotkeys that are down, carried from one look at the keyboard to the
/// next.
#[derive(Debug, Default)]
pub struct Tracker {
    down: HashSet<Hotkey>,
}

impl Tracker {
    /// Looks at `keyboard` again and says what changed since last time.
    ///
    /// `arm` is whether a hotkey may go down now: `false` while something is
    /// taking typed input, so a name being typed produces its letters rather
    /// than whatever they are bound to. Releases are reported either way — a
    /// key held into a text field must still let go when it comes up.
    ///
    /// A hotkey that is down and no longer bound, or bound to another key, is
    /// released: what it was doing ends with the binding that started it.
    pub fn update(
        &mut self,
        bound: &[(Hotkey, Chord)],
        keyboard: &impl Keyboard,
        arm: bool,
    ) -> Vec<Edge> {
        let mut edges = Vec::new();
        self.down.retain(|hotkey| {
            let held = bound
                .iter()
                .any(|(other, chord)| other == hotkey && keyboard.is_down(chord.key));
            if !held {
                edges.push(Edge {
                    hotkey: *hotkey,
                    pressed: false,
                });
            }
            held
        });
        if !arm {
            return edges;
        }
        let modifiers = keyboard.modifiers();
        for &(hotkey, chord) in bound {
            if !self.down.contains(&hotkey)
                && keyboard.is_down(chord.key)
                && (chord.ctrl, chord.shift, chord.alt) == modifiers
            {
                self.down.insert(hotkey);
                edges.push(Edge {
                    hotkey,
                    pressed: true,
                });
            }
        }
        edges
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::AudioSourceId;
    use crate::hotkey::HotkeyAction;

    #[derive(Default)]
    struct Keys {
        down: Vec<Key>,
        ctrl: bool,
        shift: bool,
    }

    impl Keyboard for Keys {
        fn is_down(&self, key: Key) -> bool {
            self.down.contains(&key)
        }

        fn modifiers(&self) -> (bool, bool, bool) {
            (self.ctrl, self.shift, false)
        }
    }

    const RECORD: Hotkey = Hotkey::Action(HotkeyAction::ToggleRecording);
    const TALK: Hotkey = Hotkey::PushToTalk(AudioSourceId(2));

    fn pressed(hotkey: Hotkey) -> Edge {
        Edge {
            hotkey,
            pressed: true,
        }
    }

    fn released(hotkey: Hotkey) -> Edge {
        Edge {
            hotkey,
            pressed: false,
        }
    }

    /// Down once, held, up once — never a press per look.
    #[test]
    fn a_held_key_is_one_press_and_one_release() {
        let bound = [(RECORD, Chord::ctrl(Key::R))];
        let mut tracker = Tracker::default();
        let held = Keys {
            down: vec![Key::R],
            ctrl: true,
            ..Keys::default()
        };
        assert_eq!(tracker.update(&bound, &held, true), [pressed(RECORD)]);
        assert!(tracker.update(&bound, &held, true).is_empty());
        assert_eq!(
            tracker.update(&bound, &Keys::default(), true),
            [released(RECORD)]
        );
    }

    /// Exact to start: an extra modifier is a different chord.
    #[test]
    fn an_extra_modifier_does_not_start_it() {
        let bound = [(RECORD, Chord::ctrl(Key::R))];
        let keys = Keys {
            down: vec![Key::R],
            ctrl: true,
            shift: true,
        };
        assert!(Tracker::default().update(&bound, &keys, true).is_empty());
    }

    /// Loose to keep: Shift pressed while talking does not cut the talking
    /// off — see this module's own docs.
    #[test]
    fn a_modifier_pressed_while_held_does_not_let_go() {
        let bound = [(TALK, Chord::plain(Key::V))];
        let mut tracker = Tracker::default();
        let talking = Keys {
            down: vec![Key::V],
            ..Keys::default()
        };
        assert_eq!(tracker.update(&bound, &talking, true), [pressed(TALK)]);
        let running = Keys {
            shift: true,
            ..talking
        };
        assert!(tracker.update(&bound, &running, true).is_empty());
    }

    /// Typing arms nothing, and lets go of what was already held.
    #[test]
    fn nothing_goes_down_while_typing_but_everything_comes_up() {
        let bound = [(TALK, Chord::plain(Key::V))];
        let mut tracker = Tracker::default();
        let v = Keys {
            down: vec![Key::V],
            ..Keys::default()
        };
        assert!(
            tracker.update(&bound, &v, false).is_empty(),
            "typed, not bound"
        );

        assert_eq!(tracker.update(&bound, &v, true), [pressed(TALK)]);
        assert_eq!(
            tracker.update(&bound, &Keys::default(), false),
            [released(TALK)]
        );
    }

    /// A binding taken away while its key is held ends what it started.
    #[test]
    fn a_hotkey_unbound_while_down_is_released() {
        let mut tracker = Tracker::default();
        let v = Keys {
            down: vec![Key::V],
            ..Keys::default()
        };
        tracker.update(&[(TALK, Chord::plain(Key::V))], &v, true);
        assert_eq!(tracker.update(&[], &v, true), [released(TALK)]);
    }
}
