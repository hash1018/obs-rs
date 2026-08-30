use eframe::egui;

use crate::i18n::{LocalizationManager, TextKey};

use crate::domain::SceneItemId;
use crate::project::{ProjectCommand, SourceCommand};

use super::super::UiAction;
use super::super::editor::{PenState, Tool};
use super::state::{PreviewScaleMode, PreviewViewState};

pub(super) const TOOLBAR_HEIGHT: f32 = 26.0;
pub(super) const TOOLBAR_WIDTH: f32 = 210.0;
/// What the pen's own controls need beside the zoom ones. Only claimed while
/// a Drawing is selected — the toolbar is its resting width otherwise.
pub(super) const PEN_TOOLBAR_WIDTH: f32 = 330.0;
pub(super) const TOOLBAR_GAP: f32 = 6.0;

/// The palette a stroke's colour is chosen from.
///
/// A short row rather than a picker: annotating is a fast, interrupting sort
/// of thing, and six that read against most pictures beats a wheel that has
/// to be aimed at. The picker is still there for anything else.
const PALETTE: [[u8; 4]; 6] = [
    [220, 40, 40, 255],
    [250, 190, 40, 255],
    [60, 200, 90, 255],
    [60, 150, 240, 255],
    [20, 20, 20, 255],
    [245, 245, 245, 255],
];

/// The widths on offer, in Canvas units.
const WIDTHS: [(f32, TextKey); 3] = [
    (3.0, TextKey::DrawingWidthThin),
    (6.0, TextKey::DrawingWidthMedium),
    (14.0, TextKey::DrawingWidthThick),
];

/// The pen's own half of the toolbar, shown only while a Drawing is selected.
pub(super) fn show_pen(
    ui: &mut egui::Ui,
    pen: &mut PenState,
    item_id: SceneItemId,
    strokes: usize,
    i18n: &LocalizationManager,
    actions: &mut Vec<UiAction>,
) {
    ui.separator();
    for (tool, key) in [
        (Tool::Select, TextKey::DrawingToolSelect),
        (Tool::Pen, TextKey::DrawingToolPen),
        (Tool::Eraser, TextKey::DrawingToolEraser),
    ] {
        let label = i18n.text(key);
        if ui
            .selectable_label(pen.tool == tool, label.as_ref())
            .on_hover_text(label.as_ref())
            .clicked()
        {
            pen.tool = tool;
            // Half a stroke belongs to the tool that was drawing it.
            pen.stroke = None;
        }
    }

    ui.separator();
    for rgba in PALETTE {
        let colour = egui::Color32::from_rgb(rgba[0], rgba[1], rgba[2]);
        let (rect, response) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::click());
        ui.painter().rect_filled(rect, 2.0, colour);
        if pen.rgba == rgba {
            ui.painter().rect_stroke(
                rect,
                2.0,
                ui.visuals().selection.stroke,
                egui::StrokeKind::Outside,
            );
        }
        if response.clicked() {
            pen.rgba = rgba;
        }
    }

    ui.separator();
    let current = i18n.text(
        WIDTHS
            .iter()
            .find(|(width, _)| (*width - pen.width).abs() < 0.01)
            .map_or(TextKey::DrawingWidthMedium, |(_, key)| *key),
    );
    ui.menu_button(current, |ui| {
        for (width, key) in WIDTHS {
            if ui
                .selectable_label((width - pen.width).abs() < 0.01, i18n.text(key))
                .clicked()
            {
                pen.width = width;
                ui.close();
            }
        }
    })
    .response
    .on_hover_text(i18n.text(TextKey::DrawingWidth));

    ui.separator();
    if icon_button(
        ui,
        strokes > 0,
        i18n.text(TextKey::DrawingUndo).as_ref(),
        paint_undo,
    ) {
        // Undo is the eraser's own command aimed at the last stroke: there is
        // no separate history, because the strokes *are* the history.
        actions.push(UiAction::Project(ProjectCommand::Source(
            SourceCommand::RemoveStrokes(item_id, vec![strokes - 1]),
        )));
    }
    if icon_button(
        ui,
        strokes > 0,
        i18n.text(TextKey::DrawingClear).as_ref(),
        paint_bin,
    ) {
        actions.push(UiAction::Project(ProjectCommand::Source(
            SourceCommand::ClearStrokes(item_id),
        )));
    }
}

/// A button whose face is drawn rather than typed.
///
/// Drawn for the reason the Sources panel draws its eye and its padlock: a
/// glyph is at the mercy of whatever font the system handed us, and these two
/// have no character that is both obvious and certain to be present. The
/// shapes are a few line segments either way.
fn icon_button(
    ui: &mut egui::Ui,
    enabled: bool,
    hover: &str,
    paint: fn(&egui::Painter, egui::Pos2, egui::Color32),
) -> bool {
    let size = egui::vec2(22.0, 20.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let response = response.on_hover_text(hover);
    let visuals = ui.style().interact(&response);
    if enabled && (response.hovered() || response.is_pointer_button_down_on()) {
        ui.painter()
            .rect_filled(rect, visuals.corner_radius, visuals.bg_fill);
    }
    let color = if enabled {
        visuals.fg_stroke.color
    } else {
        ui.visuals().widgets.noninteractive.fg_stroke.color
    };
    paint(ui.painter(), rect.center(), color);
    enabled && response.clicked()
}

/// An arrow curving back on itself: undo.
fn paint_undo(painter: &egui::Painter, center: egui::Pos2, color: egui::Color32) {
    let stroke = egui::Stroke::new(1.4, color);
    // Three quarters of a circle, left open so the head has somewhere to
    // point — a closed ring would read as a refresh rather than a step back.
    const RADIUS: f32 = 5.0;
    const START: f32 = std::f32::consts::PI * 0.85;
    const SWEEP: f32 = std::f32::consts::PI * 1.5;
    let at = |angle: f32| center + egui::vec2(angle.cos() * RADIUS, angle.sin() * RADIUS);
    let arc: Vec<egui::Pos2> = (0..=24)
        .map(|step| at(START + SWEEP * step as f32 / 24.0))
        .collect();
    painter.add(egui::Shape::line(arc, stroke));

    // The head sits on the end the arc starts from and points back the way it
    // came, along the tangent there — an arrowhead at any other angle reads
    // as a smudge rather than as a direction.
    let along = egui::vec2(START.sin(), -START.cos());
    let across = egui::vec2(-along.y, along.x);
    let base = at(START);
    painter.add(egui::Shape::convex_polygon(
        vec![base + along * 3.2, base + across * 2.0, base - across * 2.0],
        color,
        egui::Stroke::NONE,
    ));
}

/// A bin: everything drawn, thrown away.
///
/// A bin rather than another eraser, because the eraser is the tool beside it
/// and these two must not read as the same thing — one takes the stroke under
/// the pointer, the other takes all of them.
fn paint_bin(painter: &egui::Painter, center: egui::Pos2, color: egui::Color32) {
    let stroke = egui::Stroke::new(1.2, color);
    let top = center.y - 3.5;
    // Lid, with a handle above it.
    painter.line_segment(
        [
            egui::pos2(center.x - 5.0, top),
            egui::pos2(center.x + 5.0, top),
        ],
        stroke,
    );
    painter.line_segment(
        [
            egui::pos2(center.x - 1.8, top - 2.0),
            egui::pos2(center.x + 1.8, top - 2.0),
        ],
        stroke,
    );
    // Body, narrowing towards the bottom.
    painter.line_segment(
        [
            egui::pos2(center.x - 4.0, top),
            egui::pos2(center.x - 3.0, center.y + 5.5),
        ],
        stroke,
    );
    painter.line_segment(
        [
            egui::pos2(center.x + 4.0, top),
            egui::pos2(center.x + 3.0, center.y + 5.5),
        ],
        stroke,
    );
    painter.line_segment(
        [
            egui::pos2(center.x - 3.0, center.y + 5.5),
            egui::pos2(center.x + 3.0, center.y + 5.5),
        ],
        stroke,
    );
}

pub(super) fn show(ui: &mut egui::Ui, state: &mut PreviewViewState, i18n: &LocalizationManager) {
    ui.spacing_mut().item_spacing.x = 4.0;
    ui.horizontal_centered(|ui| {
        if ui
            .add_enabled(state.can_decrease(), egui::Button::new("−"))
            .on_hover_text(i18n.text(TextKey::PreviewScaleDecrease))
            .clicked()
        {
            state.decrease();
        }

        let mut percentage = state.percentage();
        let response = ui.add(
            egui::DragValue::new(&mut percentage)
                .range(40.0..=100.0)
                .speed(1.0)
                .suffix("%")
                .max_decimals(0)
                .update_while_editing(false),
        );
        if response.changed() {
            state.set_percentage(percentage);
        }

        if ui
            .add_enabled(state.can_increase(), egui::Button::new("+"))
            .on_hover_text(i18n.text(TextKey::PreviewScaleIncrease))
            .clicked()
        {
            state.increase();
        }

        let mut fit_text = egui::RichText::new(i18n.text(TextKey::PreviewScaleFit));
        if state.mode() == PreviewScaleMode::FitToWorkspace {
            fit_text = fit_text.strong();
        }
        ui.menu_button(fit_text, |ui| {
            if ui
                .selectable_label(
                    state.mode() == PreviewScaleMode::FitToWorkspace,
                    i18n.text(TextKey::PreviewFitWorkspace),
                )
                .clicked()
            {
                state.fit_to_workspace();
                ui.close();
            }
            ui.separator();
            for percentage in [50.0, 75.0, 100.0] {
                if ui.button(format!("{percentage:.0}%")).clicked() {
                    state.set_percentage(percentage);
                    ui.close();
                }
            }
            ui.separator();
            if ui.button(i18n.text(TextKey::PreviewResetView)).clicked() {
                state.reset();
                ui.close();
            }
        })
        .response
        .on_hover_text(i18n.text(TextKey::PreviewScaleOptions));
    });
}
