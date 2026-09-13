//! The Hotkeys page: one row per action, each a button that listens.
//!
//! # Why a button that listens rather than a field to type in
//!
//! A binding is a key, and the way to say which key is to press it. Typing
//! `Ctrl+F9` into a box asks the user to spell what they could simply do,
//! and puts every misspelling in the way of a working binding.
//!
//! So the button is the state: it shows what is bound, and while it is
//! waiting it says so and takes the next key. Escape leaves without
//! changing anything, and Backspace clears the binding — the two things
//! someone in the middle of choosing might want that are not a key to bind.
//!
//! While one is waiting, the hotkey layer stands down (see
//! `shell::hotkeys::dispatch`). Otherwise `Ctrl+R` would start a recording
//! on its way to being bound to something.

use eframe::egui;

use crate::hotkey::{Chord, Hotkey, HotkeyAction};
use crate::i18n::{LocalizationManager, TextKey};
use crate::settings::AppSettings;
use crate::snapshots::{AudioSnapshot, ScenesSnapshot};

/// What the page reports back, since the capture it starts outlives the pass
/// that drew it.
pub(super) struct HotkeyPageOutcome {
    /// The hotkey now waiting for a key, if the user just asked for one.
    pub(super) capture: Option<Hotkey>,
    /// Whether a key arrived and the wait is over.
    pub(super) captured: bool,
}

/// How tall the list of rows gets before it scrolls: a project with a dozen
/// Scenes has more rows than the dialog has room for.
const LIST_HEIGHT: f32 = 380.0;

pub(super) fn show(
    ui: &mut egui::Ui,
    draft: &mut AppSettings,
    capturing: Option<Hotkey>,
    audio: &AudioSnapshot,
    scenes: &ScenesSnapshot,
    i18n: &LocalizationManager,
) -> HotkeyPageOutcome {
    let mut outcome = HotkeyPageOutcome {
        capture: None,
        captured: false,
    };

    // The key that ends a wait, taken before the rows are drawn so the
    // button that started it does not also see the click that follows.
    let pressed = capturing.and_then(|_| take_press(ui.ctx()));

    // What each section lists: the fixed actions, then three keys for every
    // mixer channel the project has — plugged in or not, since a key bound
    // now is for when it is — then one for every Scene.
    let mut sections: Vec<(TextKey, Vec<Hotkey>)> = vec![(
        TextKey::HotkeySectionGeneral,
        HotkeyAction::ALL.into_iter().map(Hotkey::Action).collect(),
    )];
    if !audio.items.is_empty() {
        sections.push((
            TextKey::HotkeySectionAudio,
            audio
                .items
                .iter()
                .flat_map(|channel| {
                    [
                        Hotkey::PushToTalk(channel.id),
                        Hotkey::PushToMute(channel.id),
                        Hotkey::ToggleMute(channel.id),
                    ]
                })
                .collect(),
        ));
    }
    if !scenes.items.is_empty() {
        sections.push((
            TextKey::HotkeySectionScenes,
            scenes
                .items
                .iter()
                .map(|scene| Hotkey::Scene(scene.id))
                .collect(),
        ));
    }
    let label = |hotkey: Hotkey| label(hotkey, audio, scenes, i18n);

    // Vertical, and bounded: the pages are drawn inside the dialog's own
    // horizontal split, so a second widget beside the grid would be laid out
    // *next to* it — and an unbounded label would then stretch the dialog to
    // the width of one sentence. Every other page is a single grid and never
    // meets either.
    ui.vertical(|ui| {
        ui.set_max_width(super::PAGE_WIDTH);
        egui::ScrollArea::vertical()
            .id_salt("settings_hotkeys_list")
            .max_height(LIST_HEIGHT)
            .show(ui, |ui| {
                for (index, (heading, hotkeys)) in sections.iter().enumerate() {
                    ui.strong(i18n.text(*heading));
                    egui::Grid::new(("settings_hotkeys", index))
                        .num_columns(2)
                        .spacing([12.0, 8.0])
                        .show(ui, |ui| {
                            for &hotkey in hotkeys {
                                if row(ui, draft, hotkey, capturing, &label, i18n) {
                                    outcome.capture = Some(hotkey);
                                }
                            }
                        });
                    ui.add_space(10.0);
                }
            });

        ui.add_space(8.0);
        ui.weak(i18n.text(TextKey::HotkeyHint));
    });

    if let (Some(action), Some(press)) = (capturing, pressed) {
        outcome.captured = true;
        match press {
            // Escape: leave it as it was.
            Press::Cancelled => {}
            Press::Cleared => draft.hotkeys.set(action, None),
            Press::Bound(chord) => draft.hotkeys.set(action, Some(chord)),
        }
    }
    outcome
}

/// What the key that ended a wait meant.
enum Press {
    Bound(Chord),
    Cleared,
    Cancelled,
}

/// The next key press, whatever it was — consumed, so nothing else acts on
/// the key someone was in the middle of binding.
fn take_press(ctx: &egui::Context) -> Option<Press> {
    ctx.input_mut(|input| {
        let mut press = None;
        input.events.retain(|event| {
            let egui::Event::Key {
                key,
                modifiers,
                pressed: true,
                ..
            } = event
            else {
                return true;
            };
            if press.is_some() {
                return true;
            }
            press = Some(match key {
                egui::Key::Escape => Press::Cancelled,
                egui::Key::Backspace | egui::Key::Delete => Press::Cleared,
                key => match Chord::from_press(*key, *modifiers) {
                    Some(chord) => Press::Bound(chord),
                    None => Press::Cancelled,
                },
            });
            false
        });
        press
    })
}

/// One hotkey's row: its name, and the button that shows and takes its key.
/// Answers whether the button was clicked.
fn row(
    ui: &mut egui::Ui,
    draft: &AppSettings,
    hotkey: Hotkey,
    capturing: Option<Hotkey>,
    label: &impl Fn(Hotkey) -> String,
    i18n: &LocalizationManager,
) -> bool {
    ui.label(label(hotkey));
    let waiting = capturing == Some(hotkey);
    let text = if waiting {
        i18n.text(TextKey::HotkeyPressAKey).into_owned()
    } else {
        draft.hotkeys.binding(hotkey).map_or_else(
            || i18n.text(TextKey::HotkeyNone).into_owned(),
            |chord| chord.to_string(),
        )
    };
    let clicked = ui.add(egui::Button::new(text).selected(waiting)).clicked();
    // The conflict is reported on the row that would lose, which is the one
    // the user is looking at.
    if let Some(chord) = draft.hotkeys.binding(hotkey)
        && let Some(other) = draft.hotkeys.conflict(hotkey, chord)
    {
        let mut args = fluent_bundle::FluentArgs::new();
        args.set("action", label(other));
        ui.label(
            egui::RichText::new(i18n.text_with(TextKey::HotkeyConflict, &args))
                .color(ui.visuals().warn_fg_color),
        );
    }
    ui.end_row();
    clicked
}

/// What a hotkey is called on the page — a channel's and a Scene's with
/// their own names in, as the project has them now. One whose channel or
/// Scene has gone is still named, for the conflict it can still be in.
///
/// Also what the global listener calls it where the system shows it — see
/// `hotkey::global::Bound::description` — so the dialog asking to allow a
/// shortcut names it the way this page does.
pub(crate) fn label(
    hotkey: Hotkey,
    audio: &AudioSnapshot,
    scenes: &ScenesSnapshot,
    i18n: &LocalizationManager,
) -> String {
    let named = |key: TextKey, argument: &str, name: String| {
        let mut args = fluent_bundle::FluentArgs::new();
        args.set(argument, name);
        i18n.text_with(key, &args).into_owned()
    };
    let channel = |id| {
        audio
            .items
            .iter()
            .find(|channel| channel.id == id)
            .map_or_else(|| "?".to_owned(), |channel| channel.name.clone())
    };
    match hotkey {
        Hotkey::Action(action) => i18n
            .text(match action {
                HotkeyAction::ToggleRecording => TextKey::HotkeyToggleRecording,
                HotkeyAction::TogglePause => TextKey::HotkeyTogglePause,
                HotkeyAction::ToggleStreaming => TextKey::HotkeyToggleStreaming,
                HotkeyAction::Screenshot => TextKey::HotkeyScreenshot,
                HotkeyAction::Fullscreen => TextKey::HotkeyFullscreen,
                HotkeyAction::OpenSettings => TextKey::HotkeyOpenSettings,
            })
            .into_owned(),
        Hotkey::PushToTalk(id) => named(TextKey::HotkeyPushToTalk, "channel", channel(id)),
        Hotkey::PushToMute(id) => named(TextKey::HotkeyPushToMute, "channel", channel(id)),
        Hotkey::ToggleMute(id) => named(TextKey::HotkeyToggleMute, "channel", channel(id)),
        Hotkey::Scene(id) => named(
            TextKey::HotkeySwitchScene,
            "scene",
            scenes
                .items
                .iter()
                .find(|scene| scene.id == id)
                .map_or_else(|| "?".to_owned(), |scene| scene.name.clone()),
        ),
    }
}
