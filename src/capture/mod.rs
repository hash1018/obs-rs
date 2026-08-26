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
//! - **Linux** does not. `xdg-desktop-portal` shows *its own* picker and hands
//!   back only what the user chose; an application neither sees the list nor
//!   is allowed to. That is the security model, not a missing API.
//! - **macOS** does return a list (`SCShareableContent`), but only after the
//!   user has granted screen-recording permission, so the list can be empty
//!   for a reason that is not "nothing to capture".
//!
//! [`SourcePicker`] is that fork, named once here so the UI can branch on it
//! instead of a Windows-shaped list leaking into the rest of the app.

// Nothing draws a source list yet — the UI layout is still being decided —
// so every public item here is currently unused outside its own tests.
// Remove this once the picker is wired into the UI.
#![allow(dead_code)]

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
    /// The raw window handle, as an integer so this type stays `Send` and
    /// platform-agnostic to name. On Windows this is what
    /// `WgcCaptureSource::open` wants; see [`windows::WindowTarget`] helpers.
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
    /// The device name, e.g. `\\.\DISPLAY1`.
    pub name: String,
    /// Position and size in the virtual desktop's coordinates — the same
    /// space `DxgiCaptureOptions::area` is expressed in, so this can be
    /// handed over as a capture region unchanged.
    pub rect: MonitorRect,
    /// Whether this is the primary display.
    pub is_primary: bool,
}

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
    #[cfg(not(target_os = "windows"))]
    {
        SourcePicker::SystemDialog
    }
}
