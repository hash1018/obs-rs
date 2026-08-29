//! What the user can pick as a capture source, and how they pick it.
//!
//! The two halves are deliberately separate. `media-pp` captures a target the
//! application hands it — `WgcCaptureSource` takes an `HWND` and explicitly
//! does not show a picker — so choosing one is this crate's job.
//!
//! How that choice is made is not the same on every platform, and it is not a
//! detail that can be hidden behind one list-shaped API:
//!
//! - **Windows** lets a process enumerate windows and monitors, so obs-rs
//!   builds the list and draws it.
//! - **Linux/X11** permits enumeration through EWMH and XRandR. **Wayland**
//!   does not: `xdg-desktop-portal` shows *its own* picker and hands back only
//!   what the user chose. That is the security model, not a missing API.
//! - **macOS** does return a list (`SCShareableContent`), but only after the
//!   user has granted screen-recording permission, so the list can be empty
//!   for a reason that is not "nothing to capture".
//!
//! [`SourcePicker`] is that fork, named once here so the UI can branch on it
//! instead of a Windows-shaped list leaking into the rest of the app.
//!
//! Audio is the easy case and lives here too — see [`audio_devices`]. It has
//! no portal and no permission prompt: every platform answers with a list,
//! and both read it through `media-pp`, which already enumerates each backend
//! for its own capture sources.
//!
//! Being *told* the list changed does fork, though — see
//! [`watch_audio_devices`]. Windows raises endpoint notifications a process
//! can subscribe to, so it does; PipeWire's equivalent would mean a second
//! connection and loop of this crate's own, so Linux re-enumerates on a timer
//! instead. Both answer the same question, and neither is `media-pp`'s: it
//! captures the endpoint it is handed and has no opinion on which exist.

// Display-target enumeration is wired into the Sources dock. Window targets
// are retained for the upcoming Window Capture picker, so part of this shared
// platform API is still intentionally unused.
#![allow(dead_code)]

use crate::domain::AudioSourceKind;

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "windows")]
pub mod windows;

/// How this platform lets the user choose a capture source.
pub enum SourcePicker {
    /// This process may enumerate targets and present them itself.
    Enumerated {
        windows: Vec<WindowTarget>,
        monitors: Vec<MonitorTarget>,
    },
    /// The system shows its own picker and returns the selection. There is no
    /// list to draw, and asking for one is the wrong shape of request.
    SystemDialog,
}

/// One capturable top-level window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowTarget {
    /// The raw platform window identifier, as an integer so this type stays
    /// `Send`. It is an `HWND` on Windows and an X11 window ID on Linux/X11.
    pub handle: isize,
    /// The window's own title, as the user sees it in the task bar.
    pub title: String,
    /// The owning executable's file name — the only thing that tells two
    /// identically titled windows apart in a list.
    pub process: String,
    /// Current outer size in pixels. Shown to help pick between windows;
    /// it is not what the capture will be, since the window can be resized.
    pub size: (u32, u32),
}

/// One capturable display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorTarget {
    /// The stable display name, e.g. `\\.\DISPLAY1` or `DP-1`.
    pub name: String,
    /// Position and size in the virtual desktop's coordinates. On Windows it
    /// is the same space `DxgiCaptureOptions::area` uses; on X11 it is the
    /// XRandR root-window coordinate space.
    pub rect: MonitorRect,
    /// Whether this is the primary display.
    pub is_primary: bool,
}

/// One audio endpoint the user can pick for a mixer source.
///
/// Unlike a capture target, this really is a list on every platform: audio
/// needs no portal and no permission prompt, so both backends answer
/// immediately and without showing anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioDeviceTarget {
    /// What gets stored, and what is handed back to `media-pp` to open this
    /// endpoint again.
    ///
    /// Not the same field on both platforms, deliberately. Windows' endpoint
    /// id is opaque and stable, so that is what this holds. PipeWire's node
    /// id is only valid while the node is, and survives neither a replug nor
    /// a restart — so on Linux this is the node *name*, which does. Nothing
    /// above here has to know which, as long as nothing above here tries to
    /// interpret it.
    pub id: String,
    /// What the user reads in the picker.
    pub name: String,
    /// Which side of the sound card this is, matching the mixer source it can
    /// be chosen for: an [`AudioSourceKind::Output`] source lists playback
    /// endpoints, captured by listening to what they play.
    pub kind: AudioSourceKind,
    /// Whether this was the system's default for its own kind when the list
    /// was taken. Shown in the picker; it is not what "no device" means — a
    /// source with no device follows the default as it changes, rather than
    /// being pinned to whichever one this was.
    pub is_default: bool,
}

/// Every audio endpoint this platform can capture from, or an empty list on
/// one that has no backend.
///
/// Reads through `media-pp`, which already enumerates both WASAPI endpoints
/// and PipeWire nodes for its own capture sources. That is a dependency the
/// screen-capture half of this module deliberately does not have — see this
/// module's own docs — and the reason does not apply here: this is a static
/// call that shows nothing and needs no pipeline.
pub fn audio_devices() -> Vec<AudioDeviceTarget> {
    #[cfg(target_os = "windows")]
    {
        windows::audio_devices()
    }
    #[cfg(target_os = "linux")]
    {
        linux::audio_devices()
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    {
        Vec::new()
    }
}

/// Calls `on_change` whenever the set of audio endpoints is not what it was —
/// one plugged in, one gone, or a different one now default.
///
/// A wake-up rather than an event: it says to look again, not what changed.
/// It can arrive several times for what a person did once, and it arrives on
/// a thread this crate does not own, so the callback must be cheap and safe
/// to run twice.
///
/// `None` means this platform will not report changes and the caller sees
/// whatever was there when it last enumerated. The reason is reported where
/// it is known.
///
/// Watching stops when the returned value is dropped, and it has to be held
/// for exactly as long as the callback should keep firing.
pub fn watch_audio_devices(
    on_change: impl Fn() + Send + Sync + 'static,
) -> Option<AudioDeviceWatch> {
    #[cfg(target_os = "windows")]
    {
        windows::watch_audio_devices(on_change)
            .inspect_err(|error| eprintln!("could not watch audio devices: {error}"))
            .ok()
    }
    #[cfg(target_os = "linux")]
    {
        linux::watch_audio_devices(on_change)
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    {
        let _ = on_change;
        None
    }
}

/// What holds an endpoint watch open. Dropping it stops the notifications.
///
/// Each platform's own type, aliased rather than wrapped: the two have
/// nothing in common but their lifetime, and there is nothing to ask them
/// once they exist.
#[cfg(target_os = "windows")]
pub type AudioDeviceWatch = windows::AudioDeviceWatch;
#[cfg(target_os = "linux")]
pub type AudioDeviceWatch = linux::AudioDeviceWatch;
#[cfg(not(any(target_os = "windows", target_os = "linux")))]
pub struct AudioDeviceWatch;

/// A monitor's place in the virtual desktop. Signed origin: a display left of
/// or above the primary one has negative coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MonitorRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// Enumerates what this platform allows, or reports that it allows nothing.
///
/// Returning [`SourcePicker::SystemDialog`] is not a failure — it is the
/// answer on platforms where the system owns the picker.
pub fn source_picker() -> SourcePicker {
    #[cfg(target_os = "windows")]
    {
        SourcePicker::Enumerated {
            windows: windows::windows(),
            monitors: windows::monitors(),
        }
    }
    #[cfg(target_os = "linux")]
    {
        linux::source_picker()
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    {
        SourcePicker::SystemDialog
    }
}
