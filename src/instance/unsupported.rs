//! No exclusion, on a platform this application has no backend for either.
//!
//! Kept so the crate still compiles where `engine::backend` is already the
//! unsupported one — a build that reaches this cannot open a compositor, so
//! whether it would have allowed a second copy of itself is not a question
//! worth an implementation. Whoever writes a backend here writes this too:
//! `flock` is what the Linux half uses and works on every other Unix.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

pub(super) fn hold(path: &Path) -> io::Result<Option<File>> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(path)?;
    Ok(Some(file))
}

pub(super) fn raise(_pid: u32) -> bool {
    false
}
