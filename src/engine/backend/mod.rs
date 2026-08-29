//! The compositor, the capture Sources that feed it, and the frame it hands
//! to the Preview — one unit, chosen per platform.
//!
//! These three are not separable. What a capture element produces decides
//! which compositor can accept it, and what that compositor emits decides how
//! the frame reaches wgpu:
//!
//! ```text
//! CUDA    PipeWire open_gpu   → CudaConverter → CudaVideoCompositor  → shared buffer → NV12 resolve
//! D3D11   DxgiCaptureSource   → (no convert)  → D3d11VideoCompositor → shared texture
//! ```
//!
//! Wiring a D3D11 capture into a CUDA compositor is not merely slow, it is
//! rejected: `media-pp` compares memory domains when a branch is built, and no
//! element converts between the two. So a platform picks all three together or
//! none of them.
//!
//! Everything around this — reconciling Sources against the project snapshot,
//! layer geometry, when the Preview branch sleeps — is the same whichever
//! backend is in use, and lives in `super`.
//!
//! # Writing a backend
//!
//! Add a file here, point the `cfg_attr` below at it, and provide:
//!
//! - `Backend::start` — takes two rates and must not confuse them. `fps` is
//!   what the compositor is built for and what an output would be recorded
//!   at; `preview_fps` is only how often the frame reaching wgpu is refreshed.
//!   Build the compositor and the branch that publishes
//!   frames, and register one texture with egui. Call `on_frame` for *every*
//!   frame the compositor produced, passing the texture id only for the ones
//!   actually drawn into it: the rate of calls is the compositor's, which is
//!   what a recording would be made of, while the Preview is redrawn less
//!   often than that. The texture is registered once and overwritten;
//!   registering per frame takes the egui renderer's write lock every frame.
//! - `Backend::{pause, resume, stop}` — the Preview branch sleeps whenever no
//!   shown Source is running, so these are called often and must be cheap.
//! - `Backend::open_source` — start one SceneItem's Source and register its
//!   compositor input. Return [`OpenSource`].
//! - `Backend::remove_source` — drop a registration by name.
//! - `Layer` — runtime control for one registered input, with `set_layer` and
//!   `set_visible`. A platform whose handle already has both can alias it.
//! - `RunningSource` — `pause`, `resume`, and `stop` for one open Source.
//!   Not the pipeline itself, because one Source is not always one pipeline:
//!   desktop duplication refuses to open the same display twice on one
//!   device, so two SceneItems showing that display share one capture and
//!   this is each item's own share of it. Stopping one must leave the other
//!   running, and a shared capture may only pause once nothing shows it.
//!
//! The Preview branch must sit behind a dropping queue. A Preview that cannot
//! keep up has to drop frames rather than slow the compositor, which every
//! other branch will be built from.

use std::error::Error;
use std::sync::Arc;

use media_pp::color::Color;
use media_pp::elements::VideoCodec;

use crate::snapshots::SceneItemSnapshot;

#[cfg_attr(target_os = "linux", path = "cuda/mod.rs")]
#[cfg_attr(target_os = "windows", path = "d3d11/mod.rs")]
#[cfg_attr(
    not(any(target_os = "linux", target_os = "windows")),
    path = "unsupported.rs"
)]
mod platform;

pub(super) use platform::{Backend, Layer, PreparedRecording, RunningSource};

pub(super) type BackendError = Box<dyn Error + Send + Sync>;

/// What is behind every layer: the Canvas itself, where no Source covers it.
///
/// Part of what this module offers a backend rather than something every
/// backend must take, which is why it can be unused on one.
#[allow(dead_code)]
pub(super) const BACKGROUND: Color = Color::BLACK;

/// Which of `VideoCodec`'s H.264 entries a software choice maps to.
///
/// `Nvenc` never reaches here — it is not a software encoder and has no
/// `VideoCodec` at all — so it is folded into the one this crate would rather
/// have if it somehow did.
pub(super) fn software_codec(encoder: crate::settings::RecordingEncoder) -> VideoCodec {
    match encoder {
        crate::settings::RecordingEncoder::X264 => VideoCodec::H264,
        crate::settings::RecordingEncoder::OpenH264 | crate::settings::RecordingEncoder::Nvenc => {
            VideoCodec::OpenH264
        }
    }
}

/// The rate the encoder probe opens at.
///
/// Only the frame-rate metadata an encoder is configured with, and no encoder
/// refuses a size because of it — so this is a plausible number rather than a
/// meaningful one, and probing at the rate a recording would really use would
/// tell us nothing extra.
pub(super) const PROBE_FPS: u32 = 60;

/// Frames the recording branch may fall behind by before the compositor is
/// made to wait — at 60 fps, about an eighth of a second of slack for an
/// encoder that hiccups.
#[allow(dead_code)]
pub(super) const RECORDING_QUEUE_DEPTH: usize = 8;

/// How long the compositor waits for room in that queue before giving up on
/// a frame.
///
/// Deliberately far longer than any real backpressure: the queue above
/// absorbs an encoder that is merely behind, so reaching this at all means
/// one is genuinely stuck. Finite rather than unbounded because an unbounded
/// wait here would wedge the compositor, and with it the Preview and every
/// other branch. A timeout arrives on the bus as an error naming this
/// branch, which is what makes an overloaded encoder visible instead of
/// silent.
#[allow(dead_code)]
pub(super) const RECORDING_SEND_TIMEOUT: std::time::Duration =
    std::time::Duration::from_millis(500);

/// A Source that is running, and the controls for its layer.
/// The recording's video branch while one is running.
///
/// Platform-independent even though what feeds it is not: both backends end
/// the same way, at a `PauseGate` and a branch on their compositor's `Tee`.
pub(super) struct VideoTrack {
    pub(super) branch: media_pp::graph::BranchId,
    pub(super) pause: media_pp::elements::PauseGateHandle,
}

pub(super) struct OpenSource {
    pub(super) source: RunningSource,
    pub(super) layer: Layer,
    pub(super) name: String,
    /// The token the portal handed back, when it differs from the one it was
    /// given. `None` means the stored token is still current.
    pub(super) refreshed_token: Option<Option<String>>,
    /// Whether the Source is in the Scene being shown. One whose item left the
    /// Scene stays open but stops running, so coming back is a resume rather
    /// than another portal round trip.
    pub(super) showing: bool,
}

/// The name a SceneItem's compositor input is registered under.
#[allow(dead_code)]
pub(super) fn input_name(item: &SceneItemSnapshot) -> String {
    format!("scene-item-{}", item.id.0)
}

/// Convenience for a backend that has no Source of a given kind yet.
pub(super) fn unsupported_kind(item: &SceneItemSnapshot) -> BackendError {
    format!("{:?} is not connected to the compositor yet", item.kind).into()
}

/// One BGRA frame filled with a single colour, ready for a backend's upload
/// element. Backend-independent: both compositors take their Color Source
/// this way, differing only in which upload carries it to the GPU.
#[allow(dead_code)]
pub(super) fn flat_bgra(width: u32, height: u32, rgba: [u8; 4]) -> media_pp::buffer::MediaBuffer {
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
    MediaBuffer::Video(Arc::new(slot))
}
