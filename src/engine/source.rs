//! Opening one capture Source and wiring it into the compositor.
//!
//! This is the one part of the engine that is genuinely per-platform: which
//! `media-pp` element captures a display, and what a stored
//! `DisplayCaptureTarget` means to it, differ by operating system. Everything
//! around it — reconciling against the snapshot, layer placement, the frame
//! handoff — does not, and stays out of here.

use std::error::Error;

use std::sync::Arc;

use media_pp::{
    elements::{
        CudaDevice, CudaVideoCompositorHandle, CudaVideoLayerHandle, VideoFit, VideoLayer,
        VideoRect,
    },
    pipeline::Pipeline,
};

use crate::domain::Transform;
use crate::snapshots::SceneItemSnapshot;

pub(super) type OpenError = Box<dyn Error + Send + Sync>;

/// A capture Source that is running, and the controls for its layer.
pub(super) struct OpenSource {
    pub(super) pipeline: Arc<Pipeline>,
    pub(super) layer: CudaVideoLayerHandle,
    pub(super) name: String,
    /// The token the portal handed back, when it differs from the one it was
    /// given. `None` means the stored token is still current.
    pub(super) refreshed_token: Option<Option<String>>,
    /// Whether the Source is in the Scene being shown. A Source whose item
    /// left the Scene stays open but stops running, so coming back to that
    /// Scene costs a resume rather than a portal round trip.
    pub(super) showing: bool,
}

/// Where a SceneItem's layer sits on the Canvas, and in what order.
///
/// The rectangle already carries the Source's own size scaled by the item's
/// Transform, so the fit is [`VideoFit::Stretch`]: whatever aspect the user
/// asked for is expressed in that rectangle, and letterboxing inside it would
/// second-guess them.
pub(super) fn layer_for(
    item: &SceneItemSnapshot,
    transform: Transform,
    z_index: i32,
) -> VideoLayer {
    let [x, y, width, height] = item.canvas_rect(transform);
    let mut layer = VideoLayer::new(VideoRect::new(
        x.round() as i32,
        y.round() as i32,
        (width.round() as u32).max(1),
        (height.round() as u32).max(1),
    ));
    layer.z_index = z_index;
    layer.visible = item.visible;
    layer.fit = VideoFit::Stretch;
    layer
}

/// The name a SceneItem's compositor input is registered under.
fn input_name(item: &SceneItemSnapshot) -> String {
    format!("scene-item-{}", item.id.0)
}

#[cfg(target_os = "linux")]
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

#[cfg(not(target_os = "linux"))]
pub(super) fn open_display_capture(
    _device: &CudaDevice,
    _handle: &CudaVideoCompositorHandle,
    _item: &SceneItemSnapshot,
    _layer: VideoLayer,
    _fps: u32,
) -> Result<OpenSource, OpenError> {
    Err("display capture is not connected on this platform yet".into())
}
