use crate::domain::{AudioSourceId, DisplayCaptureSettings, SceneId, SceneItemId, Transform};

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
#[derive(Debug, Clone, PartialEq)]
pub enum AudioCommand {
    /// Gain in decibels. Clamped where it is stored rather than here, so
    /// every way in lands on the same range.
    SetGainDb(AudioSourceId, f32),
    SetMuted(AudioSourceId, bool),
}

#[derive(Debug, Clone, PartialEq)]
pub enum SourceCommand {
    AddColor(SceneId),
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
