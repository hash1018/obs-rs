use crate::domain::SceneId;

#[derive(Clone, Default)]
pub struct ScenesSnapshot {
    pub items: Vec<SceneSnapshot>,
    pub selected_scene_id: Option<SceneId>,
}

#[derive(Clone)]
pub struct SceneSnapshot {
    pub id: SceneId,
    pub name: String,
}
