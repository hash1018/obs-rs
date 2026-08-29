use std::fs;
use std::io;
use std::path::PathBuf;

#[cfg(test)]
use std::path::Path;

use eframe::egui;
use serde::{Deserialize, Serialize};

use crate::i18n::Locale;
use crate::ui::{PreviewZoom, WorkspaceDocks};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    pub locale: Locale,
    pub theme: Theme,
    pub recording: RecordingSettings,
    pub workspace: WorkspaceLayout,
}

/// Where the window was and how the docks were arranged when the application
/// last closed.
///
/// A preference like any other, and stored beside them: it is something the
/// user set deliberately and expects to find again. Written once on exit
/// rather than as it changes — a window being dragged would otherwise rewrite
/// the file every frame.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkspaceLayout {
    /// `None` until the application has closed once, which is what leaves a
    /// first run to the platform's own idea of where a window goes.
    pub window: Option<WindowGeometry>,
    pub docks: WorkspaceDocks,
    pub preview: PreviewZoom,
}

/// Where the window was, in the desktop's own coordinates.
///
/// The position is the outer rect's — the frame, not the client area — since
/// that is what a window manager is asked to place. The size is the inner
/// one, because that is what `ViewportBuilder` takes.
///
/// # The position is optional, and on Wayland it is always absent
///
/// A Wayland client is not told where it is and cannot ask to be put
/// anywhere: winit answers `outer_position` with `NotSupportedError` and
/// implements `set_outer_position` as an empty function saying "Not possible
/// on Wayland". That is the protocol rather than a gap in winit — the
/// compositor places windows and does not let a client argue.
///
/// So the position is stored only where the platform will say what it is, and
/// a file written on such a session simply has none. Keeping a stale one
/// instead would be worse than having none: it survives into an X11 session
/// and throws the window to wherever it last was several sessions ago.
///
/// The size has no such problem. Wayland will not report the window's rect
/// either — egui builds that from a position it cannot get — but the size is
/// what egui itself is drawing into, so `screen_rect` has it, and asking to
/// open at a size works everywhere.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WindowGeometry {
    /// `None` on a platform that will not say where its windows are.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y: Option<f32>,
    pub width: f32,
    pub height: f32,
    /// Restored as a maximized window. The rect beside it is the one the
    /// window had *before* it was maximized, so unmaximizing puts it back
    /// where it was rather than filling the screen a second time.
    pub maximized: bool,
}

/// Which palette the window draws in.
///
/// This crate's own enum rather than `egui::ThemePreference`, which is not
/// serialisable — and would not be worth storing directly anyway, since what
/// is written to a settings file should be named by this application rather
/// than by whichever version of a UI library it happens to use.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Theme {
    /// Follow whatever the desktop is set to.
    System,
    Light,
    /// The default this application has always started in, kept so that
    /// persisting the setting does not change what a first run looks like.
    #[default]
    Dark,
}

impl Theme {
    pub const ALL: [Self; 3] = [Self::System, Self::Light, Self::Dark];
}

impl From<Theme> for egui::ThemePreference {
    fn from(theme: Theme) -> Self {
        match theme {
            Theme::System => Self::System,
            Theme::Light => Self::Light,
            Theme::Dark => Self::Dark,
        }
    }
}

impl From<egui::ThemePreference> for Theme {
    fn from(preference: egui::ThemePreference) -> Self {
        match preference {
            egui::ThemePreference::System => Self::System,
            egui::ThemePreference::Light => Self::Light,
            egui::ThemePreference::Dark => Self::Dark,
        }
    }
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
    /// Which H.264 encoder writes the file. Not every one of them can be
    /// opened on every machine, which is why the engine probes and publishes
    /// the list this is chosen from.
    pub encoder: RecordingEncoder,
    /// Where files are written. Empty means the platform default — see
    /// [`RecordingSettings::directory_or_default`], which is what resolves it.
    ///
    /// Stored as written rather than resolved, so a default that follows the
    /// user's Videos folder keeps following it. Writing the resolved path in
    /// would freeze today's answer into the file.
    pub directory: String,
    /// What each file is named before its timestamp. The timestamp itself is
    /// not configurable — see [`crate::paths::recording_file_in`].
    pub name_prefix: String,
    /// Frames per second written to the file.
    ///
    /// Independent of the compositor's rate, which the Preview and the
    /// reported figure are made of and which this must never raise: a
    /// recording is a branch off those frames, so it can take fewer of them
    /// but not more than exist.
    pub fps: u32,
    /// Target bit rate in megabits per second.
    pub bit_rate_mbps: u32,
    /// Seconds between keyframes.
    pub keyframe_seconds: u32,
}

impl Default for RecordingSettings {
    fn default() -> Self {
        Self {
            encoder: RecordingEncoder::default(),
            directory: String::new(),
            name_prefix: crate::paths::APPLICATION.to_owned(),
            fps: DEFAULT_FPS,
            bit_rate_mbps: DEFAULT_BIT_RATE_MBPS,
            keyframe_seconds: DEFAULT_KEYFRAME_SECONDS,
        }
    }
}

/// Which H.264 encoder a recording is written with.
///
/// Only H.264 for now, which is why nothing here names a codec: the choice is
/// between the ways to produce it, not between formats.
///
/// # Hardware and software are not interchangeable
///
/// [`RecordingEncoder::Nvenc`] takes the compositor's own frames as they are —
/// already on the GPU, already in the format NVENC wants — so a recording
/// costs an encode and nothing else. Everything else here is a software
/// encoder, and reaching one means copying every frame back from the GPU and
/// converting it to `YUV420P` first. At 1080p60 that is unlikely to keep up,
/// and the recording branch's queue reports the overload on the bus rather
/// than dropping frames quietly.
///
/// It is offered regardless. A machine with no NVENC — an AMD or Intel GPU on
/// Windows, where the compositor runs anyway — would otherwise have no way to
/// record at all, and a smaller Canvas or a lower rate is a real answer there.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RecordingEncoder {
    /// `h264_nvenc`, fed the compositor's frames directly.
    #[default]
    Nvenc,
    /// `libopenh264` — Cisco's encoder, whose licence terms are why it is the
    /// software H.264 encoder most FFmpeg builds carry.
    OpenH264,
    /// `libx264`. Absent from a good many FFmpeg builds, this machine's
    /// included, which is exactly why the list is probed rather than assumed.
    X264,
}

impl RecordingEncoder {
    pub const ALL: [Self; 3] = [Self::Nvenc, Self::OpenH264, Self::X264];

    /// Whether reaching this encoder means copying frames back from the GPU
    /// and converting them.
    pub fn is_software(self) -> bool {
        !matches!(self, Self::Nvenc)
    }

    /// What to call it in a list.
    ///
    /// Not translated: these are the names FFmpeg itself uses, and someone
    /// comparing them against `ffmpeg -encoders` or a forum post needs to see
    /// the same string.
    pub fn label(self) -> &'static str {
        match self {
            Self::Nvenc => "NVENC (h264_nvenc)",
            Self::OpenH264 => "OpenH264 (libopenh264)",
            Self::X264 => "x264 (libx264)",
        }
    }
}

/// Enough for 1080p screen content at the compositor's rate, where large
/// still areas cost almost nothing and a scrolling window is the peak. The
/// figure the recording constants carried before this was settable.
pub const DEFAULT_BIT_RATE_MBPS: u32 = 12;

/// What a recording is written at unless it is changed — the compositor's own
/// rate, so the default records every frame that is composited.
pub const DEFAULT_FPS: u32 = 60;

/// The rates offered. Not a free number: an encoder is configured for exactly
/// what it is given, and a rate the compositor cannot supply would write a
/// file that claims more frames a second than it holds.
pub const FPS_CHOICES: [u32; 4] = [24, 30, 48, 60];

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
        let mbps = self
            .bit_rate_mbps
            .clamp(*BIT_RATE_MBPS_RANGE.start(), *BIT_RATE_MBPS_RANGE.end());
        mbps as usize * 1_000_000
    }

    /// The rate to write at, never above what the compositor produces.
    ///
    /// Clamped rather than rejected: a settings file naming 120 against a
    /// compositor running at 60 is asking for frames that do not exist, and
    /// the honest answer is the most it can have.
    pub fn fps_within(&self, compositor_fps: u32) -> u32 {
        self.fps.clamp(1, compositor_fps.max(1))
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

    /// A recording is a branch off the compositor's frames, so it can take
    /// fewer of them but never more than exist.
    #[test]
    fn the_recorded_rate_never_exceeds_what_the_compositor_produces() {
        let asking_too_much = RecordingSettings {
            fps: 120,
            ..RecordingSettings::default()
        };
        assert_eq!(asking_too_much.fps_within(60), 60);

        let asking_for_less = RecordingSettings {
            fps: 24,
            ..RecordingSettings::default()
        };
        assert_eq!(asking_for_less.fps_within(60), 24);
    }

    /// The encoder is written under its own name, so a settings file says
    /// which one was chosen rather than which position it held in a list.
    #[test]
    fn encoder_uses_expected_toml_value() {
        for (encoder, written) in [
            (RecordingEncoder::Nvenc, "nvenc"),
            (RecordingEncoder::OpenH264, "open-h264"),
            (RecordingEncoder::X264, "x264"),
        ] {
            let settings = AppSettings {
                recording: RecordingSettings {
                    encoder,
                    ..RecordingSettings::default()
                },
                ..AppSettings::default()
            };

            let encoded = toml::to_string(&settings).expect("encode");
            assert!(
                encoded.contains(&format!("encoder = \"{written}\"\n")),
                "{encoder:?} was written as something else: {encoded}"
            );
            assert_eq!(
                toml::from_str::<AppSettings>(&encoded)
                    .unwrap()
                    .recording
                    .encoder,
                encoder
            );
        }
    }

    /// Only the hardware one takes the compositor's frames as they are; the
    /// rest decide whether a download and a conversion go in front of them.
    #[test]
    fn only_nvenc_is_not_a_software_encoder() {
        assert!(!RecordingEncoder::Nvenc.is_software());
        assert!(RecordingEncoder::OpenH264.is_software());
        assert!(RecordingEncoder::X264.is_software());
    }

    /// A settings file written before the position could be absent has both
    /// halves; one written on Wayland has neither. Both have to load.
    #[test]
    fn a_window_geometry_loads_with_or_without_a_position() {
        let with: AppSettings = toml::from_str(
            "[workspace.window]\nx = 300.0\ny = 150.0\nwidth = 900.0\nheight = 640.0\nmaximized = false\n",
        )
        .expect("load a geometry with a position");
        let window = with.workspace.window.expect("a window");
        assert_eq!((window.x, window.y), (Some(300.0), Some(150.0)));
        assert_eq!((window.width, window.height), (900.0, 640.0));

        let without: AppSettings = toml::from_str(
            "[workspace.window]\nwidth = 900.0\nheight = 640.0\nmaximized = false\n",
        )
        .expect("load a geometry with no position");
        let window = without.workspace.window.expect("a window");
        assert_eq!((window.x, window.y), (None, None));
        assert_eq!((window.width, window.height), (900.0, 640.0));
    }

    /// An absent position writes no keys at all, rather than nulls a reader
    /// would have to know to ignore.
    #[test]
    fn an_absent_position_is_left_out_of_the_file() {
        let settings = AppSettings {
            workspace: WorkspaceLayout {
                window: Some(WindowGeometry {
                    x: None,
                    y: None,
                    width: 900.0,
                    height: 640.0,
                    maximized: false,
                }),
                ..Default::default()
            },
            ..AppSettings::default()
        };

        let encoded = toml::to_string(&settings).expect("encode");
        assert!(!encoded.contains("\nx = "), "unexpected x: {encoded}");
        assert!(!encoded.contains("\ny = "), "unexpected y: {encoded}");
        assert!(encoded.contains("width = 900.0"), "missing size: {encoded}");
    }

    /// The theme is written as this application's own name for it, not as
    /// whatever the UI library happens to call the variant.
    #[test]
    fn theme_uses_expected_toml_value() {
        let settings = AppSettings {
            theme: Theme::Light,
            ..AppSettings::default()
        };

        let encoded = toml::to_string(&settings).expect("encode");
        assert!(
            encoded.contains("theme = \"light\"\n"),
            "unexpected encoding: {encoded}"
        );
        assert_eq!(
            toml::from_str::<AppSettings>(&encoded).unwrap().theme,
            Theme::Light
        );
    }

    /// Dark is what this application has always started in, and persisting
    /// the setting must not change what a first run looks like.
    #[test]
    fn a_file_without_a_theme_still_starts_dark() {
        let settings: AppSettings = toml::from_str("locale = \"en-US\"\n").expect("load");

        assert_eq!(settings.theme, Theme::Dark);
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

        assert_eq!(
            settings.directory_or_default(),
            crate::paths::recordings_dir()
        );
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
