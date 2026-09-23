//! A Window Capture on Linux: the portal's screen cast, listing windows.
//!
//! The same capture a Display Capture opens — see [`portal_capture`] — told
//! to list windows rather than monitors. Nothing is resolved here: the portal
//! names no window, so there is no "not found" state. This opens what the
//! portal hands over, or fails.

use std::sync::Arc;

use media_pp::elements::{CaptureSourceKind, CudaDevice, CudaVideoCompositorHandle, VideoLayer};
use media_pp::rate::FrameRateHandle;

use crate::domain::{SourceSettings, WindowCaptureTarget};
use crate::engine::backend::BackendError;
use crate::engine::source::{OpenSource, portal_capture};
use crate::snapshots::SceneItemSnapshot;

pub(in crate::engine) fn open(
    device: &Arc<CudaDevice>,
    handle: &CudaVideoCompositorHandle,
    item: &SceneItemSnapshot,
    layer: VideoLayer,
    fps: u32,
) -> Result<(OpenSource, FrameRateHandle), BackendError> {
    let SourceSettings::WindowCapture(settings) = &item.settings else {
        return Err("scene item is not a window capture".into());
    };
    let WindowCaptureTarget::Portal { restore_token } = &settings.target else {
        return Err("a named window cannot be resolved through the portal".into());
    };

    portal_capture::open(
        CaptureSourceKind::Window,
        restore_token.clone(),
        device,
        handle,
        item,
        layer,
        fps,
    )
}
