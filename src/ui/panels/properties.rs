//! What the selected Source is, as it currently stands.
//!
//! Almost all of it reports rather than asks: everything shown is already
//! settable somewhere — a Transform by dragging in the Preview, visibility
//! and lock by the Sources dock's own icons — and this says what those came
//! out as, in numbers a drag cannot be precise about. The exception is a
//! Color's colour, which has nowhere else to be set; see `show_colour`.
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

use crate::domain::SceneItemId;
use crate::domain::{DisplayCaptureTarget, SourceKind, SourceSettings, WindowCaptureTarget};
use crate::i18n::{LocalizationManager, TextKey};
use crate::project::{ProjectCommand, SourceCommand};
use crate::snapshots::{SceneItemSnapshot, SourcesSnapshot};

use super::super::UiAction;
use super::super::editor::SceneEditorState;
use super::elide;

pub(in crate::ui) fn show(
    ui: &mut egui::Ui,
    editor: &SceneEditorState,
    snapshot: &SourcesSnapshot,
    i18n: &LocalizationManager,
    actions: &mut Vec<UiAction>,
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
                show_settings(ui, item, i18n, actions);
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
fn show_settings(
    ui: &mut egui::Ui,
    item: &SceneItemSnapshot,
    i18n: &LocalizationManager,
    actions: &mut Vec<UiAction>,
) {
    match &item.settings {
        SourceSettings::Color(settings) => {
            show_colour(ui, item.id, settings.rgba, i18n, actions);
            // The alpha is the layer's opacity rather than something in the
            // pixels — see `layer_for` — so it is reported as one, and the
            // picker above edits only the three that are.
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
        SourceSettings::DisplayCapture(settings) => match &settings.target {
            DisplayCaptureTarget::MonitorName(name) => {
                row(
                    ui,
                    i18n.text(TextKey::PropertiesMonitor).as_ref(),
                    name.as_str(),
                );
                show_desktop_rect(ui, name, item, i18n);
            }
            // The portal's token is opaque and long; whether there is one is
            // the whole of what a reader can use it for — it is the
            // difference between reopening silently and being asked again.
            // It names no display, so there is no rectangle to give either,
            // and the negotiated size is all there is to say about shape.
            DisplayCaptureTarget::Portal { restore_token } => {
                row(
                    ui,
                    i18n.text(TextKey::PropertiesMonitor).as_ref(),
                    i18n.text(match restore_token {
                        Some(_) => TextKey::PropertiesPortalRemembered,
                        None => TextKey::PropertiesPortalAsks,
                    })
                    .as_ref(),
                );
                row(
                    ui,
                    i18n.text(TextKey::PropertiesStream).as_ref(),
                    &format!("{:.0} × {:.0}", item.source_size[0], item.source_size[1]),
                );
            }
        },
        SourceSettings::WindowCapture(settings) => match &settings.target {
            WindowCaptureTarget::Window { process, title } => {
                row(
                    ui,
                    i18n.text(TextKey::PropertiesProcess).as_ref(),
                    process.as_str(),
                );
                // Blank for a window that reported none, which is common
                // enough that an empty row is the honest rendering — the
                // process above is then the whole of what identifies it.
                row(
                    ui,
                    i18n.text(TextKey::PropertiesTitle).as_ref(),
                    title.as_str(),
                );
            }
            WindowCaptureTarget::Portal { restore_token } => row(
                ui,
                i18n.text(TextKey::PropertiesWindow).as_ref(),
                i18n.text(match restore_token {
                    Some(_) => TextKey::PropertiesPortalRemembered,
                    None => TextKey::PropertiesPortalAsks,
                })
                .as_ref(),
            ),
        },
        SourceSettings::MediaFile(settings) => {
            // The whole path, not the file name: two files with the same name
            // in different folders are exactly the case where a reader needs
            // to know which one this is. `row` elides and puts the full text
            // on hover, which is what makes a long path survivable in a dock.
            row(
                ui,
                i18n.text(TextKey::PropertiesFile).as_ref(),
                &settings.path.display().to_string(),
            );
            show_looping(ui, item.id, settings.looping, i18n, actions);
        }
        SourceSettings::None => {}
    }
}

/// Whether the file starts again at its end.
///
/// Written to the project the moment it is clicked, unlike the colour picker
/// above: a checkbox has no gesture to wait out, and the engine applies it
/// through the demuxer's own handle rather than by reopening — so what is
/// playing does not restart, and switching it off part way through lets the
/// lap that is running play out.
fn show_looping(
    ui: &mut egui::Ui,
    item: SceneItemId,
    stored: bool,
    i18n: &LocalizationManager,
    actions: &mut Vec<UiAction>,
) {
    ui.label(i18n.text(TextKey::PropertiesLoop));
    let mut looping = stored;
    if ui.checkbox(&mut looping, "").changed() {
        actions.push(UiAction::Project(ProjectCommand::Source(
            SourceCommand::SetMediaLooping(item, looping),
        )));
    }
    ui.end_row();
}

/// The one thing here that is not read-only.
///
/// Split the way every other gesture in this application is: what is
/// composited follows the pointer, and the project is told once, when the
/// pointer comes up. A picker dragged across its square changes on every
/// frame, and a database write per frame is what this split exists to avoid.
///
/// The alpha is left alone. It is the layer's opacity rather than anything in
/// the pixels — see `layer_for` — so editing it here would be editing a
/// different thing under the same name, and it is reported on its own row.
fn show_colour(
    ui: &mut egui::Ui,
    item: SceneItemId,
    stored: [u8; 4],
    i18n: &LocalizationManager,
    actions: &mut Vec<UiAction>,
) {
    ui.label(i18n.text(TextKey::PropertiesColour));
    ui.horizontal(|ui| {
        let mut rgb = [stored[0], stored[1], stored[2]];
        let picker = egui::color_picker::color_edit_button_srgb(ui, &mut rgb);
        let rgba = [rgb[0], rgb[1], rgb[2], stored[3]];

        if rgba != stored {
            // What is composited follows every change; the project hears
            // one. `changed` fires per frame while a swatch is dragged, so
            // the release is what stands in for the `drag_stopped` a fader
            // has — and a change with the pointer already up is a preset
            // being clicked.
            actions.push(UiAction::DragSourceColour(item, rgba));
            let released = ui.input(|input| input.pointer.any_released());
            let held = ui.input(|input| input.pointer.any_down());
            if released || (picker.changed() && !held) {
                actions.push(UiAction::Project(ProjectCommand::Source(
                    SourceCommand::SetColor(item, rgba),
                )));
            }
        }
        ui.monospace(format!("#{:02X}{:02X}{:02X}", rgba[0], rgba[1], rgba[2]));
    });
    ui.end_row();
}

/// Where this display is and how big it is, as one rectangle, read now rather
/// than remembered.
///
/// Deliberately not stored anywhere: a monitor's position changes whenever
/// displays are rearranged and nothing in this application resolves against
/// it, which is why a Display Capture keeps only the name. Reading it live is
/// a different thing from persisting it — this dock is a view of how things
/// stand, so it asks the system on the frame it draws this row. That is one
/// `EnumDisplayMonitors`, and only while a Display Capture is the selection.
///
/// Two rows, and the stream.s own size is not a third: desktop duplication
/// hands over the whole display at its native size, so it would be the same
/// number as `Desktop size` beside it.
///
/// The two can still come apart — a monitor reported in a scaled coordinate
/// space while the capture negotiated native pixels — and the stream's own
/// size is then added, because what is being captured has stopped being what
/// the rectangle describes. It stays absent while they agree, so the row
/// appearing is itself the report.
///
/// Nothing is shown at all for a name the system does not currently report.
/// An unplugged display leaves the stored name above it and no rectangle,
/// which is the truth about it.
fn show_desktop_rect(
    ui: &mut egui::Ui,
    monitor: &str,
    item: &SceneItemSnapshot,
    i18n: &LocalizationManager,
) {
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
    row(
        ui,
        i18n.text(TextKey::PropertiesDesktopSize).as_ref(),
        &format!("{} × {}", target.rect.width, target.rect.height),
    );

    let stream = [item.source_size[0] as u32, item.source_size[1] as u32];
    if stream != [target.rect.width, target.rect.height] {
        row(
            ui,
            i18n.text(TextKey::PropertiesStream).as_ref(),
            &format!("{} × {}", stream[0], stream[1]),
        );
    }
}

/// One label and its value, with the value able to be selected and copied —
/// a monitor name is something people paste into a bug report.
fn row(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.label(label);
    let mut shown = value;
    let field = ui.add(
        egui::TextEdit::singleline(&mut shown)
            .desired_width(f32::INFINITY)
            .font(egui::TextStyle::Monospace),
    );
    // A window title is the one value here regularly wider than the dock. The
    // field can be clicked into and read through, but nothing says so, and a
    // value cut off mid-word reads as the whole value.
    if elide::overflows(
        ui,
        value,
        field.rect.width() - FIELD_MARGIN,
        &egui::TextStyle::Monospace,
    ) {
        field.on_hover_text(value);
    }
    ui.end_row();
}

/// What a `TextEdit` keeps for itself either side of its text.
const FIELD_MARGIN: f32 = 8.0;

fn kind_key(kind: SourceKind) -> TextKey {
    match kind {
        SourceKind::DisplayCapture => TextKey::SourceKindDisplayCapture,
        SourceKind::WindowCapture => TextKey::SourceKindWindowCapture,
        SourceKind::VideoCapture => TextKey::SourceKindVideoCapture,
        SourceKind::Image => TextKey::SourceKindImage,
        SourceKind::MediaFile => TextKey::SourceKindMediaFile,
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
