use crate::domain::{
    AudioSourceId, DisplayCaptureSettings, SceneId, SceneItemId, Stroke, Transform,
};

#[derive(Debug, Clone, PartialEq)]
pub enum ProjectCommand {
    Scene(SceneCommand),
    Source(SourceCommand),
    Audio(AudioCommand),
}

/// A change to one of the mixer's audio sources.
///
/// Carries no Scene, because audio is not in one — see
/// [`crate::domain::AudioSourceId`].
// Every variant is a `Set` because every one of them is: the mixer has no
// add or remove, only the three values a channel holds. Dropping the verb
// would leave `AudioCommand::Device(..)` reading as a noun where the rest of
// this file reads as instructions.
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, PartialEq)]
pub enum AudioCommand {
    /// Gain in decibels. Clamped where it is stored rather than here, so
    /// every way in lands on the same range.
    SetGainDb(AudioSourceId, f32),
    SetMuted(AudioSourceId, bool),
    /// Which endpoint to listen to, or `None` to follow the system default.
    SetDevice(AudioSourceId, Option<String>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum SourceCommand {
    AddColor(SceneId),
    AddDrawing(SceneId),
    AddDisplayCapture {
        scene_id: SceneId,
        settings: DisplayCaptureSettings,
    },
    Delete(SceneItemId),
    MoveUp(SceneItemId),
    MoveDown(SceneItemId),
    SetLocked(SceneItemId, bool),
    /// Replaces the portal token a Display Capture reopens with.
    SetRestoreToken(SceneItemId, Option<String>),
    SetTransform(SceneItemId, Transform),
    SetVisible(SceneItemId, bool),
    /// Puts one finished stroke on a Drawing. Sent when the pointer comes up,
    /// not while it is down — see `UiAction::DragStroke`.
    AddStroke(SceneItemId, Stroke),
    /// Takes strokes off a Drawing by their position in it, which is both
    /// what the eraser does and what undo does.
    RemoveStrokes(SceneItemId, Vec<usize>),
    ClearStrokes(SceneItemId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SceneCommand {
    Add,
    Delete(SceneId),
    Duplicate(SceneId),
    MoveUp(SceneId),
    MoveDown(SceneId),
    Rename(SceneId, String),
    Select(SceneId),
}
