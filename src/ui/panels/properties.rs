//! What the selected Source is, as it currently stands.
//!
//! Read-only. Everything here is already settable somewhere — a Transform by
//! dragging in the Preview, visibility and lock by the Sources dock's own
//! icons — and this says what those came out as, in numbers a drag cannot be
//! precise about.
//!
//! # Why it is a dock and not a dialog
//!
//! The values it reports change while you look at them: dragging a source in
//! the Preview moves the numbers here. A dialog would have to be reopened to
//! see that, and would cover the picture the numbers are about.
//!
//! # Nothing about crop
//!
//! `SceneItemSnapshot` carries one and the editor's geometry honours it, but
//! nothing in the engine does — a cropped source composites uncropped. Until
//! that is true, a crop shown here would be a number describing something the
//! recording does not do.

use eframe::egui;

use crate::domain::{DisplayCaptureTarget, SourceKind, SourceSettings};
use crate::i18n::{LocalizationManager, TextKey};
use crate::snapshots::{SceneItemSnapshot, SourcesSnapshot};

use super::super::editor::SceneEditorState;

pub(in crate::ui) fn show(
    ui: &mut egui::Ui,
    editor: &SceneEditorState,
    snapshot: &SourcesSnapshot,
    i18n: &LocalizationManager,
) {
    let Some(item) = editor
        .selected_item_id()
        .and_then(|id| snapshot.items.iter().find(|item| item.id == id))
    else {
        ui.weak(i18n.text(TextKey::PropertiesNoSelection));
        return;
    };

    egui::ScrollArea::vertical().show(ui, |ui| {
        egui::Grid::new("properties")
            .num_columns(2)
            .spacing([10.0, 6.0])
            .show(ui, |ui| {
                row(ui, i18n.text(TextKey::PropertiesName).as_ref(), &item.name);
                row(
                    ui,
                    i18n.text(TextKey::PropertiesKind).as_ref(),
                    i18n.text(kind_key(item.kind)).as_ref(),
                );
                show_placement(ui, item, editor, i18n);
                show_settings(ui, item, i18n);
            });
    });
}

/// Where it sits, which is what a drag in the Preview was aiming at.
///
/// The transform is the one being dragged when there is one, not the one the
/// project holds — otherwise the numbers would stand still through the very
/// gesture someone is watching them for.
fn show_placement(
    ui: &mut egui::Ui,
    item: &SceneItemSnapshot,
    editor: &SceneEditorState,
    i18n: &LocalizationManager,
) {
    let transform = editor.effective_transform(item.id, item.transform);
    let [x, y, width, height] = item.canvas_rect(transform);

    row(
        ui,
        i18n.text(TextKey::PropertiesPosition).as_ref(),
        &format!("{:.0}, {:.0}", x, y),
    );
    row(
        ui,
        i18n.text(TextKey::PropertiesSize).as_ref(),
        &format!("{:.0} × {:.0}", width, height),
    );
    if transform.rotation_degrees.abs() > 0.05 {
        row(
            ui,
            i18n.text(TextKey::PropertiesRotation).as_ref(),
            &format!("{:.1}°", transform.rotation_degrees),
        );
    }
    row(
        ui,
        i18n.text(TextKey::PropertiesVisible).as_ref(),
        i18n.text(yes_no(item.visible)).as_ref(),
    );
    row(
        ui,
        i18n.text(TextKey::PropertiesLocked).as_ref(),
        i18n.text(yes_no(item.locked)).as_ref(),
    );
}

/// What only this kind of Source has to say.
fn show_settings(ui: &mut egui::Ui, item: &SceneItemSnapshot, i18n: &LocalizationManager) {
    match &item.settings {
        SourceSettings::Color(settings) => {
            ui.label(i18n.text(TextKey::PropertiesColour));
            ui.horizontal(|ui| {
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                ui.painter().rect_filled(
                    rect,
                    2.0,
                    egui::Color32::from_rgb(settings.rgba[0], settings.rgba[1], settings.rgba[2]),
                );
                ui.monospace(format!(
                    "#{:02X}{:02X}{:02X}",
                    settings.rgba[0], settings.rgba[1], settings.rgba[2]
                ));
            });
            ui.end_row();
            // The alpha is the layer's opacity rather than something in the
            // pixels — see `layer_for` — so it is reported as one.
            row(
                ui,
                i18n.text(TextKey::PropertiesOpacity).as_ref(),
                &format!("{:.0}%", f32::from(settings.rgba[3]) / 255.0 * 100.0),
            );
        }
        SourceSettings::Drawing(settings) => {
            row(
                ui,
                i18n.text(TextKey::PropertiesStrokes).as_ref(),
                &settings.strokes.len().to_string(),
            );
            row(
                ui,
                i18n.text(TextKey::PropertiesSurface).as_ref(),
                &format!("{:.0} × {:.0}", settings.size[0], settings.size[1]),
            );
        }
        SourceSettings::DisplayCapture(settings) => {
            match &settings.target {
                DisplayCaptureTarget::MonitorName(name) => {
                    row(
                        ui,
                        i18n.text(TextKey::PropertiesMonitor).as_ref(),
                        name.as_str(),
                    );
                    show_desktop_position(ui, name, i18n);
                }
                // The portal's token is opaque and long; whether there is one
                // is the whole of what a reader can use it for — it is the
                // difference between reopening silently and being asked
                // again.
                DisplayCaptureTarget::Portal { restore_token } => row(
                    ui,
                    i18n.text(TextKey::PropertiesMonitor).as_ref(),
                    i18n.text(match restore_token {
                        Some(_) => TextKey::PropertiesPortalRemembered,
                        None => TextKey::PropertiesPortalAsks,
                    })
                    .as_ref(),
                ),
            }
            // What the picker said, which the capture layer replaces with the
            // stream's own size once the Source opens — so this is the live
            // figure rather than the stored hint whenever there is one.
            row(
                ui,
                i18n.text(TextKey::PropertiesStream).as_ref(),
                &format!("{:.0} × {:.0}", item.source_size[0], item.source_size[1]),
            );
        }
        SourceSettings::None => {}
    }
}

/// Where this display sits in the virtual desktop, read now rather than
/// remembered.
///
/// Deliberately not stored anywhere: a monitor's position changes whenever
/// displays are rearranged and nothing in this application resolves against
/// it, which is why a Display Capture keeps only the name. Reading it live is
/// a different thing from persisting it — this dock is a view of how things
/// stand, so it asks the system each frame it draws this row. That is one
/// `EnumDisplayMonitors`, and only while a Display Capture is the selection.
///
/// The size is not repeated here: `Stream size` below is what the capture
/// actually negotiated, which is the more useful of the two when they differ.
///
/// Nothing is shown for a name the system does not currently report — an
/// unplugged display leaves the stored name above it and no coordinates,
/// which is the truth about it.
fn show_desktop_position(ui: &mut egui::Ui, monitor: &str, i18n: &LocalizationManager) {
    let crate::capture::SourcePicker::Enumerated { monitors, .. } = crate::capture::source_picker()
    else {
        return;
    };
    let Some(target) = monitors.iter().find(|target| target.name == monitor) else {
        return;
    };
    row(
        ui,
        i18n.text(TextKey::PropertiesDesktopPosition).as_ref(),
        &format!("{}, {}", target.rect.x, target.rect.y),
    );
}

/// One label and its value, with the value able to be selected and copied —
/// a monitor name is something people paste into a bug report.
fn row(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.label(label);
    let mut shown = value;
    ui.add(
        egui::TextEdit::singleline(&mut shown)
            .desired_width(f32::INFINITY)
            .font(egui::TextStyle::Monospace),
    );
    ui.end_row();
}

fn kind_key(kind: SourceKind) -> TextKey {
    match kind {
        SourceKind::DisplayCapture => TextKey::SourceKindDisplayCapture,
        SourceKind::WindowCapture => TextKey::SourceKindWindowCapture,
        SourceKind::VideoCapture => TextKey::SourceKindVideoCapture,
        SourceKind::Image => TextKey::SourceKindImage,
        SourceKind::Color => TextKey::SourceKindColor,
        SourceKind::Drawing => TextKey::SourceKindDrawing,
    }
}

fn yes_no(value: bool) -> TextKey {
    if value {
        TextKey::PropertiesYes
    } else {
        TextKey::PropertiesNo
    }
}
