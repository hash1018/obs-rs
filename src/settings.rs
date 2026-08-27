use std::fs;
use std::io;
use std::path::PathBuf;

#[cfg(test)]
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::i18n::Locale;

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    pub locale: Locale,
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

#[cfg(target_os = "windows")]
fn settings_path() -> PathBuf {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("obs-rs")
        .join("settings.toml")
}

#[cfg(target_os = "macos")]
fn settings_path() -> PathBuf {
    home_dir()
        .join("Library")
        .join("Application Support")
        .join("obs-rs")
        .join("settings.toml")
}

#[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
fn settings_path() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".config"))
        .join("obs-rs")
        .join("settings.toml")
}

#[cfg(not(target_os = "windows"))]
fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locale_uses_expected_toml_value() {
        let settings = AppSettings {
            locale: Locale::KoKr,
        };
        let encoded = toml::to_string(&settings).unwrap();
        assert_eq!(encoded, "locale = \"ko-KR\"\n");
        assert_eq!(
            toml::from_str::<AppSettings>(&encoded).unwrap().locale,
            Locale::KoKr
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
