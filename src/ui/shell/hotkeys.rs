//! Keys that do what a control elsewhere does.
//!
//! Almost nothing here is reachable only from a key: recording, streaming,
//! a channel's mute and a Scene are all buttons or rows in a dock, and what
//! a key adds is reach — from a closed dock, or from inside a game. The
//! exceptions are push-to-talk and push-to-mute, which are a key by nature:
//! they last exactly as long as it is held.
//!
//! # Two rules the whole layer depends on
//!
//! A key is ignored while anything in this window is taking typed input.
//! Renaming a Scene and pressing `r` must produce an `r`, not a recording,
//! and there is no chord this could reserve that a name might not contain.
//!
//! A held key repeats, and a repeat is not a press. Without that distinction
//! leaning on `Ctrl+R` starts and stops a recording sixty times a second.
//!
//! # Who is listening
//!
//! Where a global listener runs — see [`crate::hotkey::global`] — every
//! hotkey but the window's own two comes from it, whichever application has
//! focus, and reaches [`act`] from the application's `logic` pass, which runs
//! even while the window is minimised. Where none runs, this module asks
//! egui with the same rule, and the keys work while the window has focus.
//!
//! Fullscreen and Settings are always the window's, and so is the Scene
//! convention: `Ctrl+1` through `Ctrl+9` select by position in the list,
//! which is not a binding — a key per Scene is [`Hotkey::Scene`].

use std::collections::{HashMap, HashSet};

use eframe::egui::{self, Key, Modifiers};

use crate::domain::AudioSourceId;
use crate::hotkey::tracker::{Edge, Keyboard, Tracker};
use crate::hotkey::{Chord, Hotkey, HotkeyAction, HotkeySettings};
use crate::project::{AudioCommand, ProjectCommand, SceneCommand};
use crate::snapshots::Snapshots;

use super::{UiAction, UiState};

/// What the hotkeys remember between passes.
#[derive(Debug, Default)]
pub(in crate::ui) struct HotkeyState {
    /// What the window saw, where no global listener is looking.
    tracker: Tracker,
    /// The held hotkeys that are down right now, from whichever listener.
    held: HashSet<Hotkey>,
    /// What each channel was last told about being silenced by a key, so it
    /// is told only when that changes.
    silenced: HashMap<AudioSourceId, bool>,
}

/// Whether this window is taking typed input, for the global listener to
/// stand down on — see this module's first rule. Only while it has focus: a
/// text field left focused in a window behind a game is taking nothing.
pub fn keyboard_taken(ctx: &egui::Context, state: &UiState) -> bool {
    let focused = ctx.input(|input| input.viewport().focused.unwrap_or(true));
    focused && (ctx.egui_wants_keyboard_input() || state.settings.capturing_hotkey())
}

/// The window's own keyboard, for the tracker.
struct WindowKeys<'a>(&'a egui::InputState);

impl Keyboard for WindowKeys<'_> {
    /// Down now, or pressed at all since the last pass: a tap that went
    /// down and up between two passes is gone from `key_down` by the time
    /// this looks, and is still a press.
    fn is_down(&self, key: Key) -> bool {
        self.0.key_down(key) || self.0.key_pressed(key)
    }

    fn modifiers(&self) -> (bool, bool, bool) {
        let held = self.0.modifiers;
        (held.ctrl || held.command, held.shift, held.alt)
    }
}

/// Turns hotkeys that went down or came up into what they do.
///
/// Called with nothing as well — every pass, from the application's `logic`
/// — because what push-to-talk silences follows the bindings as much as the
/// keys: binding one silences its channel before any key is touched.
pub fn act(
    state: &mut UiState,
    snapshots: &Snapshots,
    bindings: &HotkeySettings,
    edges: &[Edge],
    actions: &mut Vec<UiAction>,
) {
    let hotkeys = &mut state.hotkeys;
    for edge in edges {
        if edge.hotkey.is_held() {
            if edge.pressed {
                hotkeys.held.insert(edge.hotkey);
            } else {
                hotkeys.held.remove(&edge.hotkey);
            }
            continue;
        }
        if !edge.pressed {
            continue;
        }
        pressed_action(edge.hotkey, snapshots, actions);
    }
    silence(hotkeys, bindings, actions);
}

/// What one hotkey going down does, decided by what is running — the same
/// way the button it stands for decides.
fn pressed_action(hotkey: Hotkey, snapshots: &Snapshots, actions: &mut Vec<UiAction>) {
    let status = &snapshots.status;
    match hotkey {
        Hotkey::Action(HotkeyAction::ToggleRecording) => {
            // One key for both, because one button does both: what it does
            // next is whatever the Controls dock would say it does.
            actions.push(if status.recording_elapsed.is_some() {
                UiAction::StopRecording
            } else {
                UiAction::StartRecording
            });
        }
        // Only while there is a recording to pause. Outside one the key does
        // nothing rather than arming something for later.
        Hotkey::Action(HotkeyAction::TogglePause) => {
            if status.recording_elapsed.is_some() {
                actions.push(UiAction::SetRecordingPaused(!status.recording_paused));
            }
        }
        Hotkey::Action(HotkeyAction::ToggleStreaming) => {
            actions.push(if status.streaming_elapsed.is_some() {
                UiAction::StopStreaming
            } else {
                UiAction::StartStreaming
            });
        }
        // The window's own, which never come through here — see `dispatch`.
        Hotkey::Action(HotkeyAction::Fullscreen | HotkeyAction::OpenSettings) => {}
        Hotkey::ToggleMute(id) => {
            // Against what the mute button shows now, as clicking it would
            // be; a channel the project no longer has is left alone.
            if let Some(channel) = snapshots
                .audio
                .items
                .iter()
                .find(|channel| channel.id == id)
            {
                actions.push(UiAction::Project(ProjectCommand::Audio(
                    AudioCommand::SetMuted(id, !channel.muted),
                )));
            }
        }
        Hotkey::Scene(id) => {
            if snapshots.scenes.items.iter().any(|scene| scene.id == id) {
                actions.push(UiAction::Project(ProjectCommand::Scene(
                    SceneCommand::Select(id),
                )));
            }
        }
        Hotkey::PushToTalk(_) | Hotkey::PushToMute(_) => {}
    }
}

/// Tells each channel with a push-to-talk or push-to-mute key whether that
/// key is silencing it now — and one whose key was taken away that it is
/// not.
///
/// Push-to-talk silences a channel except while held; push-to-mute, only
/// while held. A channel with both is silent unless talk is held and mute
/// is not, which is what anyone holding both could mean.
fn silence(hotkeys: &mut HotkeyState, bindings: &HotkeySettings, actions: &mut Vec<UiAction>) {
    let mut wanted: HashMap<AudioSourceId, bool> = HashMap::new();
    for (hotkey, _) in bindings.bound() {
        let (id, silencing) = match hotkey {
            Hotkey::PushToTalk(id) => (id, !hotkeys.held.contains(&hotkey)),
            Hotkey::PushToMute(id) => (id, hotkeys.held.contains(&hotkey)),
            _ => continue,
        };
        *wanted.entry(id).or_insert(false) |= silencing;
    }
    for (id, silenced) in &wanted {
        if hotkeys.silenced.get(id) != Some(silenced) {
            actions.push(UiAction::SetHotkeyMuted(*id, *silenced));
        }
    }
    // A channel whose keys were all taken away is let go of once, then
    // forgotten — the binding that silenced it is gone.
    for (id, silenced) in &hotkeys.silenced {
        if *silenced && !wanted.contains_key(id) {
            actions.push(UiAction::SetHotkeyMuted(*id, false));
        }
    }
    hotkeys.silenced = wanted;
}

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

/// The keys the window hears for itself, run before anything is drawn.
///
/// `global` is whether a global listener is running, in which case every
/// hotkey but the window's own comes from it instead — see this module's
/// docs — and hearing them here too would do each twice.
pub fn dispatch(
    ctx: &egui::Context,
    state: &mut UiState,
    snapshots: &Snapshots,
    bindings: &HotkeySettings,
    global: bool,
    actions: &mut Vec<UiAction>,
) {
    // Text first: a field with focus owns the keyboard, whatever the chord.
    // The same goes for the settings page while it is waiting for a key to
    // bind — a chord spent here is one that never reaches what asked for it.
    let typing = ctx.egui_wants_keyboard_input() || state.settings.capturing_hotkey();

    if !global {
        let bound: Vec<(Hotkey, Chord)> = bindings
            .bound()
            .into_iter()
            .filter(|(hotkey, _)| hotkey.is_global())
            .collect();
        let tracker = &mut state.hotkeys.tracker;
        let edges = ctx.input(|input| tracker.update(&bound, &WindowKeys(input), !typing));
        act(state, snapshots, bindings, &edges, actions);
    }
    if typing {
        return;
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
        let pass = vec![(key, true, modifiers)];
        run(vec![pass; frames], snapshots, bindings, false)
    }

    /// One pass per entry of `passes`, each sending its keys going down or
    /// coming up, through one `Context` — and what each pass dispatched.
    fn run(
        passes: Vec<Vec<(Key, bool, Modifiers)>>,
        snapshots: &Snapshots,
        bindings: &HotkeySettings,
        global: bool,
    ) -> Vec<Vec<UiAction>> {
        let context = egui::Context::default();
        let mut state = UiState::default();
        passes
            .into_iter()
            .map(|keys: Vec<(Key, bool, Modifiers)>| {
                // What is held is its own event, not read off the keys':
                // egui keeps it from `ModifiersChanged`, as a real window
                // sends it.
                let held = keys
                    .iter()
                    .map(|(_, _, modifiers)| *modifiers)
                    .fold(Modifiers::NONE, |held, more| held | more);
                let input = egui::RawInput {
                    events: std::iter::once(egui::Event::ModifiersChanged(held))
                        .chain(
                            keys.into_iter()
                                .map(|(key, pressed, modifiers)| egui::Event::Key {
                                    key,
                                    physical_key: None,
                                    pressed,
                                    repeat: false,
                                    modifiers,
                                }),
                        )
                        .collect(),
                    ..Default::default()
                };
                let mut actions = Vec::new();
                let mut output = context.run_ui(input, |context| {
                    egui::CentralPanel::default().show(context, |ui| {
                        dispatch(
                            ui.ctx(),
                            &mut state,
                            snapshots,
                            bindings,
                            global,
                            &mut actions,
                        );
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

    /// Push-to-talk: silent before the key is ever touched, heard while it is
    /// held, silent again once let go — and told only when that changes.
    #[test]
    fn push_to_talk_is_heard_only_while_held() {
        let mic = AudioSourceId(2);
        let mut bindings = HotkeySettings::default();
        bindings.set(Hotkey::PushToTalk(mic), Some(Chord::plain(Key::V)));
        let down = (Key::V, true, Modifiers::NONE);
        let up = (Key::V, false, Modifiers::NONE);

        let passes = run(
            vec![vec![], vec![down], vec![], vec![up], vec![]],
            &Snapshots::default(),
            &bindings,
            false,
        );
        assert_eq!(
            passes,
            [
                vec![UiAction::SetHotkeyMuted(mic, true)],
                vec![UiAction::SetHotkeyMuted(mic, false)],
                vec![],
                vec![UiAction::SetHotkeyMuted(mic, true)],
                vec![],
            ]
        );
    }

    /// Push-to-mute is the other way round, and a channel with no key at all
    /// is never told anything.
    #[test]
    fn push_to_mute_is_silent_only_while_held() {
        let mic = AudioSourceId(2);
        let mut bindings = HotkeySettings::default();
        bindings.set(Hotkey::PushToMute(mic), Some(Chord::plain(Key::B)));

        let passes = run(
            vec![
                vec![],
                vec![(Key::B, true, Modifiers::NONE)],
                vec![(Key::B, false, Modifiers::NONE)],
            ],
            &Snapshots::default(),
            &bindings,
            false,
        );
        assert_eq!(
            passes,
            [
                vec![UiAction::SetHotkeyMuted(mic, false)],
                vec![UiAction::SetHotkeyMuted(mic, true)],
                vec![UiAction::SetHotkeyMuted(mic, false)],
            ]
        );
    }

    /// A channel's mute key does what its mute button would, against what
    /// the button shows now.
    #[test]
    fn the_mute_key_flips_what_the_button_shows() {
        let mic = AudioSourceId(2);
        let mut bindings = HotkeySettings::default();
        bindings.set(Hotkey::ToggleMute(mic), Some(Chord::ctrl(Key::M)));
        let mut snapshots = Snapshots::default();
        snapshots.audio.items = vec![crate::snapshots::AudioSourceSnapshot {
            id: mic,
            name: "Microphone".into(),
            kind: crate::domain::AudioSourceKind::Input,
            device: None,
            gain_db: 0.0,
            muted: true,
            monitored: false,
            peak_db: None,
            running: true,
            filters: Vec::new(),
        }];

        assert_eq!(
            hold_bound(Key::M, Modifiers::CTRL, 1, &snapshots, &bindings).remove(0),
            [UiAction::Project(ProjectCommand::Audio(
                AudioCommand::SetMuted(mic, false)
            ))]
        );
    }

    /// A Scene's own key selects it wherever it sits in the list.
    #[test]
    fn a_scenes_own_key_selects_it() {
        let mut snapshots = Snapshots::default();
        snapshots.scenes.items = vec![SceneSnapshot {
            id: SceneId(9),
            name: "last".into(),
        }];
        let mut bindings = HotkeySettings::default();
        bindings.set(Hotkey::Scene(SceneId(9)), Some(Chord::plain(Key::F5)));

        assert_eq!(
            hold_bound(Key::F5, Modifiers::NONE, 1, &snapshots, &bindings).remove(0),
            [UiAction::Project(ProjectCommand::Scene(
                SceneCommand::Select(SceneId(9))
            ))]
        );
    }

    /// With a global listener running, the window leaves every global
    /// hotkey to it — hearing one here too would do it twice — and keeps
    /// its own.
    #[test]
    fn with_a_global_listener_the_window_keeps_only_its_own_keys() {
        let idle = recording_for(None);
        let bindings = HotkeySettings::default();
        assert!(
            run(
                vec![vec![(Key::R, true, Modifiers::CTRL)]],
                &idle,
                &bindings,
                true
            )
            .remove(0)
            .is_empty()
        );
        assert_eq!(
            run(
                vec![vec![(Key::Comma, true, Modifiers::CTRL)]],
                &idle,
                &bindings,
                true
            )
            .remove(0),
            [UiAction::OpenSettings]
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
