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
    SetTransform(SceneItemId, Transform),
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
