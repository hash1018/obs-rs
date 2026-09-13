//! A Color Source: one flat colour, pushed once.
//!
//! Pushed once rather than per frame — the compositor keeps the latest frame
//! each input gave it, and a colour that never changes never needs another.
//! Position, size and opacity are the layer's, so nothing here is redrawn
//! when the item moves.
//!
//! The two backends differ only in which element carries the frame to the
//! GPU. Both hand the compositor BGRA, filtered or not: the CUDA one used to
//! convert to NV12 on the way, which a key's alpha could not have survived,
//! and which a flat colour pushed once never needed.

use std::sync::Arc;

use media_pp::{buffer::MediaBuffer, ffmpeg, pipeline::Pipeline, pool::UnboundObjectPool};

use crate::domain::SourceSettings;
use crate::snapshots::SceneItemSnapshot;

use super::super::backend::{BackendError, Layer, RunningSource};
use super::{
    FilledRack, OpenSource, PushedContent, PushedSurface, SourceFilters, filters, input_name,
};

/// One BGRA frame filled with a single colour, ready for a backend's upload
/// element. Backend-independent: both compositors take their Color Source
/// this way, differing only in which upload carries it to the GPU.
#[allow(dead_code)]
pub(in crate::engine) fn flat_bgra(width: u32, height: u32, rgba: [u8; 4]) -> MediaBuffer {
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
    MediaBuffer::Video(Arc::new(slot))
}

/// The size the frame is made at, which is the source's own rather than the
/// layer's: the layer scales it, and a Color Source has nothing to lose to
/// that.
fn size(item: &SceneItemSnapshot) -> Result<([u32; 2], [u8; 4]), BackendError> {
    let SourceSettings::Color(settings) = &item.settings else {
        return Err("scene item is not a color source".into());
    };
    Ok((
        [
            (settings.size[0].round() as u32).max(2) & !1,
            (settings.size[1].round() as u32).max(2) & !1,
        ],
        settings.rgba,
    ))
}

/// What both implementations return, so the difference between them stays the
/// pipeline and nothing else.
fn opened(
    name: String,
    source: RunningSource,
    layer: Layer,
    pusher: media_pp::elements::AppSourceHandle,
    filters: SourceFilters,
    size: [u32; 2],
    rgba: [u8; 4],
) -> Result<OpenSource, BackendError> {
    let frame = flat_bgra(size[0], size[1], rgba);
    pusher.push(frame.clone())?;
    Ok(OpenSource {
        media_file: None,
        // Its size is its own rather than something a device answered
        // with, so there is nothing to correct.
        negotiated_size: None,
        source,
        layer,
        name,
        refreshed_token: None,
        filters: filters.open,
        filter_rack: filters.filter_rack,
        showing: true,
        running: true,
        // Held, not dropped here: an `AppSource` runs only while a handle to
        // it exists, and this one used to go out of scope in the same breath
        // as its only frame.
        pushed: Some(PushedSurface {
            pusher,
            size,
            content: PushedContent::Color(rgba),
            frame,
        }),
    })
}

#[cfg(target_os = "windows")]
pub(in crate::engine) fn open(
    device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
    context: Arc<std::sync::Mutex<windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext>>,
    handle: &media_pp::elements::D3d11VideoCompositorHandle,
    item: &SceneItemSnapshot,
    layer: media_pp::elements::VideoLayer,
) -> Result<OpenSource, BackendError> {
    use media_pp::elements::{AppSource, D3d11Upload, D3d11VideoCompositorInput};

    let (size, rgba) = size(item)?;
    let name = input_name(item);
    let (source, pusher) = AppSource::new(name.clone(), 1);
    // BGRA in, BGRA composited: there is no colour-space conversion between
    // the upload and the compositor at all.
    let upload = D3d11Upload::new(format!("{name}-upload"), device, size[0], size[1]);
    let FilledRack { rack, filters } = super::filled_rack(
        &name,
        device,
        context,
        filters::ChainFormat::Bgra,
        size,
        item,
    )?;

    let D3d11VideoCompositorInput { sink, layer } = handle
        .add_source(name.clone(), layer)?
        .ok_or("the compositor is no longer running")?;
    let pipeline = Pipeline::new(name.clone(), source, move |source, context| {
        let branch = context.branch().pipe(upload).pipe(rack).to(sink)?;
        context.attach(source, 0, branch)?;
        Ok(())
    })?;
    pipeline.run()?;

    opened(
        name,
        RunningSource::Owned(pipeline),
        layer,
        pusher,
        filters,
        size,
        rgba,
    )
}

#[cfg(target_os = "linux")]
pub(in crate::engine) fn open(
    device: &Arc<media_pp::elements::CudaDevice>,
    handle: &media_pp::elements::CudaVideoCompositorHandle,
    item: &SceneItemSnapshot,
    layer: media_pp::elements::VideoLayer,
) -> Result<OpenSource, BackendError> {
    use media_pp::elements::{AppSource, CudaFrameFormat, CudaUpload, CudaVideoCompositorInput};

    let (size, rgba) = size(item)?;
    let name = input_name(item);
    let (source, pusher) = AppSource::new(name.clone(), 1);
    // BGRA all the way, as a Drawing's is: the compositor takes a BGRA layer
    // and blends it itself, and a converter to NV12 here would have been
    // where a key's alpha was lost.
    let upload = CudaUpload::new(
        format!("{name}-upload"),
        device,
        CudaFrameFormat::Bgra,
        size[0],
        size[1],
    )?;
    let FilledRack { rack, filters } =
        super::filled_rack(&name, device, filters::ChainFormat::Bgra, size, item)?;

    let CudaVideoCompositorInput { sink, layer } = handle.add_source(name.clone(), layer)?;
    let pipeline = Pipeline::new(name.clone(), source, move |source, context| {
        let branch = context.branch().pipe(upload).pipe(rack).to(sink)?;
        context.attach(source, 0, branch)?;
        Ok(())
    })?;
    pipeline.run()?;

    opened(
        name,
        RunningSource(pipeline),
        layer,
        pusher,
        filters,
        size,
        rgba,
    )
}
