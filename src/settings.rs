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
    pub audio: AudioSettings,
    pub hotkeys: crate::hotkey::HotkeySettings,
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

/// What the audio mixer sums into, and therefore what a recording's audio
/// track is made of.
///
/// Its own group rather than part of [`RecordingSettings`], because it is not
/// a recording setting: the mixer runs whether or not anything is recording,
/// and the level meters are reading its output either way.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioSettings {
    /// Hertz. 48 kHz is what both platforms' devices are overwhelmingly
    /// already at, so the mixer's own resamplers usually have nothing to do.
    pub sample_rate: u32,
    /// 1 for mono, 2 for stereo.
    pub channels: u16,
    /// Which playback endpoint monitoring is heard through, or `None` for
    /// none — which is what every installation starts as, and is not the same
    /// as the system default.
    ///
    /// Deliberately not defaulting to the default output. That endpoint is
    /// usually the one Desktop Audio is captured from, and playing the mix
    /// back into it feeds the loopback its own output: a howl that grows with
    /// every pass. Somebody has to say where monitoring goes, and headphones
    /// are the answer that cannot do this.
    pub monitor_device: Option<String>,
}

impl Default for AudioSettings {
    fn default() -> Self {
        Self {
            sample_rate: DEFAULT_SAMPLE_RATE,
            channels: DEFAULT_CHANNELS,
            monitor_device: None,
        }
    }
}

impl AudioSettings {
    /// Whether the two settings that decide what the mixer sums into differ.
    ///
    /// Asked separately from `!=` because the monitoring endpoint lives in
    /// this group without belonging to that question: changing the mix format
    /// reopens the encoder a recording is being written with and so is
    /// refused while one runs, and changing what you are listening to has
    /// nothing to do with the file.
    pub fn mix_differs_from(&self, other: &Self) -> bool {
        self.sample_rate != other.sample_rate || self.channels != other.channels
    }
}

/// What the mixer runs at unless it is changed. It was this crate's constant
/// before it was settable.
pub const DEFAULT_SAMPLE_RATE: u32 = 48_000;
pub const DEFAULT_CHANNELS: u16 = 2;

/// The rates offered.
///
/// Not a free number: every one of these has to be something the audio
/// encoders can be opened at, and 44.1 already costs `libopus` — which takes
/// 48/24/16/12/8 kHz and nothing else, and drops out of the encoder list when
/// the mixer is not at one of them.
pub const SAMPLE_RATE_CHOICES: [u32; 2] = [48_000, 44_100];

/// Mono or stereo. More would need a channel layout the mixer does not build
/// and a UI that can place them, neither of which anything has asked for.
pub const CHANNEL_CHOICES: [u16; 2] = [2, 1];

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
    /// Frames per second, which is the compositor's rate and therefore the
    /// file's — there is nothing between them that re-rates.
    ///
    /// Kept here rather than in a settings group of its own because the file
    /// this is stored in does not have to mirror the dialog's tabs, and
    /// moving it would leave every existing settings file resetting to the
    /// default. It is presented on the Video page, where it belongs.
    pub fps: u32,
    /// The height the file is written at, scaled down from the Scene Canvas.
    ///
    /// A height rather than a pair, because the width follows from the
    /// canvas's own aspect ratio — see [`RecordingSettings::output_size`],
    /// which is the only thing that should compute one. A height at or above
    /// the canvas's means no scaling at all, which is what the default is.
    ///
    /// This one really is a recording setting: only the file is scaled. The
    /// Preview and anything else off the compositor's `Tee` keep the canvas.
    pub output_height: u32,
    /// Target video bit rate in megabits per second.
    pub bit_rate_mbps: u32,
    /// Which codec the audio track is written with.
    pub audio_codec: RecordingAudioCodec,
    /// Target audio bit rate in kilobits per second. Kilobits rather than
    /// megabits because the useful range is two orders of magnitude below
    /// the video one, and 0.16 Mb/s is a worse way to write 160 kb/s.
    pub audio_bit_rate_kbps: u32,
    /// Seconds between keyframes.
    pub keyframe_seconds: u32,
    /// Which container the file is written into.
    ///
    /// A recording setting rather than a video one: it changes nothing the
    /// compositor does, only what the next file's header is.
    pub format: RecordingFormat,
    /// Whether one recording is cut into several files, and by what.
    ///
    /// Ignored for [`RecordingFormat::Hls`], which segments on its own terms
    /// — see [`RecordingFormat::segments_itself`].
    pub split: RecordingSplit,
    /// Minutes per file for [`RecordingSplit::Time`].
    pub split_minutes: u32,
    /// Megabytes per file for [`RecordingSplit::Size`].
    ///
    /// A floor, not a cap: the cut happens at the first keyframe past it, so
    /// a file runs over by up to one GOP. A hard limit would mean cutting
    /// mid-GOP, and a segment that does not start at a keyframe is one no
    /// player can open on its own.
    pub split_megabytes: u32,
}

impl Default for RecordingSettings {
    fn default() -> Self {
        Self {
            encoder: RecordingEncoder::default(),
            directory: String::new(),
            name_prefix: crate::paths::APPLICATION.to_owned(),
            fps: DEFAULT_FPS,
            bit_rate_mbps: DEFAULT_BIT_RATE_MBPS,
            audio_codec: RecordingAudioCodec::default(),
            audio_bit_rate_kbps: DEFAULT_AUDIO_BIT_RATE_KBPS,
            keyframe_seconds: DEFAULT_KEYFRAME_SECONDS,
            format: RecordingFormat::default(),
            split: RecordingSplit::default(),
            split_minutes: DEFAULT_SPLIT_MINUTES,
            split_megabytes: DEFAULT_SPLIT_MEGABYTES,
            output_height: 0,
        }
    }
}

/// How far a recording can be scaled down from the Scene Canvas, as
/// divisors of it.
///
/// Divisors rather than a list of heights, so the choices stay sensible for
/// whatever canvas they are computed against: 1080p gives 1080/900/720/540,
/// and a canvas twice that would give twice those. The same shape OBS offers,
/// and for the same reason — a recording is scaled *relative to* what is
/// being composited, not to a fixed set of numbers.
pub const OUTPUT_SCALES: [f32; 4] = [1.0, 1.2, 1.5, 2.0];

/// The heights offered for a canvas this tall, largest first.
///
/// Even, because encoders reject odd dimensions and it is better to round
/// here than to have one refuse to open.
pub fn output_heights(canvas_height: u32) -> Vec<u32> {
    OUTPUT_SCALES
        .iter()
        .map(|scale| even(canvas_height as f32 / scale))
        .collect()
}

/// Rounds to the nearest even number of at least two — what every H.264
/// encoder here requires of both dimensions.
///
/// To the *nearest* even, not down to one: 16:9 at 480 tall is 853.3 wide,
/// and rounding down gives 852, which is a slightly wrong aspect ratio where
/// 854 is the width everything else in the world calls 480p.
fn even(value: f32) -> u32 {
    (((value / 2.0).round() as u32) * 2).max(2)
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
    /// `h264_nvenc`, fed the compositor's frames directly. First because it
    /// reaches NVIDIA's encoder through NVIDIA's own API rather than through
    /// a layer that has to pick a transform.
    #[default]
    Nvenc,
    /// `h264_mf`, fed the compositor's frames directly. Media Foundation
    /// hands back whichever hardware transform the installed driver
    /// registers, so this is the one hardware entry an Intel or AMD machine
    /// has — without it those record on the CPU while their encode block sits
    /// idle. On an NVIDIA machine it reaches the same block `Nvenc` does, by
    /// a longer route, which is why it is second rather than first.
    MediaFoundation,
    /// `libopenh264` — Cisco's encoder, whose licence terms are why it is the
    /// software H.264 encoder most FFmpeg builds carry.
    OpenH264,
    /// `libx264`. Absent from a good many FFmpeg builds, this machine's
    /// included, which is exactly why the list is probed rather than assumed.
    X264,
}

impl RecordingEncoder {
    /// In preference order, which is also the order the Settings dialog
    /// lists them and the order [`Self::best_of`] falls through.
    pub const ALL: [Self; 4] = [
        Self::Nvenc,
        Self::MediaFoundation,
        Self::OpenH264,
        Self::X264,
    ];

    /// Whether reaching this encoder means copying frames back from the GPU
    /// and converting them.
    pub fn is_software(self) -> bool {
        !matches!(self, Self::Nvenc | Self::MediaFoundation)
    }

    /// The most preferred of `available`, or `None` when it is empty.
    ///
    /// What a machine whose saved encoder cannot open falls back to. The
    /// stored choice is deliberately not overwritten by this — someone who
    /// picked NVENC on a machine that has it should still have it selected
    /// after recording once on a laptop that does not.
    pub fn best_of(available: &[Self]) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|encoder| available.contains(encoder))
    }

    /// What to call it in a list.
    ///
    /// Not translated: these are the names FFmpeg itself uses, and someone
    /// comparing them against `ffmpeg -encoders` or a forum post needs to see
    /// the same string.
    pub fn label(self) -> &'static str {
        match self {
            Self::Nvenc => "NVENC (h264_nvenc)",
            Self::MediaFoundation => "Media Foundation (h264_mf)",
            Self::OpenH264 => "OpenH264 (libopenh264)",
            Self::X264 => "x264 (libx264)",
        }
    }
}

/// Which codec a recording's audio track is written with.
///
/// Both are carried by MP4. Neither needs `--enable-gpl`, unlike the software
/// H.264 encoders beside them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RecordingAudioCodec {
    /// FFmpeg's own built-in `aac`. The default because it is in every build
    /// and every player, which is the whole job for a screen recording.
    #[default]
    Aac,
    /// `libopus`. Better at a given bit rate, especially for speech, but an
    /// external library a stripped FFmpeg build may not carry — which is why
    /// this list is probed rather than assumed, the same as the video one.
    Opus,
}

impl RecordingAudioCodec {
    /// In preference order, which is also the order the dialog lists them.
    pub const ALL: [Self; 2] = [Self::Aac, Self::Opus];

    /// The most preferred of `available`, or `None` when it is empty.
    pub fn best_of(available: &[Self]) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|codec| available.contains(codec))
    }

    /// What to call it in a list — FFmpeg's own name, so it can be compared
    /// against `ffmpeg -encoders`.
    pub fn label(self) -> &'static str {
        match self {
            Self::Aac => "AAC (aac)",
            Self::Opus => "Opus (libopus)",
        }
    }
}

/// What a recording's audio track is written at unless it is changed.
///
/// 160 kb/s is comfortable for 48 kHz stereo of one desktop's output, where
/// more is not heard. It was this crate's constant before it was settable.
pub const DEFAULT_AUDIO_BIT_RATE_KBPS: u32 = 160;

/// What the dialog accepts. Wide rather than tight, the same reasoning as the
/// video bit rate's own range: these bound what is representable, not what is
/// sensible.
pub const AUDIO_BIT_RATE_KBPS_RANGE: std::ops::RangeInclusive<u32> = 32..=512;

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

/// Which container a recording is written into.
///
/// One `media-pp` element writes all three: `FileMuxer` asks FFmpeg to guess
/// a muxer from the file name, so the choice here is very nearly a choice of
/// extension. HLS is the exception — it is a playlist and a directory of
/// segments rather than a file, and a different element.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RecordingFormat {
    /// The default, and the one every player and editor takes. Its weakness
    /// is that it is finalized at the end: a recording that dies with the
    /// application leaves an unplayable file.
    #[default]
    Mp4,
    /// Matroska. Worth offering for exactly the weakness above — a `.mkv`
    /// that was never finalized still plays up to where it stopped, which is
    /// why OBS recommends it for long recordings.
    Mkv,
    /// An HLS playlist and its segments, in a directory of their own. Not a
    /// convenience for playback but for what a recording can be handed to:
    /// it is servable as-is, and every completed segment is on disk before
    /// the recording ends.
    Hls,
}

impl RecordingFormat {
    /// In the order the dialog lists them.
    pub const ALL: [Self; 3] = [Self::Mp4, Self::Mkv, Self::Hls];

    /// What the recording's own path ends in. For HLS that is the playlist's,
    /// not a segment's.
    pub fn extension(self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::Mkv => "mkv",
            Self::Hls => "m3u8",
        }
    }

    /// Whether this format writes more than one file even when nothing asked
    /// it to, and therefore wants a directory to itself.
    ///
    /// Also what makes [`RecordingSettings::split`] meaningless here: HLS
    /// already cuts on its own target duration, and a second policy cutting
    /// the same stream would be two muxers arguing about one keyframe.
    pub fn segments_itself(self) -> bool {
        matches!(self, Self::Hls)
    }

    /// What to call it in a list.
    pub fn label(self) -> &'static str {
        match self {
            Self::Mp4 => "MP4 (.mp4)",
            Self::Mkv => "Matroska (.mkv)",
            Self::Hls => "HLS (.m3u8)",
        }
    }
}

/// What cuts one recording into several files.
///
/// The cut always lands on a keyframe, whichever of these asked for it, so
/// every file opens on its own. Which means a file runs past the figure
/// rather than stopping at it — see [`RecordingSettings::split_megabytes`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RecordingSplit {
    /// One file, however long the recording runs.
    #[default]
    Off,
    /// A new file every [`RecordingSettings::split_minutes`].
    Time,
    /// A new file every [`RecordingSettings::split_megabytes`].
    Size,
}

impl RecordingSplit {
    /// In the order the dialog lists them.
    pub const ALL: [Self; 3] = [Self::Off, Self::Time, Self::Size];
}

/// Long enough that a normal recording is still one file, short enough that
/// splitting on time is doing something.
pub const DEFAULT_SPLIT_MINUTES: u32 = 15;

/// A quarter-hour at the default bit rate lands near here, so the two
/// defaults cut at roughly the same place.
pub const DEFAULT_SPLIT_MEGABYTES: u32 = 1_024;

/// A minute at the bottom because a keyframe every two seconds makes anything
/// shorter mostly keyframes; twelve hours at the top because that is past any
/// recording this is the right tool for.
pub const SPLIT_MINUTES_RANGE: std::ops::RangeInclusive<u32> = 1..=720;

/// The floor is one GOP's worth at any sane bit rate — below it the keyframe
/// wait, not the figure, is what decides the size. The ceiling is FAT32's
/// file limit, which is the practical reason to split by size at all.
pub const SPLIT_MEGABYTES_RANGE: std::ops::RangeInclusive<u32> = 50..=4_096;

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

    /// The size the file is written at, given the Scene Canvas it is scaled
    /// from.
    ///
    /// The canvas itself unless [`Self::output_height`] asks for less, and
    /// then that height with the width following the canvas's aspect ratio.
    /// Both even, because H.264 encoders refuse odd dimensions.
    ///
    /// Derived rather than stored as a pair: a width and a height that can
    /// disagree with the canvas is a stretched recording nobody asked for,
    /// and the one place to keep them agreeing is here.
    pub fn output_size(&self, canvas: [u32; 2]) -> [u32; 2] {
        let [canvas_width, canvas_height] = canvas;
        if self.output_height == 0 || self.output_height >= canvas_height || canvas_height == 0 {
            return [even(canvas_width as f32), even(canvas_height as f32)];
        }
        let scale = self.output_height as f32 / canvas_height as f32;
        [
            even(canvas_width as f32 * scale),
            even(self.output_height as f32),
        ]
    }

    /// Clamped to what this application will encode at, so a hand-edited
    /// settings file cannot ask an encoder for something it will refuse.
    pub fn bit_rate_bits(&self) -> usize {
        let mbps = self
            .bit_rate_mbps
            .clamp(*BIT_RATE_MBPS_RANGE.start(), *BIT_RATE_MBPS_RANGE.end());
        mbps as usize * 1_000_000
    }

    /// As above, in seconds; the encoder wants it in frames.
    pub fn keyframe_seconds_clamped(&self) -> u32 {
        self.keyframe_seconds.clamp(
            *KEYFRAME_SECONDS_RANGE.start(),
            *KEYFRAME_SECONDS_RANGE.end(),
        )
    }

    /// Which split policy is actually in force, which is none at all for a
    /// format that segments itself.
    ///
    /// Asked here rather than at each call site so the dialog and the
    /// recording cannot disagree about whether HLS is being split — the
    /// dialog greys the control out, and this is what makes that true.
    pub fn effective_split(&self) -> RecordingSplit {
        if self.format.segments_itself() {
            RecordingSplit::Off
        } else {
            self.split
        }
    }

    /// As above, clamped so a hand-edited settings file cannot ask for a
    /// segment shorter than its own keyframe interval.
    pub fn split_minutes_clamped(&self) -> u32 {
        self.split_minutes
            .clamp(*SPLIT_MINUTES_RANGE.start(), *SPLIT_MINUTES_RANGE.end())
    }

    /// As above, in bytes — which is what a segment policy counts.
    pub fn split_bytes(&self) -> u64 {
        let megabytes = self
            .split_megabytes
            .clamp(*SPLIT_MEGABYTES_RANGE.start(), *SPLIT_MEGABYTES_RANGE.end());
        megabytes as u64 * 1_024 * 1_024
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

    /// A machine whose stored encoder cannot open must still record, on the
    /// best one that did. Without this the default of NVENC means the first
    /// Record press on any non-NVIDIA machine fails.
    #[test]
    fn the_fallback_prefers_hardware_then_software() {
        use RecordingEncoder::*;

        assert_eq!(
            best_of_slice(&[Nvenc, MediaFoundation, OpenH264]),
            Some(Nvenc)
        );
        // The case this exists for: no NVENC, but a hardware transform.
        assert_eq!(
            best_of_slice(&[MediaFoundation, OpenH264]),
            Some(MediaFoundation)
        );
        assert_eq!(best_of_slice(&[OpenH264, X264]), Some(OpenH264));
        assert_eq!(best_of_slice(&[X264]), Some(X264));
        // Nothing opened at all — the caller has to say so rather than
        // substitute something that is not there either.
        assert_eq!(best_of_slice(&[]), None);
    }

    fn best_of_slice(available: &[RecordingEncoder]) -> Option<RecordingEncoder> {
        RecordingEncoder::best_of(available)
    }

    /// The order the fallback walks is the order the dialog lists, so a list
    /// that disagreed with it would recommend one thing and choose another.
    #[test]
    fn every_encoder_is_in_the_preference_order_exactly_once() {
        for encoder in RecordingEncoder::ALL {
            assert_eq!(
                RecordingEncoder::ALL
                    .iter()
                    .filter(|listed| **listed == encoder)
                    .count(),
                1,
                "{encoder:?}"
            );
        }
        assert!(
            RecordingEncoder::ALL
                .iter()
                .take_while(|encoder| !encoder.is_software())
                .count()
                > 0,
            "the hardware entries have to come first for the fallback to prefer them"
        );
    }

    /// The default is the canvas itself: a recording is full size until
    /// somebody asks for less.
    #[test]
    fn no_output_height_means_the_canvas() {
        let settings = RecordingSettings::default();
        assert_eq!(settings.output_size([1920, 1080]), [1920, 1080]);
    }

    /// A width is never stored, only derived, so it cannot drift from the
    /// canvas's aspect ratio and stretch the picture.
    #[test]
    fn the_width_follows_the_canvas_aspect_ratio() {
        for (canvas, height, expected) in [
            ([1920, 1080], 720, [1280, 720]),
            ([1920, 1080], 540, [960, 540]),
            ([1280, 720], 480, [854, 480]),
            // 16:10, to prove nothing here assumes 16:9.
            ([1920, 1200], 600, [960, 600]),
        ] {
            let settings = RecordingSettings {
                output_height: height,
                ..RecordingSettings::default()
            };
            assert_eq!(
                settings.output_size(canvas),
                expected,
                "{canvas:?} → {height}"
            );
        }
    }

    /// H.264 encoders refuse odd dimensions, so both sides are rounded here
    /// rather than left to fail at `avcodec_open2`.
    #[test]
    fn both_dimensions_come_out_even() {
        for (canvas, height) in [([1919, 1081], 0), ([1920, 1080], 507), ([1366, 768], 361)] {
            let settings = RecordingSettings {
                output_height: height,
                ..RecordingSettings::default()
            };
            let [width, height] = settings.output_size(canvas);
            assert_eq!(width % 2, 0, "{canvas:?} gave width {width}");
            assert_eq!(height % 2, 0, "{canvas:?} gave height {height}");
            assert!(width >= 2 && height >= 2);
        }
    }

    /// Asking for more than is being composited is asking for detail that
    /// does not exist; the honest answer is the canvas.
    #[test]
    fn an_output_taller_than_the_canvas_is_the_canvas() {
        let settings = RecordingSettings {
            output_height: 2160,
            ..RecordingSettings::default()
        };
        assert_eq!(settings.output_size([1920, 1080]), [1920, 1080]);
    }

    /// The list the dialog offers has to start at the canvas itself, or
    /// "no scaling" would not be reachable from it.
    #[test]
    fn the_offered_heights_start_at_the_canvas() {
        let heights = output_heights(1080);
        assert_eq!(heights.first(), Some(&1080));
        assert!(
            heights.windows(2).all(|pair| pair[0] > pair[1]),
            "largest first, strictly: {heights:?}"
        );
        assert!(heights.iter().all(|height| height % 2 == 0), "{heights:?}");
    }

    /// A build without libopus must still record with sound, on the codec it
    /// does have. The video fallback exists for the same reason.
    #[test]
    fn the_audio_fallback_prefers_aac() {
        use RecordingAudioCodec::*;

        assert_eq!(RecordingAudioCodec::best_of(&[Aac, Opus]), Some(Aac));
        assert_eq!(RecordingAudioCodec::best_of(&[Opus]), Some(Opus));
        assert_eq!(RecordingAudioCodec::best_of(&[]), None);
    }
}
