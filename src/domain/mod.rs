mod audio;
mod canvas;
mod scene;
mod scene_item;
mod source;

pub use audio::{AudioSource, AudioSourceId, AudioSourceKind, MAX_GAIN_DB, MIN_GAIN_DB};
pub use canvas::SceneCanvas;
pub use scene::{Scene, SceneId};
pub use scene_item::{Crop, SceneItem, SceneItemId, Transform};
pub use source::{
    ColorSourceSettings, DisplayCaptureSettings, DisplayCaptureTarget, DrawingSourceSettings,
    ImageSourceSettings, MediaFileSettings, RtspSourceSettings, RtspTransport, Source, SourceId,
    SourceKind, SourceSettings, Stroke, VideoCaptureMode, VideoCaptureSettings,
    WindowCaptureSettings, WindowCaptureTarget,
};
