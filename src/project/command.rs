use crate::domain::{DisplayCaptureSettings, SceneId, SceneItemId, Transform};

#[derive(Debug, Clone, PartialEq)]
pub enum ProjectCommand {
    Scene(SceneCommand),
    Source(SourceCommand),
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
