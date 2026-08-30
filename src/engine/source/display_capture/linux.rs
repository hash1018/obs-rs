//! A Display Capture on Linux: the portal's screen cast.
//!
//! No registry, unlike Windows. The portal hands out a separate stream per
//! request, so nothing two SceneItems show *has* to be shared and each owns
//! its own pipeline.

use media_pp::elements::{CudaDevice, CudaVideoCompositorHandle, VideoLayer};
use media_pp::pipeline::Pipeline;
use media_pp::rate::FrameRateHandle;

use crate::engine::backend::{BackendError, RunningSource};
use crate::engine::source::{OpenSource, input_name};
use crate::snapshots::SceneItemSnapshot;

/// Opens the portal's screen cast and wires it into the compositor.
pub(in crate::engine) fn open(
    device: &CudaDevice,
    handle: &CudaVideoCompositorHandle,
    item: &SceneItemSnapshot,
    layer: VideoLayer,
    fps: u32,
) -> Result<(OpenSource, FrameRateHandle), BackendError> {
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
    // A compositor may issue a fresh token on every restore, and keeping the
    // old one then means prompting on every launch — the thing persisting it
    // was for. But declining to issue a new one is not the same as revoking
    // the old, so `None` here must never replace a token that worked: that
    // would throw away the only thing that can reopen this display.
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
    // take it. `open_display_capture` is a free function, so it is handed
    // back and the caller files it — see [`Backend::set_frame_rate`].
    let frame_rate = source.frame_rate();

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

    Ok((
        OpenSource {
            source: RunningSource(pipeline),
            layer,
            name,
            refreshed_token,
            showing: true,
            pushed: None,
        },
        frame_rate,
    ))
}
