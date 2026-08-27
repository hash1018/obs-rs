mod layout;
mod renderer;

use eframe::egui;

use crate::snapshots::Snapshots;

use super::{
    UiAction,
    editor::SceneEditorState,
    panels::{scenes::ScenesPanelState, sources::SourcesPanelState},
};

pub(super) use layout::{DockLayout, DockPanel};

pub(super) fn show(
    ui: &mut egui::Ui,
    layout: &mut DockLayout,
    scenes_state: &mut ScenesPanelState,
    sources_state: &mut SourcesPanelState,
    editor: &mut SceneEditorState,
    snapshots: &Snapshots,
    actions: &mut Vec<UiAction>,
) {
    renderer::show(
        ui,
        layout,
        scenes_state,
        sources_state,
        editor,
        snapshots,
        actions,
    );
}
