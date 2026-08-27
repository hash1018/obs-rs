//! Opening one Source and wiring it into the compositor.
//!
//! Which `media-pp` element captures a display, and what a stored
//! `DisplayCaptureTarget` means to it, differ by operating system, so each
//! platform gets its own file beside this one. Everything else — layer
//! placement, the flat-colour input, the compositor registration — does not,
//! and stays here.
//!
//! This is the other half of `crate::capture`, which decides *what the user
//! can pick*. The two are apart on purpose: a picker has to run before any
//! pipeline exists and must not drag `media-pp` in with it, while opening
//! what was picked needs both that crate and a live compositor handle.

#[cfg_attr(target_os = "linux", path = "linux.rs")]
#[cfg_attr(not(target_os = "linux"), path = "unsupported.rs")]
mod platform;

use platform::open_display_capture;

use std::error::Error;

use std::sync::Arc;

use media_pp::{
    elements::{
        CudaDevice, CudaVideoCompositorHandle, CudaVideoLayerHandle, VideoFit, VideoLayer,
        VideoRect,
    },
    pipeline::Pipeline,
};

use crate::domain::{SourceKind, SourceSettings, Transform};
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
    // NV12 carries no alpha, so a Color Source's own is the layer's opacity
    // rather than something the blend could read out of its pixels.
    if let SourceSettings::Color(settings) = &item.settings {
        layer.opacity = f32::from(settings.rgba[3]) / 255.0;
    }
    layer
}

/// Starts whatever Source the item names, or reports that this build cannot.
pub(super) fn open_source(
    device: &CudaDevice,
    handle: &CudaVideoCompositorHandle,
    item: &SceneItemSnapshot,
    layer: VideoLayer,
    fps: u32,
) -> Result<OpenSource, OpenError> {
    match item.kind {
        SourceKind::DisplayCapture => open_display_capture(device, handle, item, layer, fps),
        SourceKind::Color => open_color_source(device, handle, item, layer),
        kind => Err(format!("{kind:?} is not connected to the compositor yet").into()),
    }
}

/// Feeds the compositor one frame of flat colour and leaves it there.
///
/// Pushed once rather than per frame: the compositor keeps the latest frame
/// each input gave it, and a colour that never changes never needs another.
/// Position, size and opacity are the layer's, so nothing here is redrawn
/// when the item moves.
fn open_color_source(
    device: &CudaDevice,
    handle: &CudaVideoCompositorHandle,
    item: &SceneItemSnapshot,
    layer: VideoLayer,
) -> Result<OpenSource, OpenError> {
    use media_pp::elements::{
        AppSource, CudaConverter, CudaFrameFormat, CudaUpload, CudaVideoCompositorInput,
    };

    let SourceSettings::Color(settings) = &item.settings else {
        return Err("scene item is not a color source".into());
    };
    let width = (settings.size[0].round() as u32).max(2) & !1;
    let height = (settings.size[1].round() as u32).max(2) & !1;

    let name = input_name(item);
    let (source, pusher) = AppSource::new(name.clone(), 1);
    // BGRA in, so `CudaConverter` performs the RGB-to-BT.709 conversion the
    // compositor expects instead of this having its own copy of that matrix.
    let upload = CudaUpload::new(
        format!("{name}-upload"),
        device,
        CudaFrameFormat::Bgra,
        width,
        height,
    )?;
    let converter = CudaConverter::new(format!("{name}-convert"), device, width, height)?;

    let CudaVideoCompositorInput { sink, layer } = handle.add_source(name.clone(), layer)?;
    let pipeline = Pipeline::new(name.clone(), source, move |source, context| {
        let branch = context.branch().pipe(upload).pipe(converter).to(sink)?;
        context.attach(source, 0, branch)?;
        Ok(())
    })?;
    pipeline.run()?;
    pusher.push(flat_bgra(width, height, settings.rgba))?;

    Ok(OpenSource {
        pipeline,
        layer,
        name,
        refreshed_token: None,
        showing: true,
    })
}

/// One BGRA frame filled with a single colour, ready for `CudaUpload`.
fn flat_bgra(width: u32, height: u32, rgba: [u8; 4]) -> media_pp::buffer::MediaBuffer {
    use media_pp::{buffer::MediaBuffer, ffmpeg, pool::UnboundObjectPool};

    let mut frame = ffmpeg::frame::Video::new(ffmpeg::format::Pixel::BGRA, width, height);
    let stride = frame.stride(0);
    // Opaque: the item's own alpha is the layer's opacity, and applying it
    // twice would darken the colour against the Canvas.
    let pixel = [rgba[2], rgba[1], rgba[0], 255];
    let row: Vec<u8> = pixel
        .iter()
        .copied()
        .cycle()
        .take(width as usize * 4)
        .collect();
    let data = frame.data_mut(0);
    for line in 0..height as usize {
        data[line * stride..line * stride + row.len()].copy_from_slice(&row);
    }

    // `MediaBuffer::Video` carries pooled frames; this one has no pool behind
    // it and never returns to one, which an unbound pool of zero expresses.
    let pool = UnboundObjectPool::new(0, ffmpeg::frame::Video::empty, |_| {});
    let mut slot = pool.get();
    *slot = frame;
    MediaBuffer::Video(std::sync::Arc::new(slot))
}

/// The name a SceneItem's compositor input is registered under.
fn input_name(item: &SceneItemSnapshot) -> String {
    format!("scene-item-{}", item.id.0)
}
