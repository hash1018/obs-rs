//! A Display Capture on macOS: ScreenCaptureKit, of one display, shared
//! between the items showing it.
//!
//! Everything it opens is [`screencapturekit_capture`], which a Window Capture opens
//! too. What is here is the part that is a *display*: which one a stored
//! name means — the display id it carries, see `capture::macos` — and that a
//! display that is not connected is a state rather than a failure.

use std::sync::Arc;

use media_pp::elements::{ScreenCaptureKitTarget, VideoLayer};

use crate::domain::{DisplayCaptureTarget, SourceSettings};
use crate::engine::backend::{BackendError, Compositor, Gpu};
use crate::engine::source::OpenOutcome;
use crate::engine::source::screencapturekit_capture::{self, ScreenRegistry};
use crate::snapshots::SceneItemSnapshot;

/// `Absent` when the display is not connected: a laptop taken away from its
/// monitor is an ordinary thing, and the Source comes back when it is.
pub(in crate::engine) fn open(
    screens: &Arc<ScreenRegistry>,
    gpu: &Gpu,
    handle: &Compositor,
    item: &SceneItemSnapshot,
    layer: VideoLayer,
    fps: u32,
) -> Result<OpenOutcome, BackendError> {
    let SourceSettings::DisplayCapture(settings) = &item.settings else {
        return Err("scene item is not a display capture".into());
    };
    let DisplayCaptureTarget::MonitorName(stored) = &settings.target else {
        return Err("a portal target cannot be resolved on macOS".into());
    };
    let monitors = crate::capture::macos::monitors();
    let Some(display) = crate::capture::macos::display_id(stored).filter(|id| {
        monitors
            .iter()
            .any(|monitor| crate::capture::macos::display_id(&monitor.name) == Some(*id))
    }) else {
        return Ok(OpenOutcome::Absent(format!("{stored} is not connected")));
    };
    screencapturekit_capture::open(
        screens,
        ScreenCaptureKitTarget::Display(display),
        gpu,
        handle,
        item,
        layer,
        fps,
    )
}
