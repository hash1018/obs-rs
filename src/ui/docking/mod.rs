mod layout;
mod renderer;

use eframe::egui;

use super::{
    UiAction, UiResources,
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
    resources: &UiResources<'_>,
    actions: &mut Vec<UiAction>,
) {
    renderer::show(
        ui,
        layout,
        scenes_state,
        sources_state,
        editor,
        resources,
        actions,
    );
}
