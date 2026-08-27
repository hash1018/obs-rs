//! The compositor, the capture Sources that feed it, and the frame it hands
//! to the Preview — one unit, chosen per platform.
//!
//! These three are not separable. What a capture element produces decides
//! which compositor can accept it, and what that compositor emits decides how
//! the frame reaches wgpu:
//!
//! ```text
//! CUDA    PipeWire open_gpu   → CudaConverter → CudaVideoCompositor  → download → NV12 resolve
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
//! - `Backend::start` — build the compositor and the branch that publishes
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
//!
//! The Preview branch must sit behind a dropping queue. A Preview that cannot
//! keep up has to drop frames rather than slow the compositor, which every
//! other branch will be built from.

use std::error::Error;
use std::sync::Arc;

use media_pp::{color::Color, pipeline::Pipeline};

use crate::snapshots::SceneItemSnapshot;

#[cfg_attr(target_os = "linux", path = "cuda/mod.rs")]
#[cfg_attr(not(target_os = "linux"), path = "unsupported.rs")]
mod platform;

pub(super) use platform::{Backend, Layer};

pub(super) type BackendError = Box<dyn Error + Send + Sync>;

/// What is behind every layer: the Canvas itself, where no Source covers it.
///
/// Part of what this module offers a backend rather than something every
/// backend must take, which is why it can be unused on one.
#[allow(dead_code)]
pub(super) const BACKGROUND: Color = Color::BLACK;

/// A Source that is running, and the controls for its layer.
pub(super) struct OpenSource {
    pub(super) pipeline: Arc<Pipeline>,
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
