//! A dialog the rest of the window waits for.
//!
//! Every dialog here used to be an `egui::Window`, which floats over the
//! application without stopping anything behind it. That is not what a
//! dialog promises, and for the Settings dialog it was a way to lose work:
//! the dialog edits a draft of every setting and Apply writes all of it, so a
//! theme changed from the menu bar while the dialog was open was quietly put
//! back by the next Apply. A dialog that holds the window until it is
//! answered cannot be raced like that.
//!
//! What this adds to [`egui::Modal`] is what a dialog here needs and a
//! backdrop does not give: a title where the window's title bar was, the
//! window's own frame so nothing changes shape, and Escape as the way out.
//! Clicking outside is deliberately *not* a way out. Every one of these holds
//! something being chosen or typed, and a stray click beside a half-filled
//! dialog should not throw it away — the buttons inside are how one ends.

use eframe::egui;

/// What a dialog's contents returned, and whether it was dismissed from
/// outside them.
pub(in crate::ui) struct Shown<T> {
    pub(in crate::ui) inner: T,
    /// Escape was pressed while this was the dialog on top. Callers treat it
    /// as their Cancel.
    pub(in crate::ui) escaped: bool,
}

/// Shows one dialog, centred, over a backdrop that takes every click meant
/// for what is behind it.
///
/// Escape reaches this only if nothing inside claimed it first: a combo box
/// that is open closes itself instead, and the Hotkeys page, which is
/// waiting for a key, takes it as that key.
pub(in crate::ui) fn show<T>(
    ctx: &egui::Context,
    id: &str,
    title: &str,
    content: impl FnOnce(&mut egui::Ui) -> T,
) -> Shown<T> {
    let modal = egui::Modal::new(egui::Id::new(id))
        .frame(egui::Frame::window(&ctx.global_style()))
        .show(ctx, |ui| {
            ui.heading(title);
            ui.add_space(4.0);
            content(ui)
        });
    let escaped = modal.is_top_modal
        && !modal.any_popup_open
        && ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
    Shown {
        inner: modal.inner,
        escaped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(events: Vec<egui::Event>) -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            events,
            ..Default::default()
        }
    }

    fn click_at(position: egui::Pos2) -> Vec<egui::Event> {
        vec![
            egui::Event::PointerMoved(position),
            egui::Event::PointerButton {
                pos: position,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
            egui::Event::PointerButton {
                pos: position,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            },
        ]
    }

    fn escape() -> egui::Event {
        egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }

    /// What one frame did: whether the button behind was clicked, where it
    /// is, and whether the dialog (if any) was escaped.
    struct Frame {
        behind_clicked: bool,
        behind: egui::Rect,
        escaped: bool,
    }

    /// A few frames with nothing happening, so the dialog is really there.
    ///
    /// An `egui::Area` spends its first frame invisible, measuring itself, and
    /// clicks are aimed at what the previous frame drew — so a click one frame
    /// after a dialog opens goes where the dialog is not yet. Nobody clicks
    /// that fast; a test does unless told otherwise.
    fn settle(context: &egui::Context, with_dialog: bool) -> egui::Rect {
        let mut behind = egui::Rect::NOTHING;
        for _ in 0..3 {
            behind = frame(context, input(Vec::new()), with_dialog).behind;
        }
        behind
    }

    /// One frame of the application's shape in miniature: a button in the
    /// window, and optionally a dialog over it.
    fn frame(context: &egui::Context, raw: egui::RawInput, with_dialog: bool) -> Frame {
        let mut result = Frame {
            behind_clicked: false,
            behind: egui::Rect::NOTHING,
            escaped: false,
        };
        let mut output = context.run_ui(raw, |context| {
            egui::CentralPanel::default().show(context, |ui| {
                let button = ui.button("behind");
                result.behind_clicked = button.clicked();
                result.behind = button.rect;
            });
            if with_dialog {
                result.escaped = show(context, "test_dialog", "A dialog", |ui| {
                    ui.label("inside");
                })
                .escaped;
            }
        });
        output.textures_delta.clear();
        result
    }

    /// The point of the whole change: while a dialog is up, the window behind
    /// it cannot be used. The same click without the dialog is what shows
    /// the button was hit at all.
    #[test]
    fn a_click_behind_an_open_dialog_does_not_reach_what_is_behind() {
        let without = egui::Context::default();
        let behind = settle(&without, false);
        let clicked = frame(&without, input(click_at(behind.center())), false);
        assert!(
            clicked.behind_clicked,
            "the button is where the click lands"
        );

        let with = egui::Context::default();
        let behind = settle(&with, true);
        let clicked = frame(&with, input(click_at(behind.center())), true);
        assert!(
            !clicked.behind_clicked,
            "and with a dialog open the same click must not reach it"
        );
    }

    /// Escape is a dialog's Cancel.
    #[test]
    fn escape_dismisses_the_dialog() {
        let context = egui::Context::default();
        settle(&context, true);
        assert!(frame(&context, input(vec![escape()]), true).escaped);
    }

    /// A click outside is not: every one of these holds something being
    /// chosen or typed, and a stray click must not throw that away.
    #[test]
    fn a_click_outside_the_dialog_does_not_dismiss_it() {
        let context = egui::Context::default();
        let behind = settle(&context, true);
        let clicked = frame(&context, input(click_at(behind.center())), true);
        assert!(!clicked.escaped);
    }
}
