//! Windows window and monitor enumeration.
//!
//! `EnumWindows` is the easy part. The work is deciding what to leave out: it
//! reports every top-level window in the session, and most of them are not
//! things a person would recognise, let alone want to capture. Each filter
//! below is here because without it the list fills with entries the user
//! cannot identify.

use std::ffi::c_void;

use windows::Win32::{
    Foundation::{CloseHandle, HANDLE, HWND, LPARAM, MAX_PATH, RECT},
    Graphics::{
        Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute},
        Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW},
    },
    System::Threading::{
        GetCurrentProcessId, OpenProcess, PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
    },
    UI::WindowsAndMessaging::{
        GA_ROOT, GWL_EXSTYLE, GetAncestor, GetClassNameW, GetWindowLongPtrW, GetWindowRect,
        GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible,
        WS_EX_TOOLWINDOW,
    },
};
use windows::core as windows_core;

use media_pp::elements::{WasapiCaptureSource, WasapiDeviceKind};

use crate::domain::AudioSourceKind;

use super::{AudioDeviceTarget, MonitorRect, MonitorTarget, WindowTarget};

/// `MONITORINFOF_PRIMARY`. windows-rs 0.62 does not generate a binding for
/// it, so the documented value is spelled out here rather than guessed at
/// each use site.
const MONITORINFOF_PRIMARY: u32 = 1;

/// Window classes that belong to the shell itself. They are visible, titled,
/// and top-level, so nothing else here excludes them.
const SHELL_CLASSES: &[&str] = &[
    "Progman",       // the desktop
    "WorkerW",       // the desktop's wallpaper host
    "Shell_TrayWnd", // the task bar
    "Shell_SecondaryTrayWnd",
    "Windows.UI.Core.CoreWindow", // start menu, search, action centre
];

impl WindowTarget {
    /// The handle in the form `media-pp`'s `WgcCaptureSource::open` takes.
    pub fn hwnd(&self) -> HWND {
        HWND(self.handle as *mut c_void)
    }
}

/// Every top-level window a user would plausibly recognise, in the order
/// Windows reports them, which is roughly front-to-back z-order.
pub fn windows() -> Vec<WindowTarget> {
    let mut found: Vec<WindowTarget> = Vec::new();
    // SAFETY: `enum_window` matches the documented callback signature, and
    // `found` outlives the enumeration — `EnumWindows` is synchronous and
    // retains nothing after it returns.
    let _ = unsafe {
        windows::Win32::UI::WindowsAndMessaging::EnumWindows(
            Some(enum_window),
            LPARAM(&mut found as *mut Vec<WindowTarget> as isize),
        )
    };
    found
}

unsafe extern "system" fn enum_window(hwnd: HWND, lparam: LPARAM) -> windows_core::BOOL {
    // SAFETY: `lparam` is the `&mut Vec` this enumeration was started with,
    // valid for the whole synchronous call.
    let found = unsafe { &mut *(lparam.0 as *mut Vec<WindowTarget>) };
    if let Some(target) = describe(hwnd) {
        found.push(target);
    }
    windows_core::BOOL(1)
}

/// `None` for anything the user should not be offered.
fn describe(hwnd: HWND) -> Option<WindowTarget> {
    // SAFETY: every call below reads the by-value handle and, where an
    // out-parameter is used, a live local. None retain caller storage, and a
    // window that dies mid-enumeration simply reports nothing useful.
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() {
            return None;
        }
        // Only true top-level windows. An owned dialog or tool palette
        // reports its owner here, and capturing it separately is not what a
        // user picking "the app" means.
        if GetAncestor(hwnd, GA_ROOT) != hwnd {
            return None;
        }
        // A tool window is chrome — floating palettes, IME candidates. It is
        // deliberately absent from Alt-Tab for the same reason.
        let ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        if ex_style & WS_EX_TOOLWINDOW.0 != 0 {
            return None;
        }
        // The one that is easy to miss and ruins the list: a UWP app that has
        // been closed leaves its window alive but *cloaked*, still visible and
        // still titled as far as every check above is concerned. Without this
        // the list fills with ghosts of Settings, Photos, and Store windows
        // the user closed hours ago.
        if is_cloaked(hwnd) {
            return None;
        }
        if SHELL_CLASSES.contains(&class_name(hwnd).as_str()) {
            return None;
        }

        let title = window_title(hwnd);
        // Untitled windows exist in quantity and cannot be told apart.
        if title.is_empty() {
            return None;
        }

        let mut rect = RECT::default();
        GetWindowRect(hwnd, &mut rect).ok()?;
        let width = (rect.right - rect.left).max(0) as u32;
        let height = (rect.bottom - rect.top).max(0) as u32;
        if width == 0 || height == 0 {
            return None;
        }

        let mut pid = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        // Capturing our own preview inside our own preview is a feedback
        // tunnel, and it is never what was meant.
        if pid == GetCurrentProcessId() {
            return None;
        }

        Some(WindowTarget {
            handle: hwnd.0 as isize,
            title,
            process: process_name(pid).unwrap_or_default(),
            size: (width, height),
        })
    }
}

/// Whether DWM considers this window cloaked — present but not composited.
fn is_cloaked(hwnd: HWND) -> bool {
    let mut cloaked = 0u32;
    // SAFETY: `DWMWA_CLOAKED` writes exactly one `u32`, into a live local.
    // A failure means DWM has no opinion, which is not evidence of cloaking.
    let result = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            (&raw mut cloaked).cast::<c_void>(),
            size_of::<u32>() as u32,
        )
    };
    result.is_ok() && cloaked != 0
}

fn window_title(hwnd: HWND) -> String {
    // SAFETY: the length query and the read use the same live handle, and the
    // buffer is sized from that length plus the terminator.
    unsafe {
        let length = GetWindowTextLengthW(hwnd);
        if length <= 0 {
            return String::new();
        }
        let mut buffer = vec![0u16; length as usize + 1];
        let written = GetWindowTextW(hwnd, &mut buffer);
        String::from_utf16_lossy(&buffer[..written.max(0) as usize])
    }
}

fn class_name(hwnd: HWND) -> String {
    let mut buffer = [0u16; 256];
    // SAFETY: writes at most `buffer.len()` UTF-16 units into a live local.
    let written = unsafe { GetClassNameW(hwnd, &mut buffer) };
    String::from_utf16_lossy(&buffer[..written.max(0) as usize])
}

/// The owning executable's file name, e.g. `chrome.exe`.
///
/// `None` when the process refuses to be opened — an elevated or protected
/// one does, and its windows are still perfectly capturable, so this is a
/// missing label rather than a reason to drop the entry.
fn process_name(pid: u32) -> Option<String> {
    // SAFETY: opens one query-only reference to `pid`; the handle is closed
    // exactly once below.
    let process: HANDLE =
        unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut buffer = [0u16; MAX_PATH as usize];
    let mut length = buffer.len() as u32;
    // SAFETY: `process` is live, and `length` is the buffer's capacity going
    // in and the written count coming out.
    let result = unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_FORMAT(0),
            windows::core::PWSTR(buffer.as_mut_ptr()),
            &mut length,
        )
    };
    // SAFETY: `process` came from the successful `OpenProcess` above and is
    // not used afterwards.
    let _ = unsafe { CloseHandle(process) };
    result.ok()?;

    let path = String::from_utf16_lossy(&buffer[..length as usize]);
    Some(path.rsplit(['\\', '/']).next().unwrap_or(&path).to_string())
}

/// Every display, in the order Windows reports them.
pub fn monitors() -> Vec<MonitorTarget> {
    let mut found: Vec<MonitorTarget> = Vec::new();
    // SAFETY: `enum_monitor` matches the documented callback signature and
    // `found` outlives this synchronous enumeration.
    let _ = unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(enum_monitor),
            LPARAM(&mut found as *mut Vec<MonitorTarget> as isize),
        )
    };
    found
}

unsafe extern "system" fn enum_monitor(
    monitor: HMONITOR,
    _hdc: HDC,
    _rect: *mut RECT,
    lparam: LPARAM,
) -> windows_core::BOOL {
    // SAFETY: `lparam` is the `&mut Vec` this enumeration was started with.
    let found = unsafe { &mut *(lparam.0 as *mut Vec<MonitorTarget>) };

    let mut info = MONITORINFOEXW {
        monitorInfo: MONITORINFO {
            cbSize: size_of::<MONITORINFOEXW>() as u32,
            ..Default::default()
        },
        ..Default::default()
    };
    // SAFETY: `monitor` is live for this callback and `info` is a correctly
    // sized `MONITORINFOEXW`, which `GetMonitorInfoW` accepts through its
    // `MONITORINFO` prefix — that is what `cbSize` distinguishes.
    let ok = unsafe { GetMonitorInfoW(monitor, (&raw mut info).cast::<MONITORINFO>()) };
    if ok.as_bool() {
        let rect = info.monitorInfo.rcMonitor;
        let name_end = info
            .szDevice
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(info.szDevice.len());
        found.push(MonitorTarget {
            name: String::from_utf16_lossy(&info.szDevice[..name_end]),
            rect: MonitorRect {
                x: rect.left,
                y: rect.top,
                width: (rect.right - rect.left).max(0) as u32,
                height: (rect.bottom - rect.top).max(0) as u32,
            },
            is_primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
        });
    }
    windows_core::BOOL(1)
}

/// Every active WASAPI endpoint, both playback and recording.
///
/// The endpoint id is what gets stored: it is opaque, and it is stable across
/// restarts and replugs, which is what a saved choice needs.
///
/// An enumeration that fails is an empty list rather than an error. The
/// picker's own "default device" entry still works without one — it is the
/// absence of a choice, not one of the entries — so a caller has something to
/// show either way.
pub fn audio_devices() -> Vec<AudioDeviceTarget> {
    let devices = match WasapiCaptureSource::list_devices() {
        Ok(devices) => devices,
        Err(error) => {
            eprintln!("could not list audio devices: {error}");
            return Vec::new();
        }
    };
    devices
        .into_iter()
        .map(|device| AudioDeviceTarget {
            id: device.id,
            name: device.name,
            kind: match device.kind {
                WasapiDeviceKind::Render => AudioSourceKind::Output,
                WasapiDeviceKind::Capture => AudioSourceKind::Input,
            },
            is_default: device.is_default,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// There is always at least one display, and every field has to be
    /// filled in — an entry a list cannot label is not usable.
    #[test]
    fn monitors_are_enumerated_and_described() {
        let monitors = monitors();
        assert!(!monitors.is_empty(), "a session always has a display");
        assert_eq!(
            monitors.iter().filter(|m| m.is_primary).count(),
            1,
            "exactly one display is primary"
        );
        for monitor in &monitors {
            assert!(monitor.name.starts_with(r"\\"), "{monitor:?}");
            assert!(
                monitor.rect.width > 0 && monitor.rect.height > 0,
                "{monitor:?}"
            );
        }
    }

    /// The filters have to leave something behind, and everything they leave
    /// has to be presentable: a titled, non-empty, identifiable window.
    #[test]
    fn enumerated_windows_are_all_presentable() {
        let windows = windows();
        for window in &windows {
            assert!(!window.title.is_empty(), "{window:?}");
            assert_ne!(window.handle, 0, "{window:?}");
            assert!(window.size.0 > 0 && window.size.1 > 0, "{window:?}");
        }
        // The test runner itself is a console window, so a desktop session
        // always has at least one thing to capture.
        assert!(
            !windows.is_empty(),
            "an interactive session has capturable windows"
        );
    }

    /// obs-rs must never offer its own windows: capturing them feeds the
    /// preview back into itself.
    #[test]
    fn own_windows_are_never_offered() {
        // SAFETY: reads this process's own id and retains nothing.
        let own = unsafe { GetCurrentProcessId() };
        for window in windows() {
            let mut pid = 0;
            // SAFETY: the handle came from enumeration; a window that died
            // since then leaves `pid` zero, which cannot match `own`.
            unsafe { GetWindowThreadProcessId(window.hwnd(), Some(&mut pid)) };
            assert_ne!(pid, own, "offered its own window: {window:?}");
        }
    }
}
