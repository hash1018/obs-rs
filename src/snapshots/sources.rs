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

impl SceneItemSnapshot {
    /// The item's rectangle in Canvas coordinates, as `[x, y, width, height]`.
    ///
    /// The Preview draws it and the compositor places a layer at it, so it has
    /// to be one calculation rather than two that agree by inspection.
    ///
    /// `transform` is a parameter instead of `self.transform` because the
    /// editor asks for the value it is part-way through dragging, which is not
    /// what the project database holds yet.
    pub fn canvas_rect(&self, transform: Transform) -> [f32; 4] {
        let width = (self.source_size[0] - self.crop.left - self.crop.right).max(1.0)
            * transform.scale[0].max(0.001);
        let height = (self.source_size[1] - self.crop.top - self.crop.bottom).max(1.0)
            * transform.scale[1].max(0.001);
        [
            transform.position[0] - width * transform.anchor[0],
            transform.position[1] - height * transform.anchor[1],
            width,
            height,
        ]
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
