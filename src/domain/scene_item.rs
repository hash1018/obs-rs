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
    /// How see-through this placement is, from nothing to one.
    ///
    /// The item's rather than the Source's: the same camera can be solid in
    /// one Scene and a wash in another. Applied where the layer is drawn, so
    /// it costs nothing — unlike a colour correction filter's own opacity,
    /// which is a pass over the picture and belongs to the Source.
    pub opacity: f32,
    pub z_index: i64,
}
