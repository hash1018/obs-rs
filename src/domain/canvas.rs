/// Logical composition space for a scene.
///
/// Its size does not change when the application window or PreviewViewport is resized.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SceneCanvas {
    pub width: f32,
    pub height: f32,
}

impl SceneCanvas {
    pub const DEFAULT: Self = Self {
        width: 1920.0,
        height: 1080.0,
    };

    pub fn aspect_ratio(self) -> f32 {
        self.width / self.height
    }
}
