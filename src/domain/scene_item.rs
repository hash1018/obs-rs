use super::{SceneId, SourceId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SceneItemId(pub i64);

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transform {
    pub position: [f32; 2],
    pub scale: [f32; 2],
    pub rotation_degrees: f32,
    pub anchor: [f32; 2],
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            position: [0.0, 0.0],
            scale: [1.0, 1.0],
            rotation_degrees: 0.0,
            anchor: [0.5, 0.5],
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Crop {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

#[derive(Debug, Clone)]
pub struct SceneItem {
    pub id: SceneItemId,
    pub scene_id: SceneId,
    pub source_id: SourceId,
    pub visible: bool,
    pub locked: bool,
    pub transform: Transform,
    pub crop: Crop,
    pub z_index: i64,
}
