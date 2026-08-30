//! A Window Capture on Linux: the portal's screen cast, filtered to windows.
//!
//! The same element a Display Capture uses, told to list windows rather than
//! monitors. Nothing is resolved here — the portal names no window, so there
//! is no "not found" state and this never answers `Ok(None)`.

use media_pp::elements::{
    CaptureSourceKind, CudaConverter, CudaDevice, CudaVideoCompositorHandle,
    CudaVideoCompositorInput, PipeWireScreenCaptureOptions, PipeWireScreenCaptureSource,
    VideoLayer,
};
use media_pp::pipeline::Pipeline;
use media_pp::rate::FrameRateHandle;

use crate::domain::{SourceSettings, WindowCaptureTarget};
use crate::engine::backend::{BackendError, RunningSource};
use crate::engine::source::{OpenSource, input_name};
use crate::snapshots::SceneItemSnapshot;

pub(in crate::engine) fn open(
    device: &CudaDevice,
    handle: &CudaVideoCompositorHandle,
    item: &SceneItemSnapshot,
    layer: VideoLayer,
    fps: u32,
) -> Result<Option<(OpenSource, FrameRateHandle)>, BackendError> {
    let SourceSettings::WindowCapture(settings) = &item.settings else {
        return Err("scene item is not a window capture".into());
    };
    let WindowCaptureTarget::Portal { restore_token } = &settings.target else {
        return Err("a named window cannot be resolved through the portal".into());
    };
    let restore_token = restore_token.clone();

    let name = input_name(item);
    // GPU capture, as a display's is: the window lands in CUDA surfaces and
    // never reaches system memory. The CPU path would hand the compositor
    // frames it cannot take.
    let (source, format, refreshed_token) = PipeWireScreenCaptureSource::open_gpu(
        name.clone(),
        PipeWireScreenCaptureOptions {
            fps,
            source_kind: CaptureSourceKind::Window,
            include_cursor: false,
            restore_token: restore_token.clone(),
        },
        device,
    )?;
    // A token the compositor declined to reissue must never replace one that
    // worked — see the Display Capture's own note, which this follows.
    let refreshed_token = refreshed_token
        .filter(|token| Some(token) != restore_token.as_ref())
        .map(|token| {
            eprintln!("\"{}\": the portal issued a new restore token", item.name);
            Some(token)
        });
    eprintln!(
        "\"{}\": opened {}x{} (token {})",
        item.name,
        format.width,
        format.height,
        if restore_token.is_some() {
            "restored"
        } else {
            "picked"
        }
    );

    // Before the move into the `Pipeline` below, which is the only chance to
    // take it.
    let frame_rate = source.frame_rate();

    // The size the portal negotiated, not the item's: a window is whatever
    // size it happens to be, and the converter has to be built for what
    // actually arrives.
    let converter = CudaConverter::new(
        format!("{name}-convert"),
        device,
        format.width,
        format.height,
    )?;

    let CudaVideoCompositorInput { sink, layer } = handle.add_source(name.clone(), layer)?;
    let pipeline = Pipeline::new(name.clone(), source, move |source, context| {
        let branch = context.branch().pipe(converter).to(sink)?;
        context.attach(source, 0, branch)?;
        Ok(())
    })?;
    pipeline.run()?;

    Ok(Some((
        OpenSource {
            source: RunningSource(pipeline),
            layer,
            name,
            refreshed_token,
            showing: true,
            pushed: None,
        },
        frame_rate,
    )))
}
