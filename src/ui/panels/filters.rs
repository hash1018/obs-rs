//! What is done to the selected Source's picture before it is composited.
//!
//! # Why it is a dock and not a dialog
//!
//! The same reason [`super::properties`] is, only more so: a threshold is
//! tuned by watching the Preview, and a dialog would cover the picture the
//! slider is about. OBS puts filters in a window and then has to put a second
//! preview inside it.
//!
//! # These belong to the Source
//!
//! A Transform and a Crop are the SceneItem's, and the Properties dock shows
//! them for the item that is selected. These are the Source's, so changing
//! one changes it in every Scene that Source appears in — which is what the
//! header says, in as many words, because the two docks sit side by side and
//! otherwise look like they are talking about the same thing.

use eframe::egui;

use crate::domain::{ChromaKeyMethod, Filter, FilterId, FilterKind, FilterSettings, SourceKind};
use crate::i18n::{LocalizationManager, TextKey};
use crate::project::{ProjectCommand, SourceCommand};
use crate::snapshots::{SceneItemSnapshot, SourcesSnapshot};

use super::super::UiAction;
use super::super::editor::SceneEditorState;

/// Which Source kinds the engine actually runs filters for.
///
/// One, for now. Offering an Add on a kind whose chain ignores what it adds
/// would be a control that does nothing, which is worse than a sentence
/// saying so — see this panel's own `show`.
fn accepts_filters(kind: SourceKind) -> bool {
    matches!(kind, SourceKind::VideoCapture)
}

pub(in crate::ui) fn show(
    ui: &mut egui::Ui,
    editor: &SceneEditorState,
    snapshot: &SourcesSnapshot,
    state: &mut FiltersPanelState,
    i18n: &LocalizationManager,
    actions: &mut Vec<UiAction>,
) {
    let Some(item) = editor
        .selected_item_id()
        .and_then(|id| snapshot.items.iter().find(|item| item.id == id))
    else {
        ui.weak(i18n.text(TextKey::FiltersNoSelection));
        return;
    };

    if !accepts_filters(item.kind) {
        ui.weak(i18n.text(TextKey::FiltersUnsupportedKind));
        return;
    }

    // Which Source these belong to, said where the two docks can be told
    // apart: Properties above is showing this item, and this is showing what
    // is behind it.
    ui.horizontal(|ui| {
        ui.strong(&item.name);
        ui.weak(i18n.text(TextKey::FiltersOnTheSource));
    });
    ui.separator();

    if state
        .selected
        .is_some_and(|id| !item.filters.iter().any(|filter| filter.id == id))
    {
        // The one that was selected has been removed, here or in another
        // window onto the same project.
        state.selected = None;
    }

    show_list(ui, item, state, i18n, actions);
    ui.separator();
    show_toolbar(ui, item, state, i18n, actions);

    let Some(selected) = state
        .selected
        .and_then(|id| item.filters.iter().find(|filter| filter.id == id))
    else {
        return;
    };
    ui.separator();
    show_settings(ui, item, selected, i18n, actions);
}

/// What the panel remembers between frames, which is only which row is open
/// below.
#[derive(Default)]
pub(in crate::ui) struct FiltersPanelState {
    selected: Option<FilterId>,
}

fn show_list(
    ui: &mut egui::Ui,
    item: &SceneItemSnapshot,
    state: &mut FiltersPanelState,
    i18n: &LocalizationManager,
    actions: &mut Vec<UiAction>,
) {
    if item.filters.is_empty() {
        ui.weak(i18n.text(TextKey::FiltersEmpty));
        return;
    }

    egui::ScrollArea::vertical()
        .max_height(140.0)
        .show(ui, |ui| {
            for filter in &item.filters {
                ui.horizontal(|ui| {
                    let mut enabled = filter.enabled;
                    // The one control here that never rebuilds anything: a
                    // disabled filter stays in the chain and passes frames
                    // straight through, so this is a handle call rather than
                    // a camera restarting.
                    if ui.checkbox(&mut enabled, "").changed() {
                        actions.push(command(SourceCommand::SetFilterEnabled(filter.id, enabled)));
                    }
                    let label = i18n.text(kind_key(filter));
                    if ui
                        .selectable_label(state.selected == Some(filter.id), label.as_ref())
                        .clicked()
                    {
                        state.selected = Some(filter.id);
                    }
                });
            }
        });
}

fn show_toolbar(
    ui: &mut egui::Ui,
    item: &SceneItemSnapshot,
    state: &mut FiltersPanelState,
    i18n: &LocalizationManager,
    actions: &mut Vec<UiAction>,
) {
    ui.horizontal(|ui| {
        if ui
            .button("+")
            .on_hover_text(i18n.text(TextKey::FiltersAdd))
            .clicked()
        {
            actions.push(command(SourceCommand::AddFilter {
                scene_item_id: item.id,
                kind: FilterKind::ChromaKey,
            }));
        }

        let selected = state.selected;
        ui.add_enabled_ui(selected.is_some(), |ui| {
            if ui
                .button("\u{2212}")
                .on_hover_text(i18n.text(TextKey::FiltersRemove))
                .clicked()
                && let Some(id) = selected
            {
                actions.push(command(SourceCommand::RemoveFilter(id)));
                state.selected = None;
            }
            // Up is earlier in the chain, which is nearer the Source: a
            // filter is applied top to bottom, and the list reads the same
            // way.
            if ui
                .button("\u{2191}")
                .on_hover_text(i18n.text(TextKey::FiltersMoveUp))
                .clicked()
                && let Some(id) = selected
            {
                actions.push(command(SourceCommand::MoveFilterEarlier(id)));
            }
            if ui
                .button("\u{2193}")
                .on_hover_text(i18n.text(TextKey::FiltersMoveDown))
                .clicked()
                && let Some(id) = selected
            {
                actions.push(command(SourceCommand::MoveFilterLater(id)));
            }
        });
    });
}

fn show_settings(
    ui: &mut egui::Ui,
    item: &SceneItemSnapshot,
    filter: &Filter,
    i18n: &LocalizationManager,
    actions: &mut Vec<UiAction>,
) {
    let FilterSettings::ChromaKey(settings) = filter.settings;
    let mut edited = settings;

    // Collected so the pushes below can ask what the gesture was.
    let mut sliders: Vec<egui::Response> = Vec::new();
    egui::Grid::new("filter-settings")
        .num_columns(2)
        .spacing([10.0, 6.0])
        .show(ui, |ui| {
            ui.label(i18n.text(TextKey::FiltersChromaKeyColour).as_ref());
            egui::ComboBox::from_id_salt("chroma-key-method")
                .selected_text(i18n.text(method_key(edited.method)))
                .show_ui(ui, |ui| {
                    for method in [
                        ChromaKeyMethod::Green,
                        ChromaKeyMethod::Blue,
                        ChromaKeyMethod::Custom,
                    ] {
                        ui.selectable_value(
                            &mut edited.method,
                            method,
                            i18n.text(method_key(method)),
                        );
                    }
                });
            ui.end_row();

            // Shown whichever method is selected, and only editable for the
            // one that uses it — the value is kept either way, so comparing
            // against Green and coming back finds the colour still there.
            ui.label(i18n.text(TextKey::FiltersChromaKeyCustom).as_ref());
            ui.add_enabled_ui(edited.method == ChromaKeyMethod::Custom, |ui| {
                ui.color_edit_button_srgb(&mut edited.custom_rgb);
            });
            ui.end_row();

            ui.label(i18n.text(TextKey::FiltersChromaKeyThreshold).as_ref());
            sliders.push(ui.add(egui::Slider::new(&mut edited.threshold, 0.0..=1.0)));
            ui.end_row();

            ui.label(i18n.text(TextKey::FiltersChromaKeySmoothing).as_ref());
            sliders.push(ui.add(egui::Slider::new(&mut edited.smoothing, 0.0..=1.0)));
            ui.end_row();
        });

    if edited == settings {
        return;
    }

    // Two destinations, the split the mixer's fader already documents: the
    // picture has to follow the pointer, and the project should hear one edit
    // rather than one per frame of the drag. Both reach the running element
    // through its handle either way, so neither rebuilds the chain — what
    // differs is how many rows get written.
    let dragging = sliders.iter().any(egui::Response::dragged);
    if dragging {
        actions.push(UiAction::DragFilterSettings(
            item.id,
            filter.id,
            FilterSettings::ChromaKey(edited),
        ));
        return;
    }
    actions.push(command(SourceCommand::SetChromaKeySettings(
        filter.id, edited,
    )));
}

fn command(command: SourceCommand) -> UiAction {
    UiAction::Project(ProjectCommand::Source(command))
}

fn kind_key(filter: &Filter) -> TextKey {
    match filter.settings {
        FilterSettings::ChromaKey(_) => TextKey::FiltersChromaKey,
    }
}

fn method_key(method: ChromaKeyMethod) -> TextKey {
    match method {
        ChromaKeyMethod::Green => TextKey::FiltersChromaKeyGreen,
        ChromaKeyMethod::Blue => TextKey::FiltersChromaKeyBlue,
        ChromaKeyMethod::Custom => TextKey::FiltersChromaKeyCustomMethod,
    }
}
