mod layout;
mod renderer;

use eframe::egui;

use super::{
    UiAction, UiResources,
    editor::SceneEditorState,
    panels::{scenes::ScenesPanelState, sources::SourcesPanelState},
};

pub use layout::WorkspaceDocks;
pub(super) use layout::{DockLayout, DockPanel};

/// How far a dock insets its content from its own edges.
///
/// Shared with `panels::toolbar`: the toolbar sits flush against the dock's
/// bottom edge and centres its buttons inside itself, so it has to know the
/// gap the other three edges use or the strip reads as misaligned.
pub(in crate::ui) const PANEL_MARGIN: f32 = 8.0;

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
