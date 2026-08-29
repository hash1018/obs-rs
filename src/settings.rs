use std::fs;
use std::io;
use std::path::PathBuf;

#[cfg(test)]
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::i18n::Locale;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    pub locale: Locale,
    pub recording: RecordingSettings,
}

/// What a recording is written as, and where.
///
/// Every field here is read when a recording *starts*, so changing one takes
/// effect on the next recording rather than the running one — an mp4's header
/// is written before its first frame, and nothing in it can be renegotiated
/// afterwards.
///
/// `#[serde(default)]` on the whole struct, not just on `AppSettings`: a
/// settings file written before this existed has no `[recording]` table at
/// all, and one written by a later version may be missing a field this one
/// gained. Neither should stop the application starting.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RecordingSettings {
    /// Where files are written. Empty means the platform default — see
    /// [`RecordingSettings::directory_or_default`], which is what resolves it.
    ///
    /// Stored as written rather than resolved, so a default that follows the
    /// user's Videos folder keeps following it. Writing the resolved path in
    /// would freeze today's answer into the file.
    pub directory: String,
    /// What each file is named before its timestamp. The timestamp itself is
    /// not configurable — see [`crate::paths::recording_file`].
    pub name_prefix: String,
    /// Target bit rate in megabits per second.
    pub bit_rate_mbps: u32,
    /// Seconds between keyframes.
    pub keyframe_seconds: u32,
}

impl Default for RecordingSettings {
    fn default() -> Self {
        Self {
            directory: String::new(),
            name_prefix: crate::paths::APPLICATION.to_owned(),
            bit_rate_mbps: DEFAULT_BIT_RATE_MBPS,
            keyframe_seconds: DEFAULT_KEYFRAME_SECONDS,
        }
    }
}

/// Enough for 1080p screen content at the compositor's rate, where large
/// still areas cost almost nothing and a scrolling window is the peak. The
/// figure the recording constants carried before this was settable.
pub const DEFAULT_BIT_RATE_MBPS: u32 = 12;

/// Two is the usual compromise: a seek lands within that much of where it
/// aimed, and the cost is one full frame every two seconds rather than every
/// one.
pub const DEFAULT_KEYFRAME_SECONDS: u32 = 2;

/// What the dialog will accept. Wide rather than tight — these bound what is
/// *representable*, not what is sensible, and a caller who wants 2 Mbps for a
/// long screen recording or 100 for near-lossless should not be argued with.
pub const BIT_RATE_MBPS_RANGE: std::ops::RangeInclusive<u32> = 1..=200;

/// Below one second the file is nearly all keyframes; above ten a seek lands
/// far from where it aimed.
pub const KEYFRAME_SECONDS_RANGE: std::ops::RangeInclusive<u32> = 1..=10;

impl RecordingSettings {
    /// Where recordings actually go: the configured directory, or the
    /// platform's own when none was set.
    pub fn directory_or_default(&self) -> PathBuf {
        let trimmed = self.directory.trim();
        if trimmed.is_empty() {
            crate::paths::recordings_dir()
        } else {
            PathBuf::from(trimmed)
        }
    }

    /// The prefix to name files with, falling back to the application's own
    /// when it has been cleared — a file called `-2026-08-29-143335.mp4` is
    /// not what an empty field means.
    pub fn prefix_or_default(&self) -> &str {
        let trimmed = self.name_prefix.trim();
        if trimmed.is_empty() {
            crate::paths::APPLICATION
        } else {
            trimmed
        }
    }

    /// Clamped to what this application will encode at, so a hand-edited
    /// settings file cannot ask an encoder for something it will refuse.
    pub fn bit_rate_bits(&self) -> usize {
        let mbps = self.bit_rate_mbps.clamp(
            *BIT_RATE_MBPS_RANGE.start(),
            *BIT_RATE_MBPS_RANGE.end(),
        );
        mbps as usize * 1_000_000
    }

    /// As above, in seconds; the encoder wants it in frames.
    pub fn keyframe_seconds_clamped(&self) -> u32 {
        self.keyframe_seconds.clamp(
            *KEYFRAME_SECONDS_RANGE.start(),
            *KEYFRAME_SECONDS_RANGE.end(),
        )
    }
}

pub struct SettingsStore {
    path: PathBuf,
}

impl SettingsStore {
    pub fn for_current_user() -> Self {
        Self {
            path: settings_path(),
        }
    }

    pub fn load(&self) -> io::Result<AppSettings> {
        match fs::read_to_string(&self.path) {
            Ok(contents) => toml::from_str(&contents).map_err(io::Error::other),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(AppSettings::default()),
            Err(error) => Err(error),
        }
    }

    pub fn save(&self, settings: &AppSettings) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let contents = toml::to_string_pretty(settings).map_err(io::Error::other)?;
        fs::write(&self.path, contents)
    }

    #[cfg(test)]
    fn path(&self) -> &Path {
        &self.path
    }
}

fn settings_path() -> PathBuf {
    crate::paths::config_dir().join("settings.toml")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locale_uses_expected_toml_value() {
        let settings = AppSettings {
            locale: Locale::KoKr,
            ..AppSettings::default()
        };
        let encoded = toml::to_string(&settings).unwrap();
        assert!(
            encoded.starts_with("locale = \"ko-KR\"\n"),
            "unexpected encoding: {encoded}"
        );
        assert_eq!(
            toml::from_str::<AppSettings>(&encoded).unwrap().locale,
            Locale::KoKr
        );
    }

    /// A settings file written before recording was configurable has no
    /// `[recording]` table at all. Refusing to load it would lose the user's
    /// language along with everything else in the file.
    #[test]
    fn a_file_without_recording_settings_still_loads() {
        let settings: AppSettings = toml::from_str("locale = \"ko-KR\"\n").expect("load");

        assert_eq!(settings.locale, Locale::KoKr);
        assert_eq!(settings.recording, RecordingSettings::default());
    }

    /// And one missing a single field keeps the rest of the table, which is
    /// what makes a settings file written by a later version survive here.
    #[test]
    fn a_recording_table_missing_a_field_keeps_the_others() {
        let settings: AppSettings =
            toml::from_str("[recording]\nbit_rate_mbps = 40\n").expect("load");

        assert_eq!(settings.recording.bit_rate_mbps, 40);
        assert_eq!(
            settings.recording.keyframe_seconds,
            DEFAULT_KEYFRAME_SECONDS
        );
        assert_eq!(settings.recording.name_prefix, crate::paths::APPLICATION);
    }

    /// An empty field means "the default", not an empty path or a file whose
    /// name starts with its own separator.
    #[test]
    fn cleared_fields_fall_back_rather_than_producing_nothing() {
        let settings = RecordingSettings {
            directory: "   ".to_owned(),
            name_prefix: String::new(),
            ..RecordingSettings::default()
        };

        assert_eq!(settings.directory_or_default(), crate::paths::recordings_dir());
        assert_eq!(settings.prefix_or_default(), crate::paths::APPLICATION);
    }

    /// The dialog cannot produce these, but a hand-edited file can, and an
    /// encoder asked for them would refuse to open at all.
    #[test]
    fn values_outside_the_accepted_range_are_clamped_not_passed_on() {
        let too_much = RecordingSettings {
            bit_rate_mbps: 100_000,
            keyframe_seconds: 0,
            ..RecordingSettings::default()
        };

        assert_eq!(
            too_much.bit_rate_bits(),
            *BIT_RATE_MBPS_RANGE.end() as usize * 1_000_000
        );
        assert_eq!(
            too_much.keyframe_seconds_clamped(),
            *KEYFRAME_SECONDS_RANGE.start()
        );
    }

    #[test]
    fn user_settings_file_is_named_settings_toml() {
        assert_eq!(
            SettingsStore::for_current_user()
                .path()
                .file_name()
                .and_then(|name| name.to_str()),
            Some("settings.toml")
        );
    }
}
