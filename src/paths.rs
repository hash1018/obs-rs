//! Where this application's files live on each operating system.
//!
//! The one piece of platform knowledge in the tree with no subject of its own.
//! Settings and the project database want different directories but ask the
//! same question, and were answering it separately — in different styles, and
//! with different fallbacks for the case where the environment says nothing.

use std::path::PathBuf;

/// The directory name both trees hang under.
const APPLICATION: &str = "obs-rs";

/// Per-user configuration: preferences that are not project data.
pub fn config_dir() -> PathBuf {
    base(Kind::Config).join(APPLICATION)
}

/// Per-user data: the project database, and anything else the user creates.
pub fn data_dir() -> PathBuf {
    base(Kind::Data).join(APPLICATION)
}

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
}
