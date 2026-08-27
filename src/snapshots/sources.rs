use crate::domain::{Crop, SceneId, SceneItemId, SourceId, SourceKind, Transform};

#[derive(Clone, Default)]
pub struct SourcesSnapshot {
    pub scene_id: Option<SceneId>,
    pub scene_name: Option<String>,
    /// Front-most item first, matching the order shown in the Sources dock.
    pub items: Vec<SceneItemSnapshot>,
}

#[derive(Clone)]
#[expect(
    dead_code,
    reason = "transform and source metadata are consumed by the upcoming preview editor"
)]
pub struct SceneItemSnapshot {
    pub id: SceneItemId,
    pub source_id: SourceId,
    pub name: String,
    pub kind: SourceKind,
    pub visible: bool,
    pub locked: bool,
    pub transform: Transform,
    pub crop: Crop,
    pub z_index: i64,
}
