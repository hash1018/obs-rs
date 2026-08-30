//! Getting a composited frame from the compositor to egui.
//!
//! The compositor writes into GPU memory this side does not own, and egui
//! draws from a texture wgpu does own. What is between them is the largest
//! single thing in the engine, and it is entirely different on each platform:
//! Windows shares one D3D11 texture and hands wgpu a view of it, while Linux
//! copies NV12 planes through Vulkan external memory and resolves them into
//! RGBA with a render pass.
//!
//! Neither half shares a line with the other, which is why each gets a file
//! rather than a `#[cfg]` inside one. What they do share is their shape: a
//! `PreviewSurface` both the pipeline's renderer and the `Backend` hold, and
//! a `PreviewRenderer` `media-pp` drives at the Preview's own rate.
//!
//! # Nobody looking
//!
//! Both carry a `visible` flag, because a minimised window is nobody and the
//! work here — a copy, a resolve pass, a texture write — is then done for a
//! picture no one samples. The frame that arrives while nobody is looking is
//! kept so the window has something to show the moment it comes back: a
//! Scene that is not changing sends nothing at all, so waiting for the next
//! frame could mean waiting indefinitely.

#[cfg_attr(target_os = "linux", path = "linux.rs")]
#[cfg_attr(target_os = "windows", path = "windows.rs")]
mod platform;

#[cfg(target_os = "linux")]
mod nv12;

#[cfg(target_os = "linux")]
pub(in crate::engine) use nv12::Nv12Target;
#[cfg(target_os = "linux")]
pub(in crate::engine) use platform::SharedNv12;
#[cfg(target_os = "windows")]
pub(in crate::engine) use platform::SharedTarget;
pub(in crate::engine) use platform::{PreviewRenderer, PreviewSurface};

/// One composited frame, already resident on the GPU.
///
/// The `TextureId` stays valid for the life of the engine: the texture is
/// created and registered once, and each frame overwrites its contents.
/// Registering per frame would take the egui renderer's write lock every
/// frame and stall the very thread this exists to keep free.
pub struct CompositeFrame {
    pub texture_id: eframe::egui::TextureId,
}
