use crate::domain::SceneId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectCommand {
    Scene(SceneCommand),
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
