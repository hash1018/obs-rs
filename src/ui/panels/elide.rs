//! Text that admits it did not fit.
//!
//! A dock is narrow and what it lists is not: a source named after the file
//! it shows, a window titled with a path. Rows here are laid out to the
//! pane's width rather than to their own text, so the overflow is not
//! somewhere to scroll to — it is clipped, and a clipped name reads as a
//! shorter name rather than as a longer one with its end missing.
//!
//! So text that will not fit is cut with `…`, and the whole of it is offered
//! on hover. Only when it was actually cut: a tooltip repeating what is
//! already legible is noise on every other row.

use std::sync::Arc;

use eframe::egui;

/// Lays `text` out on one row, cut with `…` at `max_width`.
///
/// The galley is coloured [`egui::Color32::PLACEHOLDER`], so whoever paints
/// it supplies the colour. A row that is selected or hovered has its own, and
/// baking one in here would freeze it to whatever this function was told.
pub(super) fn one_row(
    ui: &egui::Ui,
    text: &str,
    max_width: f32,
    style: &egui::TextStyle,
) -> Arc<egui::Galley> {
    let font_id = style.resolve(ui.style());
    let mut job = egui::text::LayoutJob::simple(
        text.to_owned(),
        font_id,
        egui::Color32::PLACEHOLDER,
        max_width,
    );
    job.wrap = egui::text::TextWrapping::truncate_at_width(max_width);
    ui.painter().layout_job(job)
}

/// Paints one row of `text` from `left_center`, cut to `max_width`, and
/// reports whether anything was left out.
///
/// The report is the point: it is what a caller attaches a tooltip on, and it
/// cannot be worked out from the string, only from the layout.
pub(super) fn paint_one_row(
    ui: &egui::Ui,
    left_center: egui::Pos2,
    max_width: f32,
    text: &str,
    color: egui::Color32,
) -> bool {
    let galley = one_row(ui, text, max_width, &egui::TextStyle::Body);
    let elided = galley.elided;
    let top_left = left_center - egui::vec2(0.0, galley.size().y / 2.0);
    ui.painter().galley(top_left, galley, color);
    elided
}

/// Whether `text` in `style` wants more room than `available`.
///
/// For text this module does not paint itself — a widget that lays out its
/// own, such as the read-only field a Properties value is shown in.
pub(super) fn overflows(
    ui: &egui::Ui,
    text: &str,
    available: f32,
    style: &egui::TextStyle,
) -> bool {
    let font_id = style.resolve(ui.style());
    let width = ui
        .painter()
        .layout_no_wrap(text.to_owned(), font_id, egui::Color32::PLACEHOLDER)
        .size()
        .x;
    width > available
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both halves of the contract in one pass, because either alone would
    /// pass for the wrong reason: a helper that always elides would satisfy
    /// the first assertion, and one that never does would satisfy the last.
    #[test]
    fn a_row_is_cut_to_its_width_and_says_so() {
        let context = egui::Context::default();
        let mut output = context.run_ui(Default::default(), |context| {
            egui::CentralPanel::default().show(context, |ui| {
                let long = "Display Capture of a monitor with a very long name indeed";

                let cut = one_row(ui, long, 60.0, &egui::TextStyle::Body);
                assert!(cut.elided, "text past the width must be cut");
                assert!(
                    cut.size().x <= 60.0,
                    "a cut row must fit: {} > 60",
                    cut.size().x
                );
                assert!(overflows(ui, long, 60.0, &egui::TextStyle::Body));

                let whole = one_row(ui, "Scene 1", 600.0, &egui::TextStyle::Body);
                assert!(!whole.elided, "text that fits must be left alone");
                assert!(!overflows(ui, "Scene 1", 600.0, &egui::TextStyle::Body));
            });
        });
        // Nothing uploads these outside a real renderer, and epaint panics on
        // a delta that is dropped unapplied.
        output.textures_delta.clear();
    }
}
