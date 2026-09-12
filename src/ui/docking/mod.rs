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

/// The panels' own state, gathered so the two `show` calls that thread it
/// through do not each take one argument per dock.
pub(super) struct PanelStates<'a> {
    pub(super) scenes: &'a mut ScenesPanelState,
    pub(super) sources: &'a mut SourcesPanelState,
    pub(super) filters: &'a mut crate::ui::panels::filters::FiltersPanelState,
    pub(super) stats: &'a mut crate::ui::panels::stats::StatsPanelState,
}

pub(super) fn show(
    ui: &mut egui::Ui,
    layout: &mut DockLayout,
    panels: PanelStates<'_>,
    editor: &mut SceneEditorState,
    resources: &UiResources<'_>,
    actions: &mut Vec<UiAction>,
) {
    renderer::show(ui, layout, panels, editor, resources, actions);
}
