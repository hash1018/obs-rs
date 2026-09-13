//! A Window Capture on Windows: Windows Graphics Capture, by `HWND`.
//!
//! Opened on the backend's own device rather than one of its own, which is
//! what keeps the frame on the GPU all the way to the compositor — the same
//! reason every other element here shares it.

use media_pp::elements::{
    D3d11VideoCompositorHandle, D3d11VideoCompositorInput, VideoLayer, WgcCaptureOptions,
    WgcCaptureSource,
};
use media_pp::pipeline::Pipeline;
use std::sync::{Arc, Mutex};
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11DeviceContext};

use crate::capture::WindowTarget;
use crate::domain::{SourceSettings, WindowCaptureTarget};
use crate::engine::backend::{BackendError, RunningSource};
use crate::engine::source::{
    FilledRack, OpenOutcome, OpenSource, filled_rack, filters, hinted_size, input_name,
};
use crate::snapshots::SceneItemSnapshot;

/// `Absent` when the window is not on screen — see this module's parent.
pub(in crate::engine) fn open(
    device: &ID3D11Device,
    context: Arc<Mutex<ID3D11DeviceContext>>,
    handle: &D3d11VideoCompositorHandle,
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

    // BGRA already, so nothing is bridged and the size is never read: a
    // window is whatever size it is from one frame to the next, and a D3D11
    // filter takes each at the size it arrives. The stored hint is what the
    // rack is told, for want of anything better and with nothing relying on
    // it.
    let FilledRack { rack, filters } = filled_rack(
        &name,
        device,
        context,
        filters::ChainFormat::Bgra,
        hinted_size(item),
        item,
    )?;

    let D3d11VideoCompositorInput { sink, layer } = handle
        .add_source(name.clone(), layer)?
        .ok_or("the compositor is no longer running")?;
    let pipeline = Pipeline::new(name.clone(), source, move |source, context| {
        let branch = context.branch().pipe(rack).to(sink)?;
        context.attach(source, 0, branch)?;
        Ok(())
    })?;
    pipeline.run()?;

    Ok(OpenOutcome::Open(OpenSource {
        media_file: None,
        source: RunningSource::Owned(pipeline),
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
        showing: true,
        running: true,
        pushed: None,
    }))
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
