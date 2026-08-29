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
    SetTheme(crate::settings::Theme),
    SetLocale(Locale),
    /// Begin recording the composited frames. Where the file goes is the
    /// engine's to decide, so this carries nothing.
    StartRecording,
    /// Stop or resume writing to the running recording, leaving its file
    /// open. The paused span does not end up in it.
    SetRecordingPaused(bool),
    /// Finish the running recording and close its file.
    StopRecording,
    /// Show the Settings dialog, seeded from what is currently set.
    OpenSettings,
    /// Commit the dialog's draft: persist it, and hand the engine the part it
    /// needs. Boxed because it is much the largest variant here and every
    /// other one would otherwise be padded to its size.
    ApplySettings(Box<crate::settings::AppSettings>),
}
