//! The claim as a Windows share mode, and the window that holds it.
//!
//! No lock call at all: opening a file already says who else may open it, and
//! a handle opened for writing while permitting only readers is refused to
//! every later writer. That refusal *is* the claim, it is enforced by the
//! file system rather than by agreement between processes, and it ends when
//! the handle closes — which Windows does for a process however it ended.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::fs::OpenOptionsExt;
use std::path::Path;

use windows::Win32::Foundation::{HWND, LPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GA_ROOT, GetAncestor, GetWindowTextLengthW, GetWindowThreadProcessId,
    IsWindowVisible, SW_RESTORE, SetForegroundWindow, ShowWindow,
};
use windows::core as windows_core;

/// `FILE_SHARE_READ`. Readers are let in on purpose: the process id in the
/// file is there to be read by the launch this one is about to turn away.
const SHARE_READ: u32 = 1;

/// `ERROR_SHARING_VIOLATION`, which is a claim already held rather than a
/// fault — every other error is a fault and is reported as one.
const SHARING_VIOLATION: i32 = 32;

pub(super) fn hold(path: &Path) -> io::Result<Option<File>> {
    let opened = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .share_mode(SHARE_READ)
        .open(path);
    match opened {
        Ok(file) => Ok(Some(file)),
        Err(error) if error.raw_os_error() == Some(SHARING_VIOLATION) => Ok(None),
        Err(error) => Err(error),
    }
}

/// Brings `pid`'s window forward, restoring it if it was minimised.
///
/// The foreground is not something a process may simply take, but the one
/// asking here was just launched by the user, and that is precisely the case
/// Windows grants it for.
pub(super) fn raise(pid: u32) -> bool {
    let mut search = Search { pid, found: None };
    // SAFETY: `enum_window` matches the documented callback signature, and
    // `search` outlives the enumeration — `EnumWindows` is synchronous and
    // retains nothing after it returns.
    let _ = unsafe {
        EnumWindows(
            Some(enum_window),
            LPARAM(&mut search as *mut Search as isize),
        )
    };
    let Some(window) = search.found else {
        return false;
    };
    // SAFETY: `window` came from this enumeration and is only used here.
    unsafe {
        // A window brought forward while minimised is still not one anybody
        // can see, so it is restored first.
        let _ = ShowWindow(window, SW_RESTORE);
        SetForegroundWindow(window).as_bool()
    }
}

struct Search {
    pid: u32,
    found: Option<HWND>,
}

unsafe extern "system" fn enum_window(hwnd: HWND, lparam: LPARAM) -> windows_core::BOOL {
    // SAFETY: `lparam` is the `&mut Search` this enumeration was started
    // with, valid for the whole synchronous call.
    let search = unsafe { &mut *(lparam.0 as *mut Search) };
    let mut owner = 0;
    // SAFETY: `hwnd` is the handle the enumeration just handed over, and
    // `owner` is a live local.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut owner)) };
    if owner == search.pid && is_the_window(hwnd) {
        search.found = Some(hwnd);
        return false.into();
    }
    true.into()
}

/// Whether `hwnd` is the window a person would say *is* the application.
///
/// Owning one window is not what a process of this shape does: measured on a
/// running obs-rs, the enumeration reports five for it — a 0×0 one, a 16×16
/// one, two hidden, and the actual window. Taking the first visible one
/// restored a zero-sized window and left the real one minimised, which looks
/// exactly like the second launch having done nothing.
///
/// A title is what separates them. The helpers carry none, because nothing
/// was ever meant to see them; a window with a caption is one that has a
/// place in the task bar. `GA_ROOT` then drops anything a dialog would own,
/// and `IsWindowVisible` the ones deliberately hidden — a minimised window
/// still counts as visible, which is the case this exists to fix.
fn is_the_window(hwnd: HWND) -> bool {
    // SAFETY: `hwnd` is live for the enumeration this is called from, and
    // each of these only reads from it.
    unsafe {
        IsWindowVisible(hwnd).as_bool()
            && GetWindowTextLengthW(hwnd) > 0
            && GetAncestor(hwnd, GA_ROOT) == hwnd
    }
}
