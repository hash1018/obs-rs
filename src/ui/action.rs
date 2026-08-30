use crate::i18n::Locale;
use crate::{
    domain::{AudioSourceId, SceneId, SceneItemId, Transform},
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
    /// A Drawing's strokes while the pointer is still down, for the same
    /// reason `DragSceneItem` exists: the mark has to be under the pointer,
    /// and the stroke is recorded once when the gesture ends.
    DrawStrokes(SceneItemId, Vec<crate::domain::Stroke>),
    /// One source's gain while the fader is still held. Goes to the audio
    /// graph and not to the project, for the same reason `DragSceneItem`
    /// does: what is heard has to follow the pointer, and the edit is
    /// recorded once when the gesture ends.
    DragAudioGain(AudioSourceId, f32),
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
    /// End the recording properly and then quit — what the closing question
    /// asks about.
    StopRecordingAndExit,
    /// Show the Settings dialog, seeded from what is currently set.
    OpenSettings,
    /// Commit the dialog's draft: persist it, and hand the engine the part it
    /// needs. Boxed because it is much the largest variant here and every
    /// other one would otherwise be padded to its size.
    ApplySettings(Box<crate::settings::AppSettings>),
}
