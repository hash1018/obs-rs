//! The embedded browser engine a Browser Source draws with.
//!
//! A browser is not a capture and not a file: it is another rendering engine
//! living inside this process, with its own threads, its own child processes,
//! and a lifetime that has to start before the window and end after it. That
//! is what this module owns — [`helper_process`] and [`Runtime`] — so the
//! rest of the application can treat a page as one more Source.
//!
//! # The shape of it
//!
//! Chromium — which is what CEF is — runs as several processes. The browser
//! process is this one; the render, GPU and utility processes are launched by
//! it, and on Windows and Linux alike they are this same executable started again with
//! `--type=` arguments. So the very first thing `main` does is ask
//! [`helper_process`] whether this process is one of those, and if it is,
//! that call runs the child's whole job and hands back its exit code. Nothing
//! else may happen first: a helper that claimed the single-instance lock,
//! opened the log, or opened a window would be a second obs-rs.
//!
//! The browser process then keeps one [`Runtime`] alive for as long as it
//! runs. It owns a thread of its own that initializes CEF and turns its
//! message pump, which is where every browser callback arrives. Dropping it
//! stops that thread and shuts CEF down.
//!
//! # Where it exists
//!
//! Windows and Linux, with the `browser` feature. The two hand a picture over
//! differently — a shared GPU texture on Windows, pixels on Linux; see
//! `cef.rs` — because importing a GPU buffer is a per-backend operation and
//! the Linux one has no bridge for what Chromium hands over there yet.
//! Anywhere else this module still compiles, as `absent.rs`, and answers
//! that there is no browser engine here.

#[cfg_attr(
    all(any(target_os = "windows", target_os = "linux"), feature = "browser"),
    path = "cef.rs"
)]
#[cfg_attr(
    not(all(any(target_os = "windows", target_os = "linux"), feature = "browser")),
    path = "absent.rs"
)]
mod engine;

pub use engine::{
    AUDIO_CHANNELS, AUDIO_RATE, Heard, Held, NamedKey, OnAudio, Page, PageInput, PageOptions,
    Pressed, Runtime, helper_process, open_page,
};
// Named only by the Linux Source, whose paint callback has to spell out the
// picture's lifetime; the Windows one never names it, and an export nothing
// reads is a warning there.
#[cfg(target_os = "linux")]
pub use engine::Painted;
