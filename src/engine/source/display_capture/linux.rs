//! A Display Capture on Linux: the portal's screen cast, listing monitors.
//!
//! Everything it opens is [`portal_capture`], which a Window Capture opens
//! too — on this platform the two are one capture told to list different
//! things. What is here is the part that is a *display*: which token reopens
//! it, and what a target the portal cannot resolve means.

use media_pp::elements::{CaptureSourceKind, VideoLayer};
use media_pp::rate::FrameRateHandle;

use crate::domain::{DisplayCaptureTarget, SourceSettings};
use crate::engine::backend::{BackendError, Compositor, Gpu};
use crate::engine::source::{OpenSource, portal_capture};
use crate::snapshots::SceneItemSnapshot;

/// Opens the portal's screen cast and wires it into the compositor.
pub(in crate::engine) fn open(
    gpu: &Gpu,
    handle: &Compositor,
    item: &SceneItemSnapshot,
    layer: VideoLayer,
    fps: u32,
) -> Result<(OpenSource, FrameRateHandle), BackendError> {
    let SourceSettings::DisplayCapture(settings) = &item.settings else {
        return Err("scene item is not a display capture".into());
    };
    let restore_token = match &settings.target {
        DisplayCaptureTarget::Portal { restore_token } => restore_token.clone(),
        // An X11 display name means nothing to the portal, which owns the
        // choice on Wayland. Leaving the token unset makes it prompt, the only
        // thing it can do with a target it cannot resolve.
        DisplayCaptureTarget::MonitorName(_) => None,
    };

    portal_capture::open(
        CaptureSourceKind::Monitor,
        restore_token,
        gpu,
        handle,
        item,
        layer,
        fps,
    )
}
