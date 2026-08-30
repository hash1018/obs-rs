//! One obs-rs at a time.
//!
//! A second copy is not a second workspace. Both open the same project
//! database, both write the same log, and each opens its own captures — two
//! duplications of one desktop, two claims on the same audio endpoints, two
//! compositors on one GPU. Nothing about that is a configuration someone
//! chose; it is what happens when an already-running application is launched
//! again, which is an ordinary thing to do by accident.
//!
//! # A file the running instance holds open
//!
//! The claim is an open file rather than something written into one. An
//! operating system releases a handle however the process ends, a crash
//! included, where a flag written into a file has to be cleared by whoever
//! stopped running — exactly the thing a crash did not get to do. So a lock
//! left behind by a killed instance is not a lock at all, and there is no
//! stale state to detect or clean up.
//!
//! Its *contents* are only a hint: the process id, so a second launch can
//! bring the running window forward instead of appearing to do nothing. A
//! wrong or missing id costs the raise, not the exclusion.
//!
//! # Failing to lock is not failing to start
//!
//! If the lock cannot be created at all — an unwritable data directory — the
//! application starts anyway. Refusing to run because a guard against a rare
//! mistake could not be built would trade a small annoyance for a total one.

use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

#[cfg_attr(target_os = "linux", path = "linux.rs")]
#[cfg_attr(target_os = "windows", path = "windows.rs")]
#[cfg_attr(
    not(any(target_os = "linux", target_os = "windows")),
    path = "unsupported.rs"
)]
mod platform;

/// The lock's name inside the data directory.
///
/// Beside the project database deliberately: what must not be shared is that
/// database and the log next to it, so the lock belongs where they are rather
/// than in a temporary directory that may be per-session.
const LOCK_FILE: &str = "instance.lock";

/// Proof that this process is the one instance, for as long as it is held.
///
/// Dropping it releases the claim, so it belongs in `main` and nowhere else.
pub(crate) struct Instance {
    /// `None` when the lock could not be created and the application is
    /// running unguarded — see this module's own docs.
    _file: Option<File>,
}

/// What asking for the claim answered.
pub(crate) enum Claim {
    /// Nobody else had it. Hold the `Instance` for the process's whole life.
    Ours(Instance),
    /// Another process has it, and said which one where it could.
    Taken { pid: Option<u32> },
}

/// Claims the right to be the running obs-rs.
pub(crate) fn claim() -> Claim {
    claim_at(&crate::paths::data_dir().join(LOCK_FILE))
}

/// Brings the window of the instance holding the claim forward.
///
/// Best effort, and false when there was nothing to raise: the process may
/// have ended between the claim being read and this being asked, and a
/// platform may have no way to ask at all.
pub(crate) fn raise(pid: u32) -> bool {
    platform::raise(pid)
}

fn claim_at(path: &Path) -> Claim {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match platform::hold(path) {
        Ok(Some(mut file)) => {
            // A failure here loses the raise and nothing else, so it is not
            // worth giving up a claim that has already been made.
            let _ = record_pid(&mut file);
            Claim::Ours(Instance { _file: Some(file) })
        }
        Ok(None) => Claim::Taken {
            pid: read_pid(path),
        },
        Err(error) => {
            eprintln!("could not lock {}: {error}", path.display());
            Claim::Ours(Instance { _file: None })
        }
    }
}

fn record_pid(file: &mut File) -> std::io::Result<()> {
    // Truncated first: a shorter id written over a longer one would otherwise
    // leave the tail of whoever ran last.
    file.set_len(0)?;
    write!(file, "{}", std::process::id())?;
    file.flush()
}

/// The id the holder wrote, where it is still readable and still a number.
fn read_pid(path: &Path) -> Option<u32> {
    let mut text = String::new();
    File::open(path).ok()?.read_to_string(&mut text).ok()?;
    text.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    /// A lock of this test run's own, so two of them cannot refuse each
    /// other and read as the thing being tested.
    fn test_lock_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("obs-rs-{}-{name}.lock", std::process::id()))
    }

    /// The whole point, and it has to hold within one process too: the lock
    /// is per open handle, not per process, which is also what makes it
    /// testable without launching a second executable.
    #[test]
    fn a_second_claim_is_refused_and_names_the_holder() {
        let path = test_lock_path("second-claim");
        let _ = std::fs::remove_file(&path);

        let first = claim_at(&path);
        assert!(matches!(first, Claim::Ours(_)), "the first claim must win");

        match claim_at(&path) {
            Claim::Taken { pid } => assert_eq!(
                pid,
                Some(std::process::id()),
                "the holder's own id must be readable while it holds the lock"
            ),
            Claim::Ours(_) => panic!("a second claim must be refused"),
        }

        drop(first);
        let _ = std::fs::remove_file(&path);
    }

    /// Releasing has to be the end of it. A lock that outlived its holder
    /// would leave the application unable to start until someone deleted a
    /// file they have no reason to know about.
    #[test]
    fn the_claim_is_released_with_the_instance() {
        let path = test_lock_path("released");
        let _ = std::fs::remove_file(&path);

        let first = claim_at(&path);
        assert!(matches!(first, Claim::Ours(_)));
        drop(first);

        assert!(
            matches!(claim_at(&path), Claim::Ours(_)),
            "a released lock must be claimable again"
        );
        let _ = std::fs::remove_file(&path);
    }
}
