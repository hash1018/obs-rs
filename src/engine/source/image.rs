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
#[cfg(target_os = "linux")]
use std::sync::Arc;

use media_pp::{buffer::MediaBuffer, ffmpeg};

use crate::domain::SourceSettings;
use crate::snapshots::SceneItemSnapshot;

use super::super::backend::BackendError;
use super::pushed::{self, Picture};
use super::{OpenOutcome, PushedContent, input_name};

/// The file this item names, or why it cannot be read right now.
fn settings(item: &SceneItemSnapshot) -> Result<Result<&Path, String>, BackendError> {
    let SourceSettings::Image(settings) = &item.settings else {
        return Err("scene item is not an image source".into());
    };
    Ok(super::present_file(&settings.path).map(|()| settings.path.as_path()))
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

    Ok((MediaBuffer::video(bgra), [width, height]))
}

/// The decoded file, and the path it came from kept so that a filter change
/// can push the same picture again rather than decode it a second time.
fn picture(frame: MediaBuffer, size: [u32; 2], path: &Path) -> Picture {
    Picture {
        size,
        content: PushedContent::Image(path.to_path_buf()),
        frame,
    }
}

#[cfg(target_os = "windows")]
pub(in crate::engine) fn open(
    gpu: &media_pp::elements::D3d11Gpu,
    handle: &media_pp::elements::D3d11VideoCompositorHandle,
    item: &SceneItemSnapshot,
    layer: media_pp::elements::VideoLayer,
) -> Result<OpenOutcome, BackendError> {
    let path = match settings(item)? {
        Ok(path) => path,
        Err(absent) => return Ok(OpenOutcome::Absent(absent)),
    };
    let (frame, size) = decode(path)?;
    let name = input_name(item);
    let wired = pushed::wire(&name, gpu, handle, item, layer)?;
    pushed::opened(name, wired, picture(frame, size, path)).map(OpenOutcome::Open)
}

#[cfg(target_os = "linux")]
pub(in crate::engine) fn open(
    device: &Arc<media_pp::elements::CudaDevice>,
    handle: &media_pp::elements::CudaVideoCompositorHandle,
    item: &SceneItemSnapshot,
    layer: media_pp::elements::VideoLayer,
) -> Result<OpenOutcome, BackendError> {
    let path = match settings(item)? {
        Ok(path) => path,
        Err(absent) => return Ok(OpenOutcome::Absent(absent)),
    };
    let (frame, size) = decode(path)?;
    let name = input_name(item);
    let wired = pushed::wire(&name, device, handle, item, layer)?;
    pushed::opened(name, wired, picture(frame, size, path)).map(OpenOutcome::Open)
}
