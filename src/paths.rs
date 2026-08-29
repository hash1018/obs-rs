//! Where this application's files live on each operating system.
//!
//! The one piece of platform knowledge in the tree with no subject of its own.
//! Settings and the project database want different directories but ask the
//! same question, and were answering it separately — in different styles, and
//! with different fallbacks for the case where the environment says nothing.

use std::path::{Path, PathBuf};

use time::OffsetDateTime;

/// The directory name both trees hang under.
/// Also the default prefix a recording is named with — see
/// [`crate::settings::RecordingSettings::prefix_or_default`].
pub const APPLICATION: &str = "obs-rs";

/// Per-user configuration: preferences that are not project data.
pub fn config_dir() -> PathBuf {
    base(Kind::Config).join(APPLICATION)
}

/// Per-user data: the project database, and anything else the user creates.
pub fn data_dir() -> PathBuf {
    base(Kind::Data).join(APPLICATION)
}

/// Where recordings are written.
///
/// Not under [`data_dir`]: a recording is the user's own video, something
/// they will look for in a file manager and hand to somebody else, not
/// application state kept in a directory the platform hides. So it goes
/// beside their other videos, in a folder named for this application.
pub fn recordings_dir() -> PathBuf {
    videos_dir().join(APPLICATION)
}

/// One recording's full path, named for the moment it started.
///
/// `started` is a parameter rather than read in here so the naming can be
/// asserted against a fixed instant instead of whatever the clock says
/// during a test.
///
/// The timestamp is not part of what a caller chooses. It is what keeps two
/// recordings from colliding, and a name a user could strip it from would
/// make the second one overwrite the first.
pub fn recording_file_in(directory: &Path, prefix: &str, started: OffsetDateTime) -> PathBuf {
    let stamp = started
        .format(STAMP)
        .unwrap_or_else(|_| String::from("unknown"));
    directory.join(format!("{prefix}-{stamp}.mp4"))
}

/// Sortable, and legal on every filesystem this runs on — which rules out
/// the colons of an ISO time.
const STAMP: &[time::format_description::FormatItem<'static>] =
    time::macros::format_description!("[year]-[month]-[day]-[hour][minute][second]");

enum Kind {
    Config,
    Data,
}

#[cfg(target_os = "windows")]
fn base(kind: Kind) -> PathBuf {
    // Roaming for preferences and local for data, which is the split Windows
    // itself makes: settings should follow a user between machines, a project
    // database should not.
    let variable = match kind {
        Kind::Config => "APPDATA",
        Kind::Data => "LOCALAPPDATA",
    };
    std::env::var_os(variable)
        .map(PathBuf::from)
        .unwrap_or_else(fallback)
}

#[cfg(target_os = "macos")]
fn base(_kind: Kind) -> PathBuf {
    // macOS draws no distinction between the two.
    home()
        .map(|home| home.join("Library").join("Application Support"))
        .unwrap_or_else(fallback)
}

#[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
fn base(kind: Kind) -> PathBuf {
    let (variable, default) = match kind {
        Kind::Config => ("XDG_CONFIG_HOME", ".config"),
        Kind::Data => ("XDG_DATA_HOME", ".local/share"),
    };
    std::env::var_os(variable)
        .map(PathBuf::from)
        .or_else(|| home().map(|home| home.join(default)))
        .unwrap_or_else(fallback)
}

/// The user's own videos folder — the parent of [`recordings_dir`].
///
/// Read from the environment like everything else here, rather than through
/// each platform's known-folder API. That is less exact — a user who moved
/// their Videos folder is not followed on Windows, and `XDG_VIDEOS_DIR` is
/// normally set in a config file rather than exported — but it keeps this
/// module what it is, and the cost of being wrong is a folder in a
/// predictable place instead of the user's preferred one.
#[cfg(target_os = "windows")]
fn videos_dir() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .unwrap_or_else(fallback)
        .join("Videos")
}

#[cfg(target_os = "macos")]
fn videos_dir() -> PathBuf {
    home()
        .map(|home| home.join("Movies"))
        .unwrap_or_else(fallback)
}

#[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
fn videos_dir() -> PathBuf {
    std::env::var_os("XDG_VIDEOS_DIR")
        .map(PathBuf::from)
        .or_else(|| home().map(|home| home.join("Videos")))
        .unwrap_or_else(fallback)
}

#[cfg(not(target_os = "windows"))]
fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// The working directory, for an environment that named nowhere to write.
///
/// Relative rather than resolved: nothing here can recover from a missing
/// `HOME`, and failing to start over it would be worse than writing beside
/// whatever launched the application.
fn fallback() -> PathBuf {
    PathBuf::from(".")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_directories_are_named_for_the_application() {
        assert_eq!(config_dir().file_name().unwrap(), APPLICATION);
        assert_eq!(data_dir().file_name().unwrap(), APPLICATION);
    }

    /// Sortable, and with nothing in it a filesystem will refuse — which is
    /// what rules out the colons an ISO time would put in the hour.
    #[test]
    fn a_recording_is_named_for_when_it_started() {
        let started = time::macros::datetime!(2026-08-29 14:30:05 +09:00);

        let file = recording_file_in(&recordings_dir(), APPLICATION, started);

        assert_eq!(
            file.file_name().unwrap(),
            "obs-rs-2026-08-29-143005.mp4",
            "a recording's name must say when it started, in a form a \
             filesystem accepts and a listing sorts"
        );
        assert_eq!(file.parent().unwrap(), recordings_dir());
    }

    /// A prefix the user chose replaces the application's, and the timestamp
    /// stays whatever they did — it is what keeps two recordings apart.
    #[test]
    fn a_chosen_prefix_and_directory_are_both_used() {
        let started = time::macros::datetime!(2026-08-29 14:30:05 +09:00);

        let file = recording_file_in(Path::new("/tmp/clips"), "demo", started);

        assert_eq!(file, Path::new("/tmp/clips/demo-2026-08-29-143005.mp4"));
    }
}
