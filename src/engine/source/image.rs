//! An Image Source: one still picture, decoded once and pushed once.
//!
//! Pushed once for the same reason a Color Source is — the compositor keeps
//! the latest frame each input gave it, and a picture that never changes never
//! needs another. Position, size and opacity belong to the layer, so moving or
//! resizing the item costs nothing here.
//!
//! # Decoded through FFmpeg, not an image library
//!
//! A PNG is a one-frame container as far as FFmpeg is concerned, so the same
//! demuxer and decoder that open a video open a picture. That is worth more
//! than the convenience of an image crate: it is already here, it already
//! carries every format this application will be asked for, and there is one
//! decoder in the tree rather than two.
//!
//! # Missing is not failure
//!
//! A path is stored as it was picked and never resolved to anything else, so a
//! file that has moved or a drive that is not mounted leaves the Source
//! [`SourceState::Missing`] and looked for again — the same standing a closed
//! window and an absent media file have. A file that is *there* and will not
//! decode is a real failure.
//!
//! [`SourceState::Missing`]: crate::engine::SourceState

use std::path::Path;
use std::sync::Arc;

use media_pp::{buffer::MediaBuffer, ffmpeg, pipeline::Pipeline, pool::UnboundObjectPool};

use crate::domain::SourceSettings;
use crate::snapshots::SceneItemSnapshot;

use super::super::backend::{BackendError, Layer, RunningSource};
use super::{OpenSource, PushedContent, PushedSurface, input_name};

/// The file this item names, or `None` where it is not there right now.
fn settings(item: &SceneItemSnapshot) -> Result<Option<&Path>, BackendError> {
    let SourceSettings::Image(settings) = &item.settings else {
        return Err("scene item is not an image source".into());
    };
    Ok(settings.path.is_file().then_some(settings.path.as_path()))
}

/// The picture as one BGRA frame, and the size it was made at.
///
/// The size is the file's own, rounded down to even in both axes: the CUDA
/// compositor's canvas is NV12, whose chroma planes are half-sized, so an odd
/// dimension has no whole pixel to carry. Rounding here rather than in the
/// upload means the scaler that is already converting the format does it, at
/// no extra pass.
fn decode(path: &Path) -> Result<(MediaBuffer, [u32; 2]), BackendError> {
    let mut input = ffmpeg::format::input(path)?;
    let stream = input
        .streams()
        .find(|stream| stream.parameters().medium() == ffmpeg::media::Type::Video)
        .ok_or("the file holds no picture")?;
    let index = stream.index();
    let mut decoder = ffmpeg::codec::context::Context::from_parameters(stream.parameters())?
        .decoder()
        .video()?;

    // One frame is the whole file, but which packet produces it is the
    // decoder's business — a format with a header packet answers the first
    // `send_packet` with nothing at all.
    let mut decoded = ffmpeg::frame::Video::empty();
    let mut have = false;
    for (stream, packet) in input.packets() {
        if stream.index() != index {
            continue;
        }
        decoder.send_packet(&packet)?;
        if decoder.receive_frame(&mut decoded).is_ok() {
            have = true;
            break;
        }
    }
    if !have {
        // Nothing came out while reading, which a decoder holding its only
        // frame back looks like. Draining is what asks for it.
        decoder.send_eof()?;
        decoder.receive_frame(&mut decoded)?;
    }

    let width = (decoded.width().max(2)) & !1;
    let height = (decoded.height().max(2)) & !1;
    let mut bgra = ffmpeg::frame::Video::empty();
    ffmpeg::software::scaling::Context::get(
        decoded.format(),
        decoded.width(),
        decoded.height(),
        ffmpeg::format::Pixel::BGRA,
        width,
        height,
        ffmpeg::software::scaling::Flags::BILINEAR,
    )?
    .run(&decoded, &mut bgra)?;

    // `MediaBuffer::Video` carries pooled frames; this one has no pool behind
    // it and never returns to one, which an unbound pool of zero expresses —
    // the same as a Color Source's single frame.
    let pool = UnboundObjectPool::new(0, ffmpeg::frame::Video::empty, |_| {});
    let mut slot = pool.get();
    *slot = bgra;
    Ok((MediaBuffer::Video(Arc::new(slot)), [width, height]))
}

/// What both implementations return, so the difference between them stays the
/// pipeline and nothing else.
fn opened(
    name: String,
    source: RunningSource,
    layer: Layer,
    pusher: media_pp::elements::AppSourceHandle,
    size: [u32; 2],
    path: &Path,
) -> OpenSource {
    OpenSource {
        media_file: None,
        source,
        layer,
        name,
        refreshed_token: None,
        showing: true,
        running: true,
        // Held rather than dropped: an `AppSource` runs only while a handle to
        // it exists, and letting this one go would end the layer with the one
        // frame it had just pushed.
        pushed: Some(PushedSurface {
            pusher,
            size,
            content: PushedContent::Image(path.to_path_buf()),
        }),
    }
}

#[cfg(target_os = "windows")]
pub(in crate::engine) fn open(
    device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
    handle: &media_pp::elements::D3d11VideoCompositorHandle,
    item: &SceneItemSnapshot,
    layer: media_pp::elements::VideoLayer,
) -> Result<Option<OpenSource>, BackendError> {
    use media_pp::elements::{AppSource, D3d11Upload, D3d11VideoCompositorInput};

    let Some(path) = settings(item)? else {
        return Ok(None);
    };
    let (frame, [width, height]) = decode(path)?;
    let name = input_name(item);
    let (source, pusher) = AppSource::new(name.clone(), 1);
    // BGRA in, BGRA composited: as with a Color Source there is no
    // colour-space conversion between the upload and the compositor.
    let upload = D3d11Upload::new(format!("{name}-upload"), device, width, height);

    let D3d11VideoCompositorInput { sink, layer } = handle
        .add_source(name.clone(), layer)?
        .ok_or("the compositor is no longer running")?;
    let pipeline = Pipeline::new(name.clone(), source, move |source, context| {
        let branch = context.branch().pipe(upload).to(sink)?;
        context.attach(source, 0, branch)?;
        Ok(())
    })?;
    pipeline.run()?;
    pusher.push(frame)?;

    Ok(Some(opened(
        name,
        RunningSource::Owned(pipeline),
        layer,
        pusher,
        [width, height],
        path,
    )))
}

#[cfg(target_os = "linux")]
pub(in crate::engine) fn open(
    device: &media_pp::elements::CudaDevice,
    handle: &media_pp::elements::CudaVideoCompositorHandle,
    item: &SceneItemSnapshot,
    layer: media_pp::elements::VideoLayer,
) -> Result<Option<OpenSource>, BackendError> {
    use media_pp::elements::{
        AppSource, CudaConverter, CudaFrameFormat, CudaUpload, CudaVideoCompositorInput,
    };

    let Some(path) = settings(item)? else {
        return Ok(None);
    };
    let (frame, [width, height]) = decode(path)?;
    let name = input_name(item);
    let (source, pusher) = AppSource::new(name.clone(), 1);
    // BGRA in, so `CudaConverter` performs the RGB-to-BT.709 conversion the
    // compositor expects rather than this having its own copy of that matrix.
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
    pusher.push(frame)?;

    Ok(Some(opened(
        name,
        RunningSource(pipeline),
        layer,
        pusher,
        [width, height],
        path,
    )))
}
