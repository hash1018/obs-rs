//! A Display Capture: one monitor, into the compositor.
//!
//! The one Source kind whose two implementations have nothing in common, so
//! each gets a file rather than a `#[cfg]` inside one.
//!
//! # Windows shares a capture, Linux cannot
//!
//! Desktop duplication opens one stream per display, and a display may be in
//! more than one SceneItem — so Windows keeps a `CaptureRegistry` and hands
//! each item a `Tee` branch off the capture it wants, which is why stopping
//! one item must leave the capture running for the others.
//!
//! The portal hands out a separate stream per request, so on Linux there is
//! nothing two SceneItems *have* to share and no registry at all: each owns
//! its own pipeline. That difference is what `RunningSource` is shaped
//! around, and why it is a type each backend defines for itself.

#[cfg_attr(target_os = "linux", path = "linux.rs")]
#[cfg_attr(target_os = "windows", path = "windows.rs")]
mod platform;

pub(in crate::engine) use platform::open;

#[cfg(target_os = "windows")]
pub(in crate::engine) use platform::CaptureRegistry;
