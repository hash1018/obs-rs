use crate::domain::SceneId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectCommand {
    Scene(SceneCommand),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SceneCommand {
    Add,
    Delete(SceneId),
    Duplicate(SceneId),
    MoveUp(SceneId),
    MoveDown(SceneId),
    Select(SceneId),
}
