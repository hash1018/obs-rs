//! What a Display Capture and a Window Capture both are on Linux: one screen
//! cast from xdg-desktop-portal, into the compositor.
//!
//! The two kinds are one capture here, unlike on Windows, where a display is
//! desktop duplication and a window is Windows.Graphics.Capture — two APIs
//! with nothing in common, each with its own file. The portal has one, and
//! what tells a display from a window is which kind of thing its picker is
//! asked to list. So the difference between the two Sources on this platform
//! is the [`CaptureSourceKind`] they pass and where their restore token comes
//! from; everything after that is here.
//!
//! No registry either way. The portal hands out a separate stream per
//! request, so nothing two SceneItems show *has* to be shared and each owns
//! its own pipeline — which is the difference `RunningSource` is shaped
//! around, and why it is a type each backend defines for itself.

use media_pp::element::BoxFilter;
use media_pp::elements::{
    CaptureSourceKind, CompositorInput, CudaConverter, CudaFrameFormat,
    PipeWireScreenCaptureOptions, PipeWireScreenCaptureSource, VideoLayer,
};
use media_pp::pipeline::Pipeline;
use media_pp::rate::FrameRateHandle;

use crate::engine::backend::{BackendError, Compositor, Gpu, RunningSource};
use crate::engine::source::{FilledRack, OpenSource, filled_rack, filters, input_name};
use crate::snapshots::SceneItemSnapshot;

/// Opens the portal's screen cast for `kind` and wires it into the
/// compositor.
///
/// `restore_token` is what the portal issued last time this Source opened,
/// where it issued one: handing it back is what reopens the same display or
/// window without asking again.
pub(in crate::engine) fn open(
    kind: CaptureSourceKind,
    restore_token: Option<String>,
    gpu: &Gpu,
    handle: &Compositor,
    item: &SceneItemSnapshot,
    layer: VideoLayer,
    fps: u32,
) -> Result<(OpenSource, FrameRateHandle), BackendError> {
    let name = input_name(item);
    let options = PipeWireScreenCaptureOptions {
        frame_rate: media_pp::ffmpeg::Rational::new(fps as i32, 1),
        source_kind: kind,
        include_cursor: false,
        restore_token: restore_token.clone(),
    };
    // Blocking, and it can sit here indefinitely: an unrecognised token makes
    // the portal show its dialog and wait for the user. Sources are opened one
    // at a time, so the rest wait behind whichever one is asking.
    let (source, format, refreshed_token, bridge, incoming) = match gpu {
        // GPU capture: what is captured lands in CUDA surfaces and never
        // reaches system memory. It negotiates DMA-BUF only and fails rather
        // than falling back, which is the point — a silent CPU path would undo
        // the whole arrangement, and would hand the compositor frames it
        // cannot take.
        //
        // Capture gives BGRA and the compositor works in NV12; nothing
        // between them converts, so the converter is not optional.
        Gpu::Cuda(device) => {
            let (source, format, token) =
                PipeWireScreenCaptureSource::open_gpu(name.clone(), options, device)?;
            let converter =
                CudaConverter::new(format!("{name}-convert"), device, CudaFrameFormat::Nv12)?;
            let bridge: BoxFilter = BoxFilter::new(converter);
            (source, format, token, bridge, filters::ChainFormat::Nv12)
        }
        // Nothing takes a DMA-BUF into a Vulkan frame here, so the capture
        // comes through system memory and is uploaded as it is — BGRA, which
        // the compositor draws as well as NV12, and which the filters want
        // anyway.
        Gpu::Vulkan(_) => {
            let (source, format, token) = PipeWireScreenCaptureSource::open(name.clone(), options)?;
            let upload = gpu.upload(format!("{name}-upload"), filters::ChainFormat::Bgra);
            (source, format, token, upload, filters::ChainFormat::Bgra)
        }
    };
    // A compositor may issue a fresh token on every restore, and keeping the
    // old one then means prompting on every launch — the thing persisting it
    // was for. But declining to issue a new one is not the same as revoking
    // the old, so `None` here must never replace a token that worked: that
    // would throw away the only thing that can reopen this capture.
    let refreshed_token = refreshed_token
        .filter(|token| Some(token) != restore_token.as_ref())
        .map(|token| {
            tracing::info!("\"{}\": the portal issued a new restore token", item.name);
            Some(token)
        });
    tracing::info!(
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
    // take it. This is a free function, so the rate handle is handed back and
    // the caller files it — see [`crate::engine::backend::Backend`]'s own
    // `set_frame_rate`.
    let frame_rate = source.frame_rate();

    // After the bridge, as a camera's rack is after its upload — see the
    // `filters` module for why a capture is not filtered before it.
    let FilledRack { rack, filters } = filled_rack(&name, gpu, incoming, item)?;

    let CompositorInput { sink, layer } = handle.add_source(name.clone(), layer)?;
    let (pipeline, ()) = Pipeline::new(name.clone(), source, move |source, context| {
        let branch = context.branch().pipe(bridge).pipe(rack).to(sink)?;
        context.attach(source, 0, branch)?;
        Ok(())
    })?;
    pipeline.run()?;

    Ok((
        OpenSource {
            media_file: None,
            page: None,
            // What the portal negotiated, not what the item stored: a window
            // is whatever size it happens to be, and a display is whatever
            // the picker was pointed at.
            negotiated_size: Some([format.width, format.height]),
            source: RunningSource::Owned(pipeline),
            layer,
            name,
            refreshed_token,
            filters: filters.open,
            filter_rack: filters.filter_rack,
            // Set by the engine where it is opened into a Scene's own
            // composition — see `Target`.
            nested_in: None,
            showing: true,
            running: true,
            pushed: None,
        },
        frame_rate,
    ))
}
