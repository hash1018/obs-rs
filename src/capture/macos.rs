//! What can be captured on macOS, listed through Core Graphics.
//!
//! macOS lets a process enumerate its displays and windows, so obs-rs builds
//! the list and draws it, as on Windows. Not through `media-pp`: its
//! ScreenCaptureKit listing asks for screen-recording permission, and a
//! picker is not where that question belongs. Core Graphics answers without
//! asking — with every window's owner and size, and its title only once the
//! permission has been granted, which is why a title can be empty here.
//!
//! A window's number is the same `CGWindowID` ScreenCaptureKit captures by,
//! and a display's is the `CGDirectDisplayID` it captures by, so what is
//! listed here is what the captures open.
//!
//! Cameras are read through `media-pp`, as on the other platforms.

use objc2_core_foundation::{
    CFArray, CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType, CGRect,
};
use objc2_core_graphics::{
    CGDirectDisplayID, CGDisplayBounds, CGDisplayIsBuiltin, CGGetActiveDisplayList,
    CGMainDisplayID, CGRectMakeWithDictionaryRepresentation, CGWindowListCopyWindowInfo,
    CGWindowListOption, kCGNullWindowID, kCGWindowBounds, kCGWindowIsOnscreen, kCGWindowLayer,
    kCGWindowName, kCGWindowNumber, kCGWindowOwnerName, kCGWindowOwnerPID,
};

use super::{MonitorRect, MonitorTarget, VideoCaptureTarget, WindowTarget};

/// More displays than a Mac can drive at once.
const MAX_DISPLAYS: u32 = 32;

/// A window smaller than this in either direction is some application's
/// helper — a status item's panel, a tooltip — rather than something anyone
/// means to capture.
const SMALLEST_WINDOW: u32 = 40;

/// Every active display, the main one marked.
///
/// Named for what it is and its display id — `Built-in Display (1)`,
/// `Display (722475459)` — since a Mac gives a display no stable name of its
/// own that Core Graphics can read, and the id is what a Display Capture
/// finds it by again: see [`display_id`].
///
/// The rectangle is in points, in the global space whose origin is the main
/// display's top-left corner — the space a window is placed in.
pub fn monitors() -> Vec<MonitorTarget> {
    let mut ids = [0 as CGDirectDisplayID; MAX_DISPLAYS as usize];
    let mut count = 0u32;
    // SAFETY: `ids` holds `MAX_DISPLAYS` entries and `count` is a live out
    // parameter, which is the whole of what the call writes.
    let error = unsafe { CGGetActiveDisplayList(MAX_DISPLAYS, ids.as_mut_ptr(), &mut count) };
    if error.0 != 0 {
        tracing::warn!("could not list the displays (CGError {})", error.0);
        return Vec::new();
    }
    let main = CGMainDisplayID();
    ids[..count as usize]
        .iter()
        .map(|&id| {
            let bounds = CGDisplayBounds(id);
            let kind = if CGDisplayIsBuiltin(id) {
                "Built-in Display"
            } else {
                "Display"
            };
            MonitorTarget {
                name: format!("{kind} ({id})"),
                rect: rect(bounds),
                is_primary: id == main,
            }
        })
        .collect()
}

/// The display id a monitor's name carries — see [`monitors`] — or `None`
/// for a name that is not one of this platform's.
pub fn display_id(name: &str) -> Option<u32> {
    let (_, id) = name.strip_suffix(')')?.rsplit_once('(')?;
    id.parse().ok()
}

/// Where the displays are, for deciding whether a remembered window position
/// still lands on one.
pub fn displays() -> Vec<MonitorRect> {
    monitors().into_iter().map(|monitor| monitor.rect).collect()
}

/// Every application window, front to back — on this Space or another,
/// since ScreenCaptureKit captures a window wherever it is.
///
/// At the normal level only — not the menu bar, the Dock or a status item —
/// and not this application's own, which capturing would only mirror back.
/// One not on screen is listed where it has a title, as `media-pp`'s own
/// listing does: off screen, the untitled ones are mostly an application's
/// hidden helpers.
pub fn windows() -> Vec<WindowTarget> {
    let Some(list) = CGWindowListCopyWindowInfo(
        CGWindowListOption::OptionAll | CGWindowListOption::ExcludeDesktopElements,
        kCGNullWindowID,
    ) else {
        return Vec::new();
    };
    // SAFETY: Core Graphics documents the list as an array of dictionaries
    // keyed by strings, which is exactly what this reads it as.
    let list: CFRetained<CFArray<CFDictionary<CFString, CFType>>> =
        unsafe { CFRetained::cast_unchecked(list) };
    let own = std::process::id();
    list.iter()
        .filter_map(|info| window(&info))
        .filter(|(_, pid)| *pid != own)
        .map(|(window, _)| window)
        .collect()
}

/// One entry of the window list, and the process it belongs to — or `None`
/// for one that is not an application's ordinary window.
fn window(info: &CFDictionary<CFString, CFType>) -> Option<(WindowTarget, u32)> {
    // SAFETY: every key read here is one of Core Graphics' own window keys,
    // live for the process.
    let (layer, handle, pid, process, title, bounds, on_screen) = unsafe {
        (
            number(info, kCGWindowLayer)?,
            number(info, kCGWindowNumber)?,
            number(info, kCGWindowOwnerPID)?,
            text(info, kCGWindowOwnerName)?,
            // Only there once screen recording is allowed.
            text(info, kCGWindowName).unwrap_or_default(),
            info.get(kCGWindowBounds)?.downcast::<CFDictionary>().ok()?,
            info.get(kCGWindowIsOnscreen)
                .and_then(|value| value.downcast::<CFBoolean>().ok())
                .is_some_and(|value| value.as_bool()),
        )
    };
    if layer != 0 || (!on_screen && title.is_empty()) {
        return None;
    }
    let mut frame = CGRect::default();
    // SAFETY: `bounds` is the window's bounds dictionary, the shape this
    // reads, and `frame` a live out parameter.
    if !unsafe { CGRectMakeWithDictionaryRepresentation(Some(&bounds), &mut frame) } {
        return None;
    }
    let size = (frame.size.width as u32, frame.size.height as u32);
    if size.0 < SMALLEST_WINDOW || size.1 < SMALLEST_WINDOW {
        return None;
    }
    Some((
        WindowTarget {
            handle: handle as isize,
            title,
            process,
            size,
        },
        pid as u32,
    ))
}

/// A number in a window's entry.
///
/// # Safety
///
/// `key` must be one of Core Graphics' window keys.
unsafe fn number(info: &CFDictionary<CFString, CFType>, key: &CFString) -> Option<i64> {
    info.get(key)?.downcast::<CFNumber>().ok()?.as_i64()
}

/// A string in a window's entry.
///
/// # Safety
///
/// `key` must be one of Core Graphics' window keys.
unsafe fn text(info: &CFDictionary<CFString, CFType>, key: &CFString) -> Option<String> {
    Some(info.get(key)?.downcast::<CFString>().ok()?.to_string())
}

fn rect(bounds: CGRect) -> MonitorRect {
    MonitorRect {
        x: bounds.origin.x as i32,
        y: bounds.origin.y as i32,
        width: bounds.size.width as u32,
        height: bounds.size.height as u32,
    }
}

/// Every camera attached, read through `media-pp` — see
/// [`crate::capture::video_capture_devices`].
pub fn video_capture_devices() -> Vec<VideoCaptureTarget> {
    media_pp::elements::AvFoundationCaptureSource::list_devices()
        .into_iter()
        .map(|device| VideoCaptureTarget {
            id: device.id,
            name: device.name,
        })
        .collect()
}

/// Every mode one camera offers — see
/// [`crate::capture::video_capture_modes`]. Empty for a camera that is not
/// attached, which a picker draws as nothing to choose from.
pub fn video_capture_modes(device: &str) -> Vec<crate::domain::VideoCaptureMode> {
    let Some(device) = media_pp::elements::AvFoundationCaptureSource::list_devices()
        .into_iter()
        .find(|candidate| candidate.id == device)
    else {
        return Vec::new();
    };
    media_pp::elements::AvFoundationCaptureSource::list_formats(&device)
        .into_iter()
        .map(|format| crate::domain::VideoCaptureMode {
            width: format.width,
            height: format.height,
            framerate_numerator: format.frame_rate.numerator().max(0) as u32,
            framerate_denominator: format.frame_rate.denominator().max(1) as u32,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A monitor's name is found again by the id it carries, and a name from
    /// another platform is not mistaken for one.
    #[test]
    fn a_display_is_found_by_the_id_in_its_name() {
        assert_eq!(display_id("Built-in Display (1)"), Some(1));
        assert_eq!(display_id("Display (722475459)"), Some(722_475_459));
        assert_eq!(display_id("DP-1"), None);
        assert_eq!(display_id(r"\\.\DISPLAY1"), None);
        assert_eq!(display_id("Display (main)"), None);
    }

    /// Every Mac has a display, one of them main, each named so it can be
    /// found again — where a session has any display at all.
    #[test]
    fn the_displays_are_listed_and_named_by_their_id() {
        let monitors = monitors();
        if monitors.is_empty() {
            eprintln!("skipped: this session has no display");
            return;
        }
        assert_eq!(
            monitors.iter().filter(|monitor| monitor.is_primary).count(),
            1
        );
        for monitor in &monitors {
            assert!(display_id(&monitor.name).is_some(), "{}", monitor.name);
            assert!(monitor.rect.width > 0 && monitor.rect.height > 0);
        }
    }
}
