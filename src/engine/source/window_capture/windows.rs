//! A Window Capture on Windows: Windows Graphics Capture, by `HWND`.
//!
//! Opened on the backend's own device rather than one of its own, which is
//! what keeps the frame on the GPU all the way to the compositor — the same
//! reason every other element here shares it.
//!
//! # One window, however many items show it
//!
//! Two items showing the same window used to capture it twice. Nothing
//! refuses that — unlike a display, and unlike a camera, which falls apart
//! when it is asked twice — so this is the plain saving: one capture session
//! and one stream of textures where there were two, for a window that is
//! being shown large in one place and small in another.
//!
//! The capture is keyed by the window it resolved to rather than by what was
//! stored, so two Sources that name the same window by different titles
//! share it, and a window that is closed and opened again is a new capture
//! — which it has to be, since the old one ended with the window.

use std::sync::{Arc, Mutex};

use media_pp::elements::{
    D3d11VideoCompositorHandle, D3d11VideoCompositorInput, TeeBuilder, VideoLayer,
    WgcCaptureOptions, WgcCaptureSource,
};
use media_pp::pipeline::Pipeline;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11DeviceContext};

use crate::capture::WindowTarget;
use crate::domain::{SourceSettings, WindowCaptureTarget};
use crate::engine::backend::{BackendError, RunningSource, pipeline_ended};
use crate::engine::source::shared::{Registry, Share, Shared, SharedCapture};
use crate::engine::source::{
    FilledRack, OpenOutcome, OpenSource, filled_rack, filters, input_name,
};
use crate::snapshots::SceneItemSnapshot;

/// Every window this backend is capturing, by the `HWND` each one is of.
///
/// The machinery is [`Registry`]'s — see [`crate::engine::source::shared`].
#[derive(Default)]
pub(in crate::engine) struct WindowRegistry {
    open: Registry<()>,
}

impl SharedCapture for WindowRegistry {
    fn detach(&self, window: &str, share: Share) {
        self.open.detach(window, share);
    }

    fn set_showing(&self, window: &str, share: Share, showing: bool) {
        self.open.set_showing(window, share, showing);
    }

    fn stats(&self, window: &str, share: Share) -> Option<media_pp::stats::PipelineStats> {
        self.open.stats(window, share)
    }

    /// A window is closed, and its capture ends with it. Every item showing
    /// it is then put back to be looked for again — see
    /// `status::notice_closed_windows` — and the first to find the window
    /// open again captures it for all of them.
    fn ended(&self, window: &str, share: Share) -> bool {
        self.open
            .with_share(window, share, |capture| pipeline_ended(capture.pipeline()))
            // Gone from the registry is gone.
            .unwrap_or(true)
    }
}

/// `Absent` when the window is not on screen — see this module's parent.
pub(in crate::engine) fn open(
    device: &ID3D11Device,
    context: Arc<Mutex<ID3D11DeviceContext>>,
    handle: &D3d11VideoCompositorHandle,
    windows: &Arc<WindowRegistry>,
    item: &SceneItemSnapshot,
    layer: VideoLayer,
    fps: u32,
) -> Result<OpenOutcome, BackendError> {
    let SourceSettings::WindowCapture(settings) = &item.settings else {
        return Err("scene item is not a window capture".into());
    };
    let WindowCaptureTarget::Window { process, title } = &settings.target else {
        return Err("a portal target cannot be resolved on Windows".into());
    };
    let Some(target) = resolve(process, title) else {
        return Ok(OpenOutcome::Absent(format!(
            "no window of {process} is open"
        )));
    };

    let name = input_name(item);
    let D3d11VideoCompositorInput { sink, layer } = handle.add_source(name.clone(), layer)?;

    // The window it resolved to, not what was stored: that is what decides
    // whether two items are showing the same thing.
    let key = target.handle.to_string();
    let mut kept = None;
    let (share, _) = windows.open.attach(
        &key,
        || open_window(&target, device, fps),
        |builder, _| {
            // BGRA already, so nothing is bridged and the capture's size is
            // never read: a window is whatever size it is from one frame to
            // the next, and a D3D11 filter takes each at the size it
            // arrives. The item's own stored hint is what its rack is told,
            // for want of anything better and with nothing relying on it.
            let FilledRack { rack, filters } =
                filled_rack(&name, device, context, filters::ChainFormat::Bgra, item)?;
            kept = Some(filters);
            Ok(builder.pipe(rack).to(sink)?)
        },
    )?;
    let filters = kept.ok_or("the capture answered without finishing the branch")?;

    Ok(OpenOutcome::Open(OpenSource {
        media_file: None,
        page: None,
        source: RunningSource::Shared {
            capture: Arc::clone(windows) as Arc<dyn SharedCapture>,
            key,
            share,
        },
        layer,
        name,
        refreshed_token: None,
        filters: filters.open,
        filter_rack: filters.filter_rack,
        // `None` rather than a guess: Windows Graphics Capture settles the
        // frame size once the capture is running and `WgcCaptureSource`
        // reports none, so there is nothing here to correct the stored hint
        // with. The Linux half has one because the portal negotiates its
        // stream up front.
        negotiated_size: None,
        // Set by the engine where it is opened into a Scene's own
        // composition — see `Target`.
        nested_in: None,
        showing: true,
        running: true,
        pushed: None,
    }))
}

/// Starts capturing one window into a `Tee` nothing is attached to yet.
fn open_window(
    target: &WindowTarget,
    device: &ID3D11Device,
    fps: u32,
) -> Result<Shared<()>, BackendError> {
    // The window's own name rather than any item's: the capture outlives
    // each of them, and this is what the log and the Stats dock show it as.
    let name = format!("window-{}", target.handle);
    let source = WgcCaptureSource::open_with_device(
        name.clone(),
        HWND(target.handle as *mut std::ffi::c_void),
        WgcCaptureOptions {
            fps,
            // The pointer belongs to whoever is using the window, and a
            // recording of it is usually about what the window shows rather
            // than where its user's mouse was.
            include_cursor: false,
        },
        device,
    )?;

    let mut handle = None;
    let (pipeline, ()) = Pipeline::new(name.clone(), source, |source, context| {
        let (tee, tee_handle) =
            TeeBuilder::new(format!("{name}-tee"), context.clone()).build_dynamic()?;
        context.attach(source, 0, tee)?;
        handle = Some(tee_handle);
        Ok(())
    })?;
    let tee = handle.expect("the wire closure always produces the TeeHandle");
    pipeline.run()?;

    // Windows Graphics Capture settles the size itself and reports none, so
    // there is no size to record: every branch builds its rack from its own
    // item's hint instead, and nothing reads this one.
    Ok(Shared::new(pipeline, tee, [0, 0], ()))
}

/// The window on screen that best matches what was stored.
///
/// An exact match on both first, because that is what was chosen. Failing
/// that, the same process with any title — a window whose title changed is
/// still the window someone picked, and titles change constantly: a document
/// name, a tab, an unsaved marker. Taking the process alone when several of
/// its windows are open picks one of them arbitrarily, which is worse than
/// nothing only if the alternative were correct, and it is not: there is no
/// stored fact that tells them apart.
fn resolve(process: &str, title: &str) -> Option<WindowTarget> {
    let windows = crate::capture::windows::windows();
    windows
        .iter()
        .find(|window| window.process == process && window.title == title)
        .or_else(|| windows.iter().find(|window| window.process == process))
        .cloned()
}
