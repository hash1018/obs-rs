//! The claim as a `flock`, which is all this needs here.
//!
//! `flock` belongs to the open file description rather than to the process,
//! so a second open of the same path is refused even from inside the same
//! process — and the kernel drops it when the descriptor closes, a killed
//! process included.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::path::Path;

pub(super) fn hold(path: &Path) -> io::Result<Option<File>> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        // Not truncated on open, and that matters: this runs before the lock
        // is known to be ours, so truncating here would wipe the running
        // instance's pid on the way to discovering it is running — costing
        // the raise the pid is written for. The holder clears it itself once
        // it has won, in `record_pid`.
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

/// Not written, and deliberately not guessed at.
///
/// Raising a window is a different act under X11 (`_NET_ACTIVE_WINDOW`
/// through the root window) than under Wayland, where a compositor may
/// refuse it outright — an application is not allowed to steal focus there,
/// which is the point of the protocol rather than a gap in it. Until one of
/// those is written, a second launch says so on stderr and stops, which is
/// the honest half of the behaviour.
pub(super) fn raise(_pid: u32) -> bool {
    false
}
