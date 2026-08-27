//! Desktop capture on Linux, through `xdg-desktop-portal` and PipeWire.

use media_pp::{
    elements::{CudaDevice, CudaVideoCompositorHandle, VideoLayer},
    pipeline::Pipeline,
};

use crate::snapshots::SceneItemSnapshot;

use super::{OpenError, OpenSource, input_name};

pub(super) fn open_display_capture(
    device: &CudaDevice,
    handle: &CudaVideoCompositorHandle,
    item: &SceneItemSnapshot,
    layer: VideoLayer,
    fps: u32,
) -> Result<OpenSource, OpenError> {
    use media_pp::elements::{
        CaptureSourceKind, CudaConverter, CudaVideoCompositorInput, PipeWireScreenCaptureOptions,
        PipeWireScreenCaptureSource,
    };

    use crate::domain::{DisplayCaptureTarget, SourceSettings};

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

    let name = input_name(item);
    // Blocking, and it can sit here indefinitely: an unrecognised token makes
    // the portal show its dialog and wait for the user. Sources are opened one
    // at a time, so the rest wait behind whichever one is asking.
    // GPU capture: the desktop lands in CUDA surfaces and never reaches system
    // memory. It negotiates DMA-BUF only and fails rather than falling back,
    // which is the point — a silent CPU path would undo the whole arrangement.
    let (source, format, refreshed_token) = PipeWireScreenCaptureSource::open_gpu(
        name.clone(),
        PipeWireScreenCaptureOptions {
            fps,
            source_kind: CaptureSourceKind::Monitor,
            include_cursor: false,
            restore_token: restore_token.clone(),
        },
        device,
    )?;
    // A compositor may issue a fresh token on every restore. Keeping the old
    // one then means prompting on every launch, which is the thing persisting
    // it was for.
    let refreshed_token = (refreshed_token != restore_token).then_some(refreshed_token);

    // Capture gives BGRA and the compositor works in NV12; nothing between
    // them converts, so this element is not optional.
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

    Ok(OpenSource {
        pipeline,
        layer,
        name,
        refreshed_token,
        showing: true,
    })
}
