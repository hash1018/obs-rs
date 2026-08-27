use eframe::egui;

use crate::i18n::Locale;
use crate::{
    domain::{SceneId, SceneItemId, Transform},
    project::ProjectCommand,
};

#[derive(Debug, Clone, PartialEq)]
pub enum UiAction {
    Exit,
    Project(ProjectCommand),
    OpenSystemDisplayPicker(SceneId),
    /// One item's Transform while the pointer is still down. Goes to the
    /// compositor and not to the project: a drag is one edit, recorded when it
    /// ends, but the picture has to follow the pointer meanwhile.
    DragSceneItem(SceneItemId, Transform),
    SetFullscreen(bool),
    SetTheme(egui::ThemePreference),
    SetLocale(Locale),
}
