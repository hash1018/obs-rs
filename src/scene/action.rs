use super::SceneId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SceneAction {
    Add,
    Delete(SceneId),
    Duplicate(SceneId),
    MoveUp(SceneId),
    MoveDown(SceneId),
    Select(SceneId),
}
