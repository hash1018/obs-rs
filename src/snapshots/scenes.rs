use crate::domain::{SceneId, Transition};

#[derive(Clone, Default)]
pub struct ScenesSnapshot {
    pub items: Vec<SceneSnapshot>,
    pub selected_scene_id: Option<SceneId>,
    /// What switching between them does. One answer for the project, shown
    /// and set under the list itself.
    pub transition: Transition,
}

#[derive(Clone)]
pub struct SceneSnapshot {
    pub id: SceneId,
    pub name: String,
}
