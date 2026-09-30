//! The claim on macOS: `flock`, as on Linux; and the running copy brought
//! forward through `NSRunningApplication`.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::path::Path;

pub(super) fn hold(path: &Path) -> io::Result<Option<File>> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        // Not truncated on open, for the reason the Linux half gives: this
        // runs before the lock is known to be ours, and the running copy's
        // pid is what a second launch reads.
        .truncate(false)
        .open(path)?;
    // SAFETY: `file` owns the descriptor for the whole call, and `flock`
    // does nothing with it beyond the lock.
    let locked = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if locked == 0 {
        return Ok(Some(file));
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        // Held by someone else, which is an answer rather than a fault.
        Some(libc::EWOULDBLOCK) => Ok(None),
        _ => Err(error),
    }
}

/// Brings `pid`'s windows forward, showing it again if it was hidden.
///
/// Since macOS 14 an application is activated only by one that is active
/// itself and yields to it: asked by anything else, it stays where it was —
/// measured, with a Finder window in front. This launch has not become
/// active by the time it asks, since it quits before it has a window, so it
/// becomes active first — as an accessory, so without a Dock icon — and
/// yields to the running copy. Before macOS 14, where yielding does not
/// exist, asking was enough.
pub(super) fn raise(pid: u32) -> bool {
    use objc2::runtime::NSObjectProtocol;
    use objc2::{MainThreadMarker, sel};
    use objc2_app_kit::{
        NSApplication, NSApplicationActivationOptions, NSApplicationActivationPolicy,
        NSRunningApplication,
    };

    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    let Some(running) = NSRunningApplication::runningApplicationWithProcessIdentifier(pid) else {
        return false;
    };
    running.unhide();
    let options = NSApplicationActivationOptions::ActivateAllWindows;
    // Asked from `main`, before anything else runs: this is the main thread.
    if let Some(main_thread) = MainThreadMarker::new() {
        let this = NSApplication::sharedApplication(main_thread);
        if this.respondsToSelector(sel!(yieldActivationToApplication:)) {
            this.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
            #[allow(deprecated)]
            this.activateIgnoringOtherApps(true);
            this.yieldActivationToApplication(&running);
            return running.activateFromApplication_options(
                &NSRunningApplication::currentApplication(),
                options,
            );
        }
    }
    #[allow(deprecated)]
    running.activateWithOptions(options)
}
