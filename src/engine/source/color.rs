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

use media_pp::{buffer::MediaBuffer, ffmpeg};

use crate::domain::SourceSettings;
use crate::snapshots::SceneItemSnapshot;

use super::super::backend::BackendError;
use super::pushed::{self, Picture};
use super::{OpenSource, PushedContent, input_name};

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

    MediaBuffer::video(frame)
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

/// One flat colour, and the colour itself kept so that a filter change can
/// push the same picture again rather than redraw it.
fn picture(size: [u32; 2], rgba: [u8; 4]) -> Picture {
    Picture {
        size,
        content: PushedContent::Color(rgba),
        frame: flat_bgra(size[0], size[1], rgba),
    }
}

#[cfg(target_os = "windows")]
pub(in crate::engine) fn open(
    device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
    context: Arc<std::sync::Mutex<windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext>>,
    handle: &media_pp::elements::D3d11VideoCompositorHandle,
    item: &SceneItemSnapshot,
    layer: media_pp::elements::VideoLayer,
) -> Result<OpenSource, BackendError> {
    let (size, rgba) = size(item)?;
    let name = input_name(item);
    let wired = pushed::wire(&name, device, context, handle, item, layer)?;
    pushed::opened(name, wired, picture(size, rgba))
}

#[cfg(target_os = "linux")]
pub(in crate::engine) fn open(
    device: &Arc<media_pp::elements::CudaDevice>,
    handle: &media_pp::elements::CudaVideoCompositorHandle,
    item: &SceneItemSnapshot,
    layer: media_pp::elements::VideoLayer,
) -> Result<OpenSource, BackendError> {
    let (size, rgba) = size(item)?;
    let name = input_name(item);
    let wired = pushed::wire(&name, device, handle, item, layer)?;
    pushed::opened(name, wired, picture(size, rgba))
}
