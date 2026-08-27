//! Desktop capture on platforms with no capture element wired up yet.
//!
//! Windows has `DxgiCaptureSource` and `WgcCaptureSource` waiting for it in
//! `media-pp`; until one is connected, saying so is better than a Source that
//! opens and never produces a frame.

use media_pp::elements::{CudaDevice, CudaVideoCompositorHandle, VideoLayer};

use crate::snapshots::SceneItemSnapshot;

use super::{OpenError, OpenSource};

pub(super) fn open_display_capture(
    _device: &CudaDevice,
    _handle: &CudaVideoCompositorHandle,
    _item: &SceneItemSnapshot,
    _layer: VideoLayer,
    _fps: u32,
) -> Result<OpenSource, OpenError> {
    Err("display capture is not connected on this platform yet".into())
}
