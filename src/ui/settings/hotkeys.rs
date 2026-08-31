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

use crate::hotkey::{Chord, HotkeyAction};
use crate::i18n::{LocalizationManager, TextKey};
use crate::settings::AppSettings;

/// What the page reports back, since the capture it starts outlives the pass
/// that drew it.
pub(super) struct HotkeyPageOutcome {
    /// The action now waiting for a key, if the user just asked for one.
    pub(super) capture: Option<HotkeyAction>,
    /// Whether a key arrived and the wait is over.
    pub(super) captured: bool,
}

pub(super) fn show(
    ui: &mut egui::Ui,
    draft: &mut AppSettings,
    capturing: Option<HotkeyAction>,
    i18n: &LocalizationManager,
) -> HotkeyPageOutcome {
    let mut outcome = HotkeyPageOutcome {
        capture: None,
        captured: false,
    };

    // The key that ends a wait, taken before the rows are drawn so the
    // button that started it does not also see the click that follows.
    let pressed = capturing.and_then(|_| take_press(ui.ctx()));

    // Vertical, and bounded: the pages are drawn inside the dialog's own
    // horizontal split, so a second widget beside the grid would be laid out
    // *next to* it — and an unbounded label would then stretch the dialog to
    // the width of one sentence. Every other page is a single grid and never
    // meets either.
    ui.vertical(|ui| {
        ui.set_max_width(super::PAGE_WIDTH);
        egui::Grid::new("settings_hotkeys")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                for action in HotkeyAction::ALL {
                    ui.label(i18n.text(label(action)));
                    let waiting = capturing == Some(action);
                    let text = if waiting {
                        i18n.text(TextKey::HotkeyPressAKey).into_owned()
                    } else {
                        draft.hotkeys.binding(action).map_or_else(
                            || i18n.text(TextKey::HotkeyNone).into_owned(),
                            |chord| chord.to_string(),
                        )
                    };
                    if ui.add(egui::Button::new(text).selected(waiting)).clicked() {
                        outcome.capture = Some(action);
                    }
                    // The conflict is reported on the row that would lose, which
                    // is the one the user is looking at.
                    if let Some(chord) = draft.hotkeys.binding(action)
                        && let Some(other) = draft.hotkeys.conflict(action, chord)
                    {
                        let mut args = fluent_bundle::FluentArgs::new();
                        args.set("action", i18n.text(label(other)).into_owned());
                        ui.label(
                            egui::RichText::new(i18n.text_with(TextKey::HotkeyConflict, &args))
                                .color(ui.visuals().warn_fg_color),
                        );
                    }
                    ui.end_row();
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

fn label(action: HotkeyAction) -> TextKey {
    match action {
        HotkeyAction::ToggleRecording => TextKey::HotkeyToggleRecording,
        HotkeyAction::TogglePause => TextKey::HotkeyTogglePause,
        HotkeyAction::Fullscreen => TextKey::HotkeyFullscreen,
        HotkeyAction::OpenSettings => TextKey::HotkeyOpenSettings,
    }
}
