//! A Window Capture on macOS: ScreenCaptureKit, of one window, shared between
//! the items showing it.
//!
//! Resolved as on Windows — the owning application and the title, matched
//! against what is on screen now; see the parent's docs — and opened by the
//! window number the match has, which is the one ScreenCaptureKit captures
//! by, and which the stream is shared under. Everything it opens is
//! [`screencapturekit_capture`].

use std::sync::Arc;

use media_pp::elements::{ScreenCaptureKitTarget, VideoLayer};

use crate::capture::WindowTarget;
use crate::domain::{SourceSettings, WindowCaptureTarget};
use crate::engine::backend::{BackendError, Compositor, Gpu};
use crate::engine::source::OpenOutcome;
use crate::engine::source::screencapturekit_capture::{self, ScreenRegistry};
use crate::snapshots::SceneItemSnapshot;

/// `Absent` when the window is not on screen — see this module's parent.
pub(in crate::engine) fn open(
    screens: &Arc<ScreenRegistry>,
    gpu: &Gpu,
    handle: &Compositor,
    item: &SceneItemSnapshot,
    layer: VideoLayer,
    fps: u32,
) -> Result<OpenOutcome, BackendError> {
    let SourceSettings::WindowCapture(settings) = &item.settings else {
        return Err("scene item is not a window capture".into());
    };
    let WindowCaptureTarget::Window { process, title } = &settings.target else {
        return Err("a portal target cannot be resolved on macOS".into());
    };
    let Some(target) = resolve(process, title) else {
        return Ok(OpenOutcome::Absent(format!(
            "no window of {process} is open"
        )));
    };
    screencapturekit_capture::open(
        screens,
        ScreenCaptureKitTarget::Window(target.handle as u32),
        gpu,
        handle,
        item,
        layer,
        fps,
    )
}

/// The window on screen that best matches what was stored — the Windows
/// twin's rule: both exactly, then the same application with any title.
fn resolve(process: &str, title: &str) -> Option<WindowTarget> {
    let windows = crate::capture::macos::windows();
    windows
        .iter()
        .find(|window| window.process == process && window.title == title)
        .or_else(|| windows.iter().find(|window| window.process == process))
        .cloned()
}
