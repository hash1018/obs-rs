//! A Window Capture: one application window, into the compositor.
//!
//! # Not found is not failure
//!
//! A window is closed and reopened as a matter of course — that is what
//! windows are for — so a target that is not on screen right now is an
//! ordinary state rather than an error. Opening one answers `Ok(None)` for
//! it, and the engine keeps the Source as [`SourceState::Missing`] and looks
//! again. A real failure — a handle that will not open, a device that will
//! not do BGRA — is still `Err`, and still terminal.
//!
//! [`SourceState::Missing`]: crate::engine::SourceState
//!
//! # What is resolved, and against what
//!
//! Windows stores the owning executable and the title, because an `HWND` is
//! meaningful only inside the session that issued it. Resolution is a search
//! of what is on screen for that pair — see
//! [`crate::domain::WindowCaptureTarget`] for why neither half alone would
//! do, and why this is a best match rather than a lookup.
//!
//! Wayland resolves nothing: its target is a portal token, and the portal's
//! own picker decides what the token reopens.

#[cfg_attr(target_os = "linux", path = "linux.rs")]
#[cfg_attr(target_os = "windows", path = "windows.rs")]
mod platform;

pub(in crate::engine) use platform::open;
