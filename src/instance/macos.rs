//! The claim on macOS: `flock`, as on Linux.
//!
//! Raising the running copy is not written yet. It is
//! `NSRunningApplication`'s activation, which macOS 14 made cooperative —
//! the running copy is asked, and whether it comes forward depends on what
//! the launched one yields — and that wants trying against a real window
//! before it is relied on. Until then a second launch is refused and the
//! first stays where it was.

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

/// Not written yet — see this module's own docs.
pub(super) fn raise(_pid: u32) -> bool {
    false
}
