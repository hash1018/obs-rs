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
//! backdrop does not give: a title bar where the window's own was, the
//! window's frame so nothing changes shape, and Escape as the way out.
//! Clicking outside is deliberately *not* a way out. Every one of these holds
//! something being chosen or typed, and a stray click beside a half-filled
//! dialog should not throw it away — the buttons inside are how one ends.
//!
//! # The title bar moves it
//!
//! A modal is centred and stays centred, which is right until it covers the
//! thing it is asking about — the Settings dialog over the Preview whose
//! frame rate it is setting, a picker over the Scene it is adding to. These
//! were `egui::Window`s before and could be dragged aside; taking that away
//! along with the floating was not the point of the change.
//!
//! So the title bar is a handle. Dragging it moves the dialog by offsetting
//! the anchor [`egui::Modal`] would otherwise pin to the centre.
//!
//! Keeping it on screen is `egui::Area`'s own doing, not this module's: it
//! constrains what it draws to the window, and it bounds the pointer, so an
//! offset cannot run away however far a drag asks. Nothing here repeats
//! that — a first attempt did, and measuring it showed it changed nothing.
//!
//! One rough edge is left, and left knowingly. Make the window much smaller
//! while a dialog sits at an edge and the offset describes somewhere the
//! window no longer has; `Area` draws it at the edge regardless, so the next
//! drag jumps rather than following the pointer. It settles after that one
//! drag. Fixing it properly means owning the position outright instead of
//! anchoring — which is a larger change than the problem is worth.

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
    let dialog_id = egui::Id::new(id);
    let offset_id = dialog_id.with("offset");
    let offset: egui::Vec2 = ctx.data(|data| data.get_temp(offset_id).unwrap_or_default());

    let frame = egui::Frame::window(&ctx.global_style());
    let margin = frame.inner_margin;
    let modal = egui::Modal::new(dialog_id)
        .frame(frame)
        // The anchor the default area pins to the centre, moved by however
        // far this dialog has been dragged.
        .area(egui::Modal::default_area(dialog_id).anchor(egui::Align2::CENTER_CENTER, offset))
        .show(ctx, |ui| {
            let bar = title_bar(ui, title, margin);
            let inner = content(ui);
            // Painted after the content, because only now is the dialog's
            // width known — a title bar is the width of what it titles, and
            // what it titles is laid out below it.
            bar.finish(ui);
            (inner, bar.drag_delta(ui, dialog_id))
        });

    let (inner, dragged) = modal.inner;
    if dragged != egui::Vec2::ZERO {
        ctx.data_mut(|data| data.insert_temp(offset_id, offset + dragged));
    }

    let escaped = modal.is_top_modal
        && !modal.any_popup_open
        && ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
    Shown { inner, escaped }
}

/// The strip across the top of a dialog: its title, and the handle that
/// moves it.
///
/// Built in two parts because its own width is not known while it is drawn.
/// The title row goes down first, at whatever width the title needs; the
/// background and the rule beneath it are painted into slots reserved before
/// it, once the content below has settled what the dialog's width really is
/// — see [`TitleBar::finish`].
struct TitleBar {
    background: egui::layers::ShapeIdx,
    rule: egui::layers::ShapeIdx,
    /// What the title row occupied, before it is widened to the dialog.
    row: egui::Rect,
    /// The frame's own inner margin, so the strip reaches the window frame
    /// rather than stopping at the content's inset.
    margin: egui::Margin,
}

/// The gap above and below the title, inside the strip.
const BAR_PADDING: f32 = 4.0;

fn title_bar(ui: &mut egui::Ui, title: &str, margin: egui::Margin) -> TitleBar {
    // Reserved before anything is drawn over them, so the background stays
    // behind the title rather than on top of it.
    let background = ui.painter().add(egui::Shape::Noop);
    let rule = ui.painter().add(egui::Shape::Noop);
    let row = ui
        .scope(|ui| {
            ui.add_space(BAR_PADDING);
            ui.heading(title);
            ui.add_space(BAR_PADDING);
        })
        .response
        .rect;
    // Clear of the rule, so the content below does not sit on the line.
    ui.add_space(BAR_PADDING * 2.0);
    TitleBar {
        background,
        rule,
        row,
        margin,
    }
}

impl TitleBar {
    /// The strip, at the width the finished dialog turned out to be.
    fn rect(&self, ui: &egui::Ui) -> egui::Rect {
        let dialog = ui.min_rect();
        egui::Rect::from_min_max(
            egui::pos2(
                dialog.left() - self.margin.leftf(),
                self.row.top() - self.margin.topf(),
            ),
            egui::pos2(dialog.right() + self.margin.rightf(), self.row.bottom()),
        )
    }

    /// Paints the strip, now that the width is known.
    fn finish(&self, ui: &egui::Ui) {
        let rect = self.rect(ui);
        let visuals = ui.visuals();
        // The window frame's own top corners, so the strip reads as part of
        // the frame rather than as a panel laid inside it.
        let corners = visuals.window_corner_radius;
        ui.painter().set(
            self.background,
            egui::Shape::rect_filled(
                rect,
                egui::CornerRadius {
                    nw: corners.nw,
                    ne: corners.ne,
                    sw: 0,
                    se: 0,
                },
                visuals.widgets.noninteractive.bg_fill,
            ),
        );
        ui.painter().set(
            self.rule,
            egui::Shape::hline(
                rect.x_range(),
                rect.bottom(),
                visuals.widgets.noninteractive.bg_stroke,
            ),
        );
    }

    /// How far the pointer moved the bar this frame.
    ///
    /// Asked after the strip's real width is known, so the whole width of it
    /// is the handle rather than only as much as the title needed.
    fn drag_delta(&self, ui: &egui::Ui, id: egui::Id) -> egui::Vec2 {
        let response = ui.interact(self.rect(ui), id.with("title-bar"), egui::Sense::drag());
        if response.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        } else if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        }
        response.drag_delta()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(events: Vec<egui::Event>) -> egui::RawInput {
        sized_input(events, egui::vec2(800.0, 600.0))
    }

    fn sized_input(events: Vec<egui::Event>, size: egui::Vec2) -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
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
    /// is, whether the dialog (if any) was escaped, and where that dialog
    /// ended up.
    struct Frame {
        behind_clicked: bool,
        behind: egui::Rect,
        escaped: bool,
        dialog: egui::Rect,
    }

    /// Presses at `from`, moves by `delta`, and lets go — over four frames,
    /// which is what makes it a drag rather than a click.
    ///
    /// egui decides a press has become a drag by comparing frames, and it
    /// applies a moved dialog's new offset on the frame *after* the one that
    /// measured the movement. Delivering the whole gesture in one pass gets
    /// a click and a dialog that has not moved.
    ///
    /// Answers where the dialog ended up.
    fn drag_by(context: &egui::Context, from: egui::Pos2, delta: egui::Vec2) -> egui::Rect {
        let press = vec![
            egui::Event::PointerMoved(from),
            egui::Event::PointerButton {
                pos: from,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
        ];
        frame(context, input(press), true);
        frame(
            context,
            input(vec![egui::Event::PointerMoved(from + delta)]),
            true,
        );
        let release = vec![egui::Event::PointerButton {
            pos: from + delta,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        }];
        frame(context, input(release), true);
        // One more with nothing happening, which is the frame the new offset
        // is drawn at.
        frame(context, input(Vec::new()), true).dialog
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
            dialog: egui::Rect::NOTHING,
        };
        let mut output = context.run_ui(raw, |context| {
            egui::CentralPanel::default().show(context, |ui| {
                let button = ui.button("behind");
                result.behind_clicked = button.clicked();
                result.behind = button.rect;
            });
            if with_dialog {
                let shown = show(context, "test_dialog", "A dialog", |ui| {
                    // Deliberately wider than the title: a dialog whose
                    // content is narrower than its own heading would make
                    // `the_title_bar_is_as_wide_as_the_dialog` prove nothing.
                    ui.label("inside, and wider than the title above it is");
                    ui.min_rect()
                });
                result.escaped = shown.escaped;
                result.dialog = shown.inner;
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

    /// A modal is centred and stays centred, which is exactly the problem
    /// when it covers the thing it is asking about. Dragging the title bar
    /// moves it, and the dialog stays where it was put.
    #[test]
    fn dragging_the_title_bar_moves_the_dialog_and_it_stays_moved() {
        let context = egui::Context::default();
        settle(&context, true);
        let before = frame(&context, input(Vec::new()), true).dialog;

        // From inside the strip across the top — a few pixels below its top
        // edge, which is where a pointer aiming at a title bar lands.
        let grab = egui::pos2(before.center().x, before.top() + 8.0);
        let moved = drag_by(&context, grab, egui::vec2(60.0, 40.0));
        assert_eq!(
            (moved.left() - before.left(), moved.top() - before.top()),
            (60.0, 40.0),
            "the dialog did not follow the drag"
        );

        // And it is still there on a frame where nothing happens, rather
        // than springing back to the centre the anchor would otherwise pin
        // it to.
        let after = frame(&context, input(Vec::new()), true).dialog;
        assert_eq!(after.min, moved.min, "the dialog sprang back");
    }

    /// The content is not a handle. A drag that begins inside the dialog but
    /// below its title bar is a drag of whatever it began on — a slider, a
    /// selection in a text field — and moving the window out from under it
    /// would make those unusable.
    #[test]
    fn dragging_the_body_does_not_move_the_dialog() {
        let context = egui::Context::default();
        settle(&context, true);
        let before = frame(&context, input(Vec::new()), true).dialog;

        let grab = egui::pos2(before.center().x, before.bottom() - 4.0);
        let after = drag_by(&context, grab, egui::vec2(60.0, 40.0));
        assert_eq!(after.min, before.min);
    }

    /// However far a drag asks, the dialog stays somewhere a pointer can
    /// reach it.
    ///
    /// `egui::Area`'s guarantee rather than this module's — it constrains
    /// what it draws to the window — which is exactly why it is worth
    /// pinning here: this relies on it, and a dialog that could be pushed
    /// out of the window would take the window with it, since nothing
    /// behind a modal can be clicked.
    #[test]
    fn a_dialog_cannot_be_dragged_out_of_reach() {
        let context = egui::Context::default();
        settle(&context, true);
        let before = frame(&context, input(Vec::new()), true).dialog;
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));

        let grab = egui::pos2(before.center().x, before.top() + 8.0);
        for away in [
            egui::vec2(-5_000.0, 0.0),
            egui::vec2(5_000.0, 0.0),
            egui::vec2(0.0, -5_000.0),
            egui::vec2(0.0, 5_000.0),
        ] {
            let context = egui::Context::default();
            settle(&context, true);
            let moved = drag_by(&context, grab, away);
            // Far out, then a small nudge back the other way.
            let back = drag_by(
                &context,
                egui::pos2(moved.center().x, moved.top() + 8.0),
                -away.normalized() * 30.0,
            );
            eprintln!("PROBE away={away:?} -> {moved:?} then back -> {back:?}");
            assert!(
                moved.right() > screen.left() && moved.left() < screen.right(),
                "dragged by {away:?} and left nothing to grab horizontally: {moved:?}"
            );
            assert!(
                moved.top() >= screen.top() - 1.0 && moved.top() < screen.bottom(),
                "dragged by {away:?} and left nothing to grab vertically: {moved:?}"
            );
        }
    }

    /// The whole strip is the handle, not just the words on it.
    ///
    /// The title row is laid out at whatever width the title needs, which is
    /// narrower than the dialog whenever the content below is wider — so a
    /// bar sized to the row would leave most of itself dead, and grabbing
    /// near a corner would do nothing. It is widened to the dialog after the
    /// content settles, and this is what says so.
    #[test]
    fn the_title_bar_is_as_wide_as_the_dialog() {
        let context = egui::Context::default();
        settle(&context, true);
        let before = frame(&context, input(Vec::new()), true).dialog;

        // Past the right-hand end of the title text — the title row starts
        // at the dialog's left edge, so only the far side is somewhere a bar
        // sized to the row would not reach.
        let far_side = egui::pos2(before.right() - 3.0, before.top() + 3.0);
        let moved = drag_by(&context, far_side, egui::vec2(50.0, 25.0));
        assert_eq!(
            (moved.left() - before.left(), moved.top() - before.top()),
            (50.0, 25.0),
            "the corner of the title bar is not part of the handle"
        );
    }
}
