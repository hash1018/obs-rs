//! A Scene shown inside another Scene.
//!
//! Composited on its own and handed over as one picture — see the platform
//! half for what that buys and what it costs.

// Linux and macOS share one: both composite through `backend::Gpu`.
#[cfg_attr(target_os = "windows", path = "windows.rs")]
#[cfg_attr(any(target_os = "linux", target_os = "macos"), path = "gpu.rs")]
#[cfg_attr(
    not(any(target_os = "windows", target_os = "linux", target_os = "macos")),
    path = "absent.rs"
)]
mod platform;

pub(in crate::engine) use platform::*;
