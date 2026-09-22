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
    /// How long it takes to come up when shown and to go when hidden — see
    /// [`VisibilityFades`].
    pub fades: VisibilityFades,
    pub z_index: i64,
}

/// How long one placement takes to come up when it is shown, and to go when
/// it is hidden.
///
/// Milliseconds, and zero is at once — what every item did before there was
/// a choice, and what one still does until somebody asks otherwise. Two
/// values rather than one, as OBS has them: an overlay that eases in and cuts
/// out is a common enough wish that one number for both would be the wrong
/// economy.
///
/// Only a fade. What moves is the layer's opacity, which the compositor
/// already takes for free; a slide would be the layer's position, and is a
/// different change.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VisibilityFades {
    pub show_ms: u32,
    pub hide_ms: u32,
}

/// The longest either fade can be, in milliseconds — the Scene transition's
/// own limit, since both are the same kind of thing.
pub const MAX_VISIBILITY_FADE_MS: u32 = crate::domain::MAX_TRANSITION_MS;
