mod action;
mod docking;
mod editor;
mod panels;
mod preview;
mod settings;
mod shell;
mod state;

use eframe::egui;

use crate::capture::AudioDeviceTarget;
use crate::engine::CompositeFrame;
use crate::i18n::LocalizationManager;
use crate::snapshots::Snapshots;

pub use action::UiAction;
pub use docking::WorkspaceDocks;
pub use preview::PreviewZoom;
pub use state::UiState;

pub(super) struct UiResources<'a> {
    snapshots: &'a Snapshots,
    /// What is currently set, as opposed to the draft the Settings dialog
    /// edits. Only the key bindings are read from here so far.
    settings: &'a crate::settings::AppSettings,
    /// Every audio endpoint the mixer can offer. Enumerated by the
    /// application, not here — see `ObsApp::audio_devices`.
    audio_devices: &'a [AudioDeviceTarget],
    i18n: &'a LocalizationManager,
    /// The latest frame the engine composited, or `None` before the first one
    /// arrives or when no engine is running.
    composite_frame: Option<&'a CompositeFrame>,
}

pub fn show(
    ui: &mut egui::Ui,
    state: &mut UiState,
    snapshots: &Snapshots,
    settings: &crate::settings::AppSettings,
    audio_devices: &[AudioDeviceTarget],
    i18n: &LocalizationManager,
    composite_frame: Option<&CompositeFrame>,
    actions: &mut Vec<UiAction>,
) {
    let resources = UiResources {
        snapshots,
        settings,
        audio_devices,
        i18n,
        composite_frame,
    };
    shell::show(ui, state, &resources, actions);
}
