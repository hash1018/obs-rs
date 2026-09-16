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
//! it, and on Windows they are this same executable started again with
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
//! Windows, with the `browser` feature — the same pairing the D3D11 backend
//! has, and for the same reason: an off-screen page is handed over as a
//! shared GPU texture, and importing one is a per-backend operation. Anywhere
//! else this module still compiles, as `absent.rs`, and answers that there is
//! no browser engine here.

#[cfg_attr(all(target_os = "windows", feature = "browser"), path = "cef.rs")]
#[cfg_attr(
    not(all(target_os = "windows", feature = "browser")),
    path = "absent.rs"
)]
mod engine;

pub use engine::{Runtime, helper_process};
