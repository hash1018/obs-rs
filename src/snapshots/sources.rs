use crate::domain::{
    Crop, SceneCanvas, SceneId, SceneItemId, SourceKind, SourceSettings, Transform,
};

#[derive(Clone)]
pub struct SourcesSnapshot {
    pub canvas: SceneCanvas,
    pub scene_id: Option<SceneId>,
    pub scene_name: Option<String>,
    /// Front-most item first, matching the order shown in the Sources dock.
    pub items: Vec<SceneItemSnapshot>,
}

impl Default for SourcesSnapshot {
    fn default() -> Self {
        Self {
            canvas: SceneCanvas::DEFAULT,
            scene_id: None,
            scene_name: None,
            items: Vec::new(),
        }
    }
}

#[derive(Clone)]
pub struct SceneItemSnapshot {
    pub id: SceneItemId,
    pub name: String,
    pub kind: SourceKind,
    pub settings: SourceSettings,
    /// The Source's own size in Canvas units, before `transform` scales it.
    pub source_size: [f32; 2],
    pub visible: bool,
    pub locked: bool,
    pub transform: Transform,
    pub crop: Crop,
}
