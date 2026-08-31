//! Keys that do what a control elsewhere does.
//!
//! Nothing here is reachable only from a key: every one of these is a button,
//! a menu item, or a row in a dock. What a key adds is reach — the Controls
//! dock can be closed, and a recording has to be startable and stoppable
//! without hunting for the thing that was put away.
//!
//! # Two rules the whole layer depends on
//!
//! A key is ignored while anything is taking typed input. Renaming a Scene
//! and pressing `r` must produce an `r`, not a recording, and there is no
//! chord this could reserve that a name might not contain.
//!
//! A held key repeats, and a repeat is not a press. Without that distinction
//! leaning on `Ctrl+R` starts and stops a recording sixty times a second.
//!
//! # Bound where they are stored, and not global
//!
//! Four of these come from settings, which is where the user can change
//! them. The Scene keys do not: `Ctrl+1` through `Ctrl+9` select by position
//! in the list, which is a convention rather than a binding — see
//! [`crate::hotkey::HotkeyAction`] for why a per-Scene key is a different
//! model rather than nine more rows.
//!
//! Global hotkeys — keys that work while another application has focus,
//! which is what a recorder eventually wants — are a different mechanism per
//! platform (`RegisterHotKey` behind a message pump of its own on Windows,
//! the `GlobalShortcuts` portal or `XGrabKey` on Linux) and not this.

use eframe::egui::{self, Key, Modifiers};

use crate::hotkey::{Chord, HotkeyAction, HotkeySettings};
use crate::project::{ProjectCommand, SceneCommand};
use crate::snapshots::Snapshots;

use super::{UiAction, UiState};

/// How many Scenes a key can reach. `Ctrl+1` through `Ctrl+9`, because the
/// tenth would be `Ctrl+0` and reading that as "ten" is a guess.
const SCENE_KEYS: [Key; 9] = [
    Key::Num1,
    Key::Num2,
    Key::Num3,
    Key::Num4,
    Key::Num5,
    Key::Num6,
    Key::Num7,
    Key::Num8,
    Key::Num9,
];

pub fn dispatch(
    ctx: &egui::Context,
    state: &mut UiState,
    snapshots: &Snapshots,
    bindings: &HotkeySettings,
    actions: &mut Vec<UiAction>,
) {
    // Text first: a field with focus owns the keyboard, whatever the chord.
    // The same goes for the settings page while it is waiting for a key to
    // bind — a chord spent here is one that never reaches what asked for it.
    if ctx.egui_wants_keyboard_input() || state.settings.capturing_hotkey() {
        return;
    }

    let recording = snapshots.status.recording_elapsed.is_some();
    if bound(ctx, bindings, HotkeyAction::ToggleRecording) {
        // One key for both, because one button does both: what it does next
        // is whatever the Controls dock would say it does.
        actions.push(if recording {
            UiAction::StopRecording
        } else {
            UiAction::StartRecording
        });
    }
    // Only while there is a recording to pause. Outside one the key does
    // nothing rather than arming something for later.
    if recording && bound(ctx, bindings, HotkeyAction::TogglePause) {
        actions.push(UiAction::SetRecordingPaused(
            !snapshots.status.recording_paused,
        ));
    }

    for (index, key) in SCENE_KEYS.iter().enumerate() {
        if !pressed(ctx, Modifiers::CTRL, *key) {
            continue;
        }
        // Nothing for a Scene that is not there: a project with two Scenes
        // has no third to select, and inventing one is worse than silence.
        if let Some(scene) = snapshots.scenes.items.get(index) {
            actions.push(UiAction::Project(ProjectCommand::Scene(
                SceneCommand::Select(scene.id),
            )));
        }
    }

    if bound(ctx, bindings, HotkeyAction::Fullscreen) {
        state.fullscreen = !state.fullscreen;
        actions.push(UiAction::SetFullscreen(state.fullscreen));
    }
    if bound(ctx, bindings, HotkeyAction::OpenSettings) {
        actions.push(UiAction::OpenSettings);
    }
}

/// Whether whatever `action` is bound to was pressed. An action bound to
/// nothing is never pressed, which is what clearing a binding means.
fn bound(ctx: &egui::Context, bindings: &HotkeySettings, action: HotkeyAction) -> bool {
    bindings
        .binding(action)
        .is_some_and(|chord: Chord| pressed(ctx, chord.modifiers(), chord.key))
}

/// Whether this chord was pressed — and consumed, so nothing drawn later
/// sees it as its own.
///
/// Exact modifiers, not egui's `matches_logically`: that treats extra Shift
/// and Alt as noise, which would leave `Ctrl+Shift+R` starting a recording
/// and no way to ever bind it to anything else.
///
/// `repeat: false` is the other half — see this module's own docs.
fn pressed(ctx: &egui::Context, modifiers: Modifiers, key: Key) -> bool {
    ctx.input_mut(|input| {
        let mut hit = false;
        input.events.retain(|event| {
            let matched = matches!(
                event,
                egui::Event::Key {
                    key: event_key,
                    modifiers: event_modifiers,
                    pressed: true,
                    repeat: false,
                    ..
                } if *event_key == key && event_modifiers.matches_exact(modifiers)
            );
            hit |= matched;
            !matched
        });
        hit
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::domain::SceneId;
    use crate::snapshots::SceneSnapshot;

    /// The same key held down for `frames` passes of one `Context`, and
    /// what each pass dispatched.
    ///
    /// One `Context` throughout, because the flag that says a press is a
    /// repeat is not the caller's to set: egui rewrites it from its own
    /// record of which keys are down (`*repeat = !first_press`), so a key
    /// only repeats by being sent again without a release in between.
    fn hold(
        key: Key,
        modifiers: Modifiers,
        frames: usize,
        snapshots: &Snapshots,
    ) -> Vec<Vec<UiAction>> {
        hold_bound(
            key,
            modifiers,
            frames,
            snapshots,
            &HotkeySettings::default(),
        )
    }

    /// The same, against bindings a test chose.
    fn hold_bound(
        key: Key,
        modifiers: Modifiers,
        frames: usize,
        snapshots: &Snapshots,
        bindings: &HotkeySettings,
    ) -> Vec<Vec<UiAction>> {
        let context = egui::Context::default();
        let mut state = UiState::default();
        (0..frames)
            .map(|_| {
                let input = egui::RawInput {
                    events: vec![egui::Event::Key {
                        key,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers,
                    }],
                    ..Default::default()
                };
                let mut actions = Vec::new();
                let mut output = context.run_ui(input, |context| {
                    egui::CentralPanel::default().show(context, |ui| {
                        dispatch(ui.ctx(), &mut state, snapshots, bindings, &mut actions);
                    });
                });
                output.textures_delta.clear();
                actions
            })
            .collect()
    }

    /// One press of one key.
    fn press(key: Key, modifiers: Modifiers, snapshots: &Snapshots) -> Vec<UiAction> {
        hold(key, modifiers, 1, snapshots).remove(0)
    }

    fn recording_for(elapsed: Option<Duration>) -> Snapshots {
        let mut snapshots = Snapshots::default();
        snapshots.status.recording_elapsed = elapsed;
        snapshots
    }

    /// One key, two meanings, decided by what is running — the same way the
    /// one button in the Controls dock decides.
    #[test]
    fn the_recording_key_starts_and_stops() {
        let idle = recording_for(None);
        assert!(matches!(
            press(Key::R, Modifiers::CTRL, &idle).as_slice(),
            [UiAction::StartRecording]
        ));

        let running = recording_for(Some(Duration::from_secs(3)));
        assert!(matches!(
            press(Key::R, Modifiers::CTRL, &running).as_slice(),
            [UiAction::StopRecording]
        ));
    }

    /// The pause key outside a recording arms nothing.
    #[test]
    fn pause_needs_something_to_pause() {
        assert!(
            press(Key::P, Modifiers::CTRL, &recording_for(None)).is_empty(),
            "nothing is recording, so there is nothing to pause"
        );
        assert!(matches!(
            press(
                Key::P,
                Modifiers::CTRL,
                &recording_for(Some(Duration::from_secs(1)))
            )
            .as_slice(),
            [UiAction::SetRecordingPaused(true)]
        ));
    }

    /// The whole point of the settings page: what the file says is what the
    /// key does.
    #[test]
    fn a_rebound_key_is_the_one_that_acts() {
        let mut bindings = HotkeySettings::default();
        bindings.set(HotkeyAction::ToggleRecording, Some(Chord::plain(Key::F9)));
        let idle = recording_for(None);

        assert!(matches!(
            hold_bound(Key::F9, Modifiers::NONE, 1, &idle, &bindings)
                .remove(0)
                .as_slice(),
            [UiAction::StartRecording]
        ));
        assert!(
            hold_bound(Key::R, Modifiers::CTRL, 1, &idle, &bindings)
                .remove(0)
                .is_empty(),
            "the key it used to be bound to does nothing now"
        );
    }

    /// Cleared means cleared — the action keeps working everywhere else, and
    /// no key reaches it.
    #[test]
    fn an_action_bound_to_nothing_has_no_key() {
        let mut bindings = HotkeySettings::default();
        bindings.set(HotkeyAction::ToggleRecording, None);
        assert!(
            hold_bound(Key::R, Modifiers::CTRL, 1, &recording_for(None), &bindings)
                .remove(0)
                .is_empty()
        );
    }

    /// A held key is one press, not sixty a second.
    #[test]
    fn a_repeat_is_not_a_press() {
        let frames = hold(Key::R, Modifiers::CTRL, 3, &recording_for(None));
        assert!(
            matches!(frames[0].as_slice(), [UiAction::StartRecording]),
            "the first press acts"
        );
        assert!(
            frames[1..].iter().all(Vec::is_empty),
            "holding it must not act again: {frames:?}"
        );
    }

    /// Extra modifiers are a different chord, not the same one with noise —
    /// otherwise `Ctrl+Shift+R` could never be bound to anything else.
    #[test]
    fn extra_modifiers_are_a_different_chord() {
        assert!(
            press(
                Key::R,
                Modifiers::CTRL | Modifiers::SHIFT,
                &recording_for(None)
            )
            .is_empty()
        );
    }

    #[test]
    fn a_scene_key_reaches_the_scene_at_its_place_and_no_further() {
        let mut snapshots = Snapshots::default();
        snapshots.scenes.items = vec![
            SceneSnapshot {
                id: SceneId(7),
                name: "first".into(),
            },
            SceneSnapshot {
                id: SceneId(9),
                name: "second".into(),
            },
        ];

        assert!(matches!(
            press(Key::Num2, Modifiers::CTRL, &snapshots).as_slice(),
            [UiAction::Project(ProjectCommand::Scene(
                SceneCommand::Select(SceneId(9))
            ))]
        ));
        assert!(
            press(Key::Num3, Modifiers::CTRL, &snapshots).is_empty(),
            "there is no third Scene to select"
        );
    }
}
