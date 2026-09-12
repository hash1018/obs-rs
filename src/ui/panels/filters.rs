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
//!
//! # And a mixer channel's
//!
//! Desktop Audio and the microphone are in no Scene, so selecting something
//! in the Preview can never point this dock at them. A channel's own menu in
//! the Audio Mixer does instead, and the dock shows that channel's filters
//! until something is next selected in the Preview.

use eframe::egui;

use crate::domain::{
    AudioFilter, AudioFilterId, AudioFilterKind, AudioFilterSettings, AudioSourceId,
    ChromaKeyMethod, CompressorSettings, Filter, FilterId, FilterKind, FilterSettings,
    LimiterSettings, NoiseGateSettings, SceneItemId, SourceKind,
};
use crate::i18n::{LocalizationManager, TextKey};
use crate::project::{AudioCommand, ProjectCommand, SourceCommand};
use crate::snapshots::{AudioSnapshot, AudioSourceSnapshot, SceneItemSnapshot, SourcesSnapshot};

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
    audio: &AudioSnapshot,
    state: &mut FiltersPanelState,
    i18n: &LocalizationManager,
    actions: &mut Vec<UiAction>,
) {
    // A mixer channel chosen from its own menu stays shown until something
    // else is chosen in the Preview: that is the other way this dock is
    // pointed at something, and the more recent of the two wins.
    let selected_item = editor.selected_item_id();
    if state.seen_item != selected_item {
        state.seen_item = selected_item;
        if selected_item.is_some() {
            state.audio = None;
        }
    }
    if let Some(channel) = state
        .audio
        .and_then(|id| audio.items.iter().find(|channel| channel.id == id))
    {
        show_channel(ui, channel, state, i18n, actions);
        return;
    }

    let Some(item) = selected_item.and_then(|id| snapshot.items.iter().find(|item| item.id == id))
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

/// What the panel remembers between frames: which row is open below, and
/// whether it is showing a mixer channel rather than the selected Source.
#[derive(Default)]
pub(in crate::ui) struct FiltersPanelState {
    selected: Option<FilterId>,
    /// The mixer channel whose filters are shown, when one was chosen from
    /// its menu more recently than anything was selected in the Preview.
    audio: Option<AudioSourceId>,
    audio_selected: Option<AudioFilterId>,
    /// The Preview's selection as last seen, so a new one is noticed and
    /// takes the dock back from a channel.
    seen_item: Option<SceneItemId>,
}

impl FiltersPanelState {
    /// Points the dock at one mixer channel — see `UiState::show_audio_filters`.
    pub(in crate::ui) fn show_audio(&mut self, id: AudioSourceId) {
        if self.audio != Some(id) {
            self.audio_selected = None;
        }
        self.audio = Some(id);
    }
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
                    // The cheapest control here, and the only one that
                    // changes nothing about the chain: a disabled filter
                    // stays in it and passes frames straight through, so
                    // this is one handle call. The buttons below rebuild
                    // what is in the Source's rack, which is still not a
                    // camera restarting.
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

    // Two destinations, the split the mixer's fader already documents: the
    // picture has to follow the pointer, and the project should hear one edit
    // rather than one per frame of the drag. Both reach the running element
    // through its handle either way, so neither rebuilds the chain — what
    // differs is how many rows get written. See `Gesture` for when each.
    let gesture = Gesture::of(&sliders, edited != settings);
    if gesture.drag {
        actions.push(UiAction::DragFilterSettings(
            item.id,
            filter.id,
            FilterSettings::ChromaKey(edited),
        ));
    }
    if gesture.record {
        actions.push(command(SourceCommand::SetChromaKeySettings(
            filter.id, edited,
        )));
    }
}

/// Where one frame's edit to a filter's settings goes.
///
/// Heard while dragging, recorded once when let go — the fader's split, and
/// its way of knowing when that is. The value is recorded on the frame the
/// drag stops, not on the next change: every frame of the drag has already
/// put it into the snapshot the dock reads, so on release nothing differs,
/// and waiting for a difference recorded nothing. The next project snapshot
/// then put the slider back where it was before the drag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Gesture {
    /// Send it to what is running, and not to the project.
    drag: bool,
    /// Record it in the project.
    record: bool,
}

impl Gesture {
    fn of(sliders: &[egui::Response], changed: bool) -> Self {
        Self::decide(
            sliders.iter().any(egui::Response::dragged),
            sliders.iter().any(egui::Response::drag_stopped),
            changed,
        )
    }

    /// `changed` is whether the value differs from the snapshot's.
    fn decide(dragging: bool, released: bool, changed: bool) -> Self {
        Self {
            drag: changed && dragging,
            record: released || (changed && !dragging),
        }
    }
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

// ---- A mixer channel's filters ----------------------------------------
//
// The same list, toolbar and settings as a Source's, over the channel's own
// filters and commands. Kept beside them rather than made generic over both:
// the two share a layout and nothing else — different ids, different kinds,
// different places the edits go.

fn show_channel(
    ui: &mut egui::Ui,
    channel: &AudioSourceSnapshot,
    state: &mut FiltersPanelState,
    i18n: &LocalizationManager,
    actions: &mut Vec<UiAction>,
) {
    ui.horizontal(|ui| {
        ui.strong(&channel.name);
        ui.weak(i18n.text(TextKey::FiltersOnTheChannel));
    });
    ui.separator();

    if state
        .audio_selected
        .is_some_and(|id| !channel.filters.iter().any(|filter| filter.id == id))
    {
        state.audio_selected = None;
    }

    if channel.filters.is_empty() {
        ui.weak(i18n.text(TextKey::FiltersEmpty));
    } else {
        egui::ScrollArea::vertical()
            .id_salt("audio-filters")
            .max_height(140.0)
            .show(ui, |ui| {
                for filter in &channel.filters {
                    ui.horizontal(|ui| {
                        let mut enabled = filter.enabled;
                        if ui.checkbox(&mut enabled, "").changed() {
                            actions.push(audio(AudioCommand::SetFilterEnabled(filter.id, enabled)));
                        }
                        let label = i18n.text(audio_kind_key(filter.settings.kind()));
                        if ui
                            .selectable_label(
                                state.audio_selected == Some(filter.id),
                                label.as_ref(),
                            )
                            .clicked()
                        {
                            state.audio_selected = Some(filter.id);
                        }
                    });
                }
            });
    }
    ui.separator();

    ui.horizontal(|ui| {
        // A menu rather than a button, since there is more than one kind to
        // add — and in the order a streaming application recommends running
        // them: take the noise out first, gate what is left, even it out,
        // and limit last.
        ui.menu_button("+", |ui| {
            for kind in AudioFilterKind::ALL {
                if ui.button(i18n.text(audio_kind_key(kind))).clicked() {
                    actions.push(audio(AudioCommand::AddFilter {
                        audio_source_id: channel.id,
                        kind,
                    }));
                    ui.close();
                }
            }
        })
        .response
        .on_hover_text(i18n.text(TextKey::FiltersAdd));

        let selected = state.audio_selected;
        ui.add_enabled_ui(selected.is_some(), |ui| {
            if ui
                .button("\u{2212}")
                .on_hover_text(i18n.text(TextKey::FiltersRemove))
                .clicked()
                && let Some(id) = selected
            {
                actions.push(audio(AudioCommand::RemoveFilter(id)));
                state.audio_selected = None;
            }
            if ui
                .button("\u{2191}")
                .on_hover_text(i18n.text(TextKey::FiltersMoveUp))
                .clicked()
                && let Some(id) = selected
            {
                actions.push(audio(AudioCommand::MoveFilterEarlier(id)));
            }
            if ui
                .button("\u{2193}")
                .on_hover_text(i18n.text(TextKey::FiltersMoveDown))
                .clicked()
                && let Some(id) = selected
            {
                actions.push(audio(AudioCommand::MoveFilterLater(id)));
            }
        });
    });

    let Some(selected) = state
        .audio_selected
        .and_then(|id| channel.filters.iter().find(|filter| filter.id == id))
    else {
        return;
    };
    ui.separator();
    // Scrolled on its own: a compressor's five rows under a chain of four
    // are taller than the dock starts out, and the last of them is the one
    // a compressor is least use without.
    egui::ScrollArea::vertical()
        .id_salt("audio-filter-settings")
        .show(ui, |ui| {
            show_channel_settings(ui, channel, selected, i18n, actions);
        });
}

fn show_channel_settings(
    ui: &mut egui::Ui,
    channel: &AudioSourceSnapshot,
    filter: &AudioFilter,
    i18n: &LocalizationManager,
    actions: &mut Vec<UiAction>,
) {
    let settings = filter.settings;
    let mut sliders: Vec<egui::Response> = Vec::new();
    let edited = egui::Grid::new("audio-filter-settings")
        .num_columns(2)
        .spacing([10.0, 6.0])
        .show(ui, |ui| match settings {
            AudioFilterSettings::NoiseSuppression => None,
            AudioFilterSettings::NoiseGate(gate) => Some(AudioFilterSettings::NoiseGate(
                gate_sliders(ui, gate, i18n, &mut sliders),
            )),
            AudioFilterSettings::Compressor(compressor) => Some(AudioFilterSettings::Compressor(
                compressor_sliders(ui, compressor, i18n, &mut sliders),
            )),
            AudioFilterSettings::Limiter(limiter) => Some(AudioFilterSettings::Limiter(
                limiter_sliders(ui, limiter, i18n, &mut sliders),
            )),
        })
        .inner;
    let Some(edited) = edited else {
        // RNNoise has nothing to set; what is worth saying is what it costs.
        ui.weak(i18n.text(TextKey::FiltersNoiseSuppressionAbout));
        return;
    };

    // Within range before it goes anywhere — for a gate, the close threshold
    // following the open one down rather than letting the pair cross, which
    // the element would refuse.
    let edited = edited.sanitised();
    let gesture = Gesture::of(&sliders, edited != settings);
    if gesture.drag {
        actions.push(UiAction::DragAudioFilterSettings(
            channel.id, filter.id, edited,
        ));
    }
    if gesture.record {
        actions.push(audio(AudioCommand::SetFilterSettings(filter.id, edited)));
    }
}

/// One labelled slider in a settings grid, kept for [`Gesture::of`].
fn slider_row(
    ui: &mut egui::Ui,
    label: &str,
    slider: egui::Slider<'_>,
    sliders: &mut Vec<egui::Response>,
) {
    ui.label(label);
    sliders.push(ui.add(slider));
    ui.end_row();
}

fn gate_sliders(
    ui: &mut egui::Ui,
    mut edited: NoiseGateSettings,
    i18n: &LocalizationManager,
    sliders: &mut Vec<egui::Response>,
) -> NoiseGateSettings {
    let range = NoiseGateSettings::MIN_THRESHOLD_DB..=0.0;
    slider_row(
        ui,
        i18n.text(TextKey::FiltersGateOpen).as_ref(),
        egui::Slider::new(&mut edited.open_threshold_db, range.clone()).suffix(" dB"),
        sliders,
    );
    slider_row(
        ui,
        i18n.text(TextKey::FiltersGateClose).as_ref(),
        egui::Slider::new(&mut edited.close_threshold_db, range).suffix(" dB"),
        sliders,
    );
    for (key, value, most) in [
        (TextKey::FiltersAttack, &mut edited.attack_ms, 1_000),
        (TextKey::FiltersGateHold, &mut edited.hold_ms, 2_000),
        (TextKey::FiltersRelease, &mut edited.release_ms, 2_000),
    ] {
        slider_row(
            ui,
            i18n.text(key).as_ref(),
            egui::Slider::new(value, 0..=most).suffix(" ms"),
            sliders,
        );
    }
    edited
}

/// A streaming application's compressor ranges, so a setting copied from
/// one fits.
fn compressor_sliders(
    ui: &mut egui::Ui,
    mut edited: CompressorSettings,
    i18n: &LocalizationManager,
    sliders: &mut Vec<egui::Response>,
) -> CompressorSettings {
    slider_row(
        ui,
        i18n.text(TextKey::FiltersRatio).as_ref(),
        egui::Slider::new(&mut edited.ratio, 1.0..=CompressorSettings::MAX_RATIO).suffix(":1"),
        sliders,
    );
    slider_row(
        ui,
        i18n.text(TextKey::FiltersThreshold).as_ref(),
        egui::Slider::new(
            &mut edited.threshold_db,
            CompressorSettings::MIN_THRESHOLD_DB..=0.0,
        )
        .suffix(" dB"),
        sliders,
    );
    slider_row(
        ui,
        i18n.text(TextKey::FiltersAttack).as_ref(),
        egui::Slider::new(&mut edited.attack_ms, 1..=500).suffix(" ms"),
        sliders,
    );
    slider_row(
        ui,
        i18n.text(TextKey::FiltersRelease).as_ref(),
        egui::Slider::new(&mut edited.release_ms, 1..=1_000).suffix(" ms"),
        sliders,
    );
    let gain = CompressorSettings::OUTPUT_GAIN_DB;
    slider_row(
        ui,
        i18n.text(TextKey::FiltersOutputGain).as_ref(),
        egui::Slider::new(&mut edited.output_gain_db, -gain..=gain).suffix(" dB"),
        sliders,
    );
    edited
}

fn limiter_sliders(
    ui: &mut egui::Ui,
    mut edited: LimiterSettings,
    i18n: &LocalizationManager,
    sliders: &mut Vec<egui::Response>,
) -> LimiterSettings {
    slider_row(
        ui,
        i18n.text(TextKey::FiltersThreshold).as_ref(),
        egui::Slider::new(
            &mut edited.threshold_db,
            LimiterSettings::MIN_THRESHOLD_DB..=0.0,
        )
        .suffix(" dB"),
        sliders,
    );
    slider_row(
        ui,
        i18n.text(TextKey::FiltersRelease).as_ref(),
        egui::Slider::new(&mut edited.release_ms, 1..=1_000).suffix(" ms"),
        sliders,
    );
    edited
}

fn audio(command: AudioCommand) -> UiAction {
    UiAction::Project(ProjectCommand::Audio(command))
}

fn audio_kind_key(kind: AudioFilterKind) -> TextKey {
    match kind {
        AudioFilterKind::NoiseSuppression => TextKey::FiltersNoiseSuppression,
        AudioFilterKind::NoiseGate => TextKey::FiltersNoiseGate,
        AudioFilterKind::Compressor => TextKey::FiltersCompressor,
        AudioFilterKind::Limiter => TextKey::FiltersLimiter,
    }
}

#[cfg(test)]
mod tests {
    use super::Gesture;

    /// A slider dragged over three frames and let go on the fourth. Each
    /// frame of the drag goes to what is running and writes the value into
    /// the snapshot, so the release frame sees no difference — and it is the
    /// one that must record. This is the frame that used to send nothing,
    /// which put the slider back where it was on the next project snapshot.
    #[test]
    fn letting_go_of_a_slider_records_the_value_it_was_dragged_to() {
        for _ in 0..3 {
            assert_eq!(
                Gesture::decide(true, false, true),
                Gesture {
                    drag: true,
                    record: false
                },
                "while dragging: heard, not recorded"
            );
        }
        assert_eq!(
            Gesture::decide(false, true, false),
            Gesture {
                drag: false,
                record: true
            },
            "let go: recorded, though nothing differs from the snapshot"
        );
    }

    /// A change that is not a drag — a click on the track, a typed value —
    /// is recorded at once, and a frame with nothing happening sends nothing.
    #[test]
    fn a_change_without_a_drag_is_recorded_and_no_change_sends_nothing() {
        assert_eq!(
            Gesture::decide(false, false, true),
            Gesture {
                drag: false,
                record: true
            }
        );
        assert_eq!(
            Gesture::decide(false, false, false),
            Gesture {
                drag: false,
                record: false
            }
        );
        assert_eq!(
            Gesture::decide(true, false, false),
            Gesture {
                drag: false,
                record: false
            },
            "held still mid-drag: nothing new to hear"
        );
    }
}
