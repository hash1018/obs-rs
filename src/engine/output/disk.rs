//! How much room is left where recordings are written.
//!
//! One figure, asked of the volume rather than worked out from the files:
//! what matters is whether the next hour of recording fits, and that depends
//! on everything else on the disk as much as on anything this application
//! wrote.

use std::path::Path;

/// Bytes free to this user on the volume holding `directory`, or `None`
/// where that cannot be asked.
///
/// A directory that does not exist yet is asked about through the nearest
/// one above it that does: the first recording creates it, and the room it
/// will have is the room on whatever volume that turns out to be.
pub(in crate::engine) fn available(directory: &Path) -> Option<u64> {
    let existing = directory.ancestors().find(|path| path.is_dir())?;
    platform::available(existing)
}

#[cfg(target_os = "windows")]
mod platform {
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    use windows::core::PCWSTR;

    pub(super) fn available(directory: &Path) -> Option<u64> {
        let wide: Vec<u16> = directory
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let mut free = 0u64;
        // SAFETY: `wide` is a NUL-terminated UTF-16 path that outlives the
        // call, `free` is a live `u64` the one requested figure is written
        // into, and the two figures not asked for are passed as null.
        unsafe {
            GetDiskFreeSpaceExW(
                PCWSTR(wide.as_ptr()),
                Some(&mut free as *mut u64),
                None,
                None,
            )
        }
        .ok()?;
        Some(free)
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    pub(super) fn available(directory: &Path) -> Option<u64> {
        let path = CString::new(directory.as_os_str().as_bytes()).ok()?;
        let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        // SAFETY: `path` is a NUL-terminated path that outlives the call, and
        // `stat` is a correctly sized out-parameter for it.
        if unsafe { libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
            return None;
        }
        // SAFETY: `statvfs` returned success, which is its promise to have
        // filled in the whole structure.
        let stat = unsafe { stat.assume_init() };
        // Blocks free to an unprivileged user, not `f_bfree`: the reserve
        // only root may write into is room a recording will never get.
        u64::from(stat.f_bavail).checked_mul(u64::from(stat.f_frsize))
    }
}

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
mod platform {
    use std::path::Path;

    pub(super) fn available(_directory: &Path) -> Option<u64> {
        None
    }
}

#[cfg(all(test, any(target_os = "windows", target_os = "linux")))]
mod tests {
    use super::*;

    /// The directory the tests run in is on a disk with room on it.
    #[test]
    fn a_directory_that_exists_is_on_a_volume_with_room() {
        let here = std::env::current_dir().expect("a working directory");
        assert!(available(&here).is_some_and(|free| free > 0));
    }

    /// The first recording creates its directory, so before it the room is
    /// that of the nearest directory that does exist.
    #[test]
    fn a_directory_not_created_yet_is_asked_about_through_its_parent() {
        let here = std::env::current_dir().expect("a working directory");
        let missing = here.join("not-created-yet").join("nor-this");
        assert!(!missing.exists());
        assert!(available(&missing).is_some());
    }
}
