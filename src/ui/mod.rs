mod action;
mod dialog;
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

pub use action::{AudioFilterHost, UiAction};
pub use docking::WorkspaceDocks;
pub use preview::PreviewZoom;
pub use shell::hotkeys::{act as act_on_hotkeys, keyboard_taken};
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
    /// Whether a global hotkey listener is running, which takes every
    /// hotkey but the window's own away from it — see `shell::hotkeys`.
    global_hotkeys: bool,
}

impl<'a> UiResources<'a> {
    /// Gathered by the caller rather than by `show`, which would otherwise
    /// take every one of these as a parameter of its own — the growth
    /// AGENTS.md asks this type to absorb.
    pub(super) fn new(
        snapshots: &'a Snapshots,
        settings: &'a crate::settings::AppSettings,
        audio_devices: &'a [AudioDeviceTarget],
        i18n: &'a LocalizationManager,
        composite_frame: Option<&'a CompositeFrame>,
        global_hotkeys: bool,
    ) -> Self {
        Self {
            snapshots,
            settings,
            audio_devices,
            i18n,
            composite_frame,
            global_hotkeys,
        }
    }
}

pub(super) fn show(
    ui: &mut egui::Ui,
    state: &mut UiState,
    resources: &UiResources<'_>,
    actions: &mut Vec<UiAction>,
) {
    shell::show(ui, state, resources, actions);
}
