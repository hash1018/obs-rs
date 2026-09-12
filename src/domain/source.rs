use std::path::PathBuf;
use std::time::Duration;

use super::{Filter, SceneCanvas};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SourceId(pub i64);

stored_by_name! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum SourceKind {
        DisplayCapture => "display_capture",
        WindowCapture => "window_capture",
        VideoCapture => "video_capture",
        MediaFile => "media_file",
        /// A live network stream, pulled over RTSP — an IP camera, most often.
        Rtsp => "rtsp",
        Image => "image",
        Color => "color",
        Drawing => "drawing",
        /// A line of text drawn by this application rather than captured from
        /// anywhere — a caption, a name plate, a clock.
        Text => "text",
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColorSourceSettings {
    pub size: [f32; 2],
    pub rgba: [u8; 4],
}

stored_by_name! {
    /// Where a line of text sits in the box it is drawn into.
    ///
    /// A box rather than a rectangle that hugs the glyphs, because the surface
    /// the engine pushes is fixed when the Source opens — see `text::open`. So
    /// the string's own width changes underneath it, and this is what decides
    /// which edge stays put while it does. Right for a clock, whose last digit
    /// is the one that must not walk.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub enum TextAlignment {
        #[default]
        Left => "left",
        Centre => "centre",
        Right => "right",
    }
}

/// The glyph height a Text Source starts at, in the box's own pixels.
///
/// Read at 1920x1080 from across a room, which is what a caption is for.
pub const DEFAULT_FONT_SIZE: f32 = 72.0;

stored_by_name! {
    /// What a Text Source says, as opposed to how it looks.
    ///
    /// The three are one enum rather than a flag and a string because they are
    /// the same question — where the words come from — and only one answer can
    /// be true at a time.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub enum TextMode {
        /// What was typed, and nothing else. Redrawn when it is edited.
        #[default]
        Static => "static",
        /// The wall clock, redrawn as it moves.
        Clock => "clock",
        /// A stopwatch this Source owns, started and stopped from the Properties
        /// dock — see [`TextTimer`].
        Timer => "timer",
    }
}

stored_by_name! {
    /// How [`TextMode::Clock`] writes the time.
    ///
    /// A fixed list rather than a format string the user types. A mistyped
    /// format is a Source that silently shows nothing, and there is no good place
    /// to report that — the caption *is* the report, and it is blank.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub enum ClockFormat {
        /// `14:32:07`
        #[default]
        Time => "time",
        /// `14:32`
        TimeToMinute => "time-to-minute",
        /// `2026-09-07 14:32:07`
        DateAndTime => "date-and-time",
        /// `2026-09-07`
        Date => "date",
    }
}

stored_by_name! {
    /// How [`TextMode::Timer`] writes an elapsed duration.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub enum TimerFormat {
        /// `01:23:45`
        #[default]
        HoursMinutesSeconds => "hours-minutes-seconds",
        /// `23:45`, and `83:45` once it passes an hour — minutes keep counting
        /// rather than rolling into a field that is not being shown.
        MinutesSeconds => "minutes-seconds",
    }
}

/// One Text Source's stopwatch.
///
/// Two fields rather than one instant, because a stopwatch that can be
/// stopped has to remember what it already counted: `running_since` is the
/// current run and `accumulated` is every run before it.
///
/// The instant is a wall-clock time and not an [`std::time::Instant`],
/// because it is written to the project file and read back on the next run —
/// a monotonic clock has no meaning across a restart. The cost is that
/// setting the system clock while a timer runs moves the figure, which is
/// the same bargain every stopwatch that survives a reboot makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TextTimer {
    /// Unix microseconds of the moment the current run began, or `None`
    /// while the timer is stopped.
    pub running_since: Option<i64>,
    /// What every completed run added up to.
    pub accumulated: Duration,
}

impl TextTimer {
    pub fn running(&self) -> bool {
        self.running_since.is_some()
    }

    /// What the timer reads, against a wall clock in unix microseconds.
    ///
    /// Saturating rather than wrapping: a system clock moved backwards past
    /// the start of the run would otherwise read as an enormous duration
    /// instead of standing still.
    pub fn elapsed(&self, now: i64) -> Duration {
        let Some(since) = self.running_since else {
            return self.accumulated;
        };
        self.accumulated + Duration::from_micros(now.saturating_sub(since).max(0) as u64)
    }
}

/// A line of text, and how to draw it.
///
/// Kept as a string and a style rather than as pixels, for the reason
/// [`DrawingSourceSettings`] keeps strokes: rasterizing is the engine's job
/// and happens once per change, so the file stays small and the glyphs stay
/// sharp at whatever size the box turns out to be.
#[derive(Debug, Clone, PartialEq)]
pub struct TextSourceSettings {
    /// The box glyphs are drawn into, which is also the layer's own size.
    ///
    /// Fixed while the Source runs — the upload element refuses a frame of a
    /// different size — so changing it reopens the Source, exactly as a
    /// Drawing's surface does.
    pub size: [f32; 2],
    /// What [`TextMode::Static`] shows. Kept while another mode is selected
    /// rather than repurposed as its format, so switching to a clock and
    /// back does not lose what was typed.
    pub text: String,
    pub mode: TextMode,
    pub clock_format: ClockFormat,
    pub timer_format: TimerFormat,
    pub timer: TextTimer,
    /// The font file to draw with, or `None` for the one this application
    /// already found for its own interface — see `crate::i18n::font`.
    ///
    /// A path rather than the bytes: a project file that carried a font in it
    /// would be a licence question, and the file is the same one every time
    /// the project opens.
    pub font: Option<PathBuf>,
    /// Pixel height of the glyphs, in the box's own coordinates.
    pub font_size: f32,
    pub rgba: [u8; 4],
    pub alignment: TextAlignment,
}

/// One continuous mark, from the pointer going down to it coming up.
///
/// Points are in the Drawing's own coordinates, not the Canvas's — the
/// SceneItem's Transform is undone before a point is recorded, so moving or
/// resizing the source afterwards carries its marks with it instead of
/// leaving them where the pointer happened to be.
#[derive(Debug, Clone, PartialEq)]
pub struct Stroke {
    /// Straight segments between consecutive points. A single point is a dot,
    /// which is what a click without a drag draws.
    pub points: Vec<[f32; 2]>,
    pub rgba: [u8; 4],
    /// Line width in the Drawing's own coordinates, so it scales with the
    /// source the same way its marks do.
    pub width: f32,
}

/// A surface to draw on, kept as the marks that were made rather than as
/// pixels.
///
/// Strokes rather than an image because everything this needs falls out of
/// it: the eraser takes whole strokes away, undo pops one, the file stays
/// small, and redrawing at a different size stays sharp. Rasterizing is the
/// engine's job and happens once per change.
#[derive(Debug, Clone, PartialEq)]
pub struct DrawingSourceSettings {
    /// The surface's own size, which is what strokes are positioned within.
    pub size: [f32; 2],
    pub strokes: Vec<Stroke>,
}

/// Which display a Display Capture source captures.
///
/// The two forms are not interchangeable and neither platform can produce the
/// other. Windows and X11 hand out a stable display name, and the capture layer
/// resolves it against whatever display layout is live at the time. Wayland
/// never names a display at all: `xdg-desktop-portal` owns the picker, and the
/// only thing that reproduces an earlier selection is the opaque restore token
/// it issues.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DisplayCaptureTarget {
    /// A stable display name such as `\\.\DISPLAY1` or `DP-1`.
    MonitorName(String),
    /// A selection made in the desktop portal's own picker.
    ///
    /// `restore_token` is `None` when the compositor declined to persist the
    /// selection. That is not an error: starting capture then shows the picker
    /// again instead of restoring silently, which is the portal's design.
    Portal { restore_token: Option<String> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayCaptureSettings {
    pub target: DisplayCaptureTarget,
    /// The display's pixel size as the picker reported it, or `None` when it
    /// reported none.
    ///
    /// A hint, not a fact. The display layout can change between runs, and a
    /// compositor may scale a Wayland stream to a size the portal never named,
    /// so this is not authoritative and is never resolved against. It exists so
    /// a new SceneItem starts at the display's own shape instead of standing in
    /// at Canvas size; the capture layer replaces it with the stream's
    /// negotiated size once the Source actually opens.
    pub size_hint: Option<[u32; 2]>,
}

/// Which window a Window Capture reproduces.
///
/// The same two-formed problem a display has, and worse. Windows hands out an
/// `HWND`, but one is only meaningful inside the session that issued it — it
/// is recycled, and the window is gone the moment its application closes. So
/// what is stored is the pair a person would use to find the window again:
/// the owning executable and the title it had. The capture resolves that
/// against whatever is on screen at the time, exactly as a display name is
/// resolved against the live layout.
///
/// Wayland names nothing here either. The portal's picker owns the choice and
/// the restore token is all that reproduces it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowCaptureTarget {
    /// The owning executable's file name and the window's title.
    ///
    /// Neither alone is enough: a title is often empty or duplicated across
    /// an application's windows, and a process usually has more than one.
    /// Together they are what a person reads off a task bar, which is the
    /// standard this can be held to — not uniqueness, which no pair of these
    /// can promise.
    Window { process: String, title: String },
    /// A selection made in the desktop portal's own picker.
    ///
    /// `restore_token` is `None` when the compositor declined to persist the
    /// selection, the same as for a display.
    Portal { restore_token: Option<String> },
}

impl WindowCaptureTarget {
    /// Whether the window behind this can be looked for again without
    /// interrupting anyone.
    ///
    /// A stored `{program, title}` is searched against the live window list,
    /// which costs nothing and asks no one — so a window that closed and came
    /// back is simply found again. A portal selection is not searchable at
    /// all: the portal owns the picker, a closed window's restore token is
    /// dead, and there is no way to ask whether one is still good without
    /// starting the flow that puts a dialog on screen.
    ///
    /// So the engine only goes looking for the first kind. The second is left
    /// where it is until someone asks for it, because looking *is* the
    /// interruption.
    pub fn can_be_reopened_silently(&self) -> bool {
        matches!(self, Self::Window { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowCaptureSettings {
    pub target: WindowCaptureTarget,
    /// The window's outer size when it was picked, or `None` when nothing
    /// reported one.
    ///
    /// A hint, and a weaker one than a display's: a window is resized by the
    /// person using it, so this is only ever what it was at the moment it was
    /// chosen. It gives a new SceneItem a shape to start at, and the capture
    /// layer replaces it with what the stream actually negotiates.
    pub size_hint: Option<[u32; 2]>,
}

/// A video file played into the Scene.
///
/// The path is what was picked and is not resolved to anything else: a file
/// that has been moved or a drive that is not mounted is an ordinary state
/// the same way a closed window is, and the Source waits for it rather than
/// being an error. What is stored is therefore the path itself, not a handle
/// or an id that would stop meaning anything outside this session.
#[derive(Debug, Clone, PartialEq)]
pub struct MediaFileSettings {
    pub path: PathBuf,
    /// Whether reaching the end of the file starts it again instead of
    /// leaving the Scene showing its last frame.
    ///
    /// Switchable while it plays, and switching it off does not rewind: the
    /// lap that is running plays out and then stops. See
    /// `media_pp::elements::FileDemuxerHandle`.
    pub looping: bool,
    /// The video's pixel size as the file reported it when it was picked, or
    /// `None` when it could not be read.
    ///
    /// A hint, and a stronger one than a display's: a file's frames do not
    /// change size between runs. It is still not resolved against — a file
    /// can be replaced on disk — so it only decides what shape a new
    /// SceneItem starts at.
    pub size_hint: Option<[u32; 2]>,
    /// Whether the file had a sound track when it was picked.
    ///
    /// What the Audio Mixer dock draws a channel from, so it is stored rather
    /// than asked of the running Source: the dock has to know before anything
    /// is open, and a Scene the user is not looking at has nothing running at
    /// all. A hint like the size, and wrong for the same reason — a file can
    /// be replaced on disk — which costs a channel that moves nothing.
    pub has_audio: bool,
    /// Gain in decibels, where `0.0` is unchanged, matching every other
    /// fader in this application. See [`crate::domain::MIN_GAIN_DB`].
    pub gain_db: f32,
    /// How long the file is, as it reported when it was picked.
    ///
    /// What a progress bar is drawn against, so it is stored rather than
    /// asked of the running Source: the bar has to have a length before
    /// anything is open, and a Scene the user is not looking at has nothing
    /// running at all. `None` for a file that would not say — a bar then has
    /// no scale and shows the position alone.
    pub duration: Option<Duration>,
    /// Whether playback is stopped where it is.
    ///
    /// Stored, so it survives the Scene changing under it and the
    /// application being restarted — a paused clip that started itself again
    /// on the next launch would be a surprise. Hiding the SceneItem also
    /// stops it, and that is not recorded here for the same reason muting
    /// is not: one state with two effects rather than two to keep in step.
    pub paused: bool,
    /// Whether this file's sound is muted.
    ///
    /// Only what the mute button set. Hiding the SceneItem also silences it,
    /// but that is not recorded here: hiding is one state with two effects,
    /// not two states to keep in step, and unhiding must not have to remember
    /// what the mute was before.
    pub muted: bool,
    /// Whether this file is played back to the person running obs-rs.
    ///
    /// It matters more here than on a device channel: a file's sound exists
    /// only inside obs-rs, so with this off there is no way to hear it at
    /// all. See [`crate::domain::AudioSource::monitored`].
    pub monitored: bool,
}

/// A still picture placed in the Scene.
///
/// The same shape as a media file's settings minus everything that moves:
/// one path, stored as it was picked, and the size it was read at. There is
/// nothing to loop, fade or mute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageSourceSettings {
    pub path: PathBuf,
    /// The picture's pixel size when it was picked, or `None` when it could
    /// not be read. A hint like a media file's, and wrong for the same
    /// reason — a file can be replaced on disk.
    pub size_hint: Option<[u32; 2]>,
}

/// How the video is carried once an RTSP session is negotiated.
///
/// The same two `media_pp::elements::RtspTransport` offers, mirrored here so
/// the project's own types do not depend on the pipeline library's — this is
/// stored in the database and read by the UI, neither of which should have to
/// know what the element takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RtspTransport {
    /// Interleaved in the control connection. The default because it is the
    /// one that crosses a firewall or a NAT without being arranged for.
    #[default]
    Tcp,
    /// Separate RTP and RTCP ports, negotiated with the server. Lower latency
    /// on a network you control, and nothing at all on one where those ports
    /// do not get through — which is why it is a choice rather than a guess.
    Udp,
}

impl RtspTransport {
    pub(crate) fn storage_name(self) -> &'static str {
        match self {
            Self::Tcp => "tcp",
            Self::Udp => "udp",
        }
    }

    pub(crate) fn from_storage_name(name: &str) -> Option<Self> {
        match name {
            "tcp" => Some(Self::Tcp),
            "udp" => Some(Self::Udp),
            _ => None,
        }
    }
}

/// A live stream, and what to do when it stops arriving.
///
/// The URL is stored as it was typed and never resolved to anything else. A
/// camera that is switched off, rebooting, or behind a network that is down
/// is an ordinary state rather than an error — the same standing a closed
/// window has — so the Source waits for it instead of failing.
#[derive(Debug, Clone, PartialEq)]
pub struct RtspSourceSettings {
    pub url: String,
    pub transport: RtspTransport,
    /// How long to wait before connecting again after the stream stops, or
    /// `None` to leave it to the user.
    ///
    /// A dropped stream is what a camera does when it reboots, so trying
    /// again by itself is usually what is wanted. `None` is for the case
    /// where it is not — a connection that is metered, or a camera that is
    /// only occasionally on — and then the Sources dock offers it the way it
    /// offers a disconnected window capture.
    pub reconnect: Option<Duration>,
    /// The video's pixel size as the stream reported it when it was added, or
    /// `None` when nothing could be read — an address that was not answering
    /// yet still becomes a Source.
    pub size_hint: Option<[u32; 2]>,
    /// Whether the stream announced a sound track when it was added. What the
    /// Audio Mixer draws a channel from, on the same terms as a media file's.
    pub has_audio: bool,
    pub gain_db: f32,
    pub muted: bool,
}

/// One picture shape a camera offers, as it is stored and shown.
///
/// Mirrors `media_pp::elements::MfCaptureFormat` rather than reusing it, for
/// the reason [`RtspTransport`] is mirrored: this is written to the database
/// and drawn by the UI, neither of which should have to know what the capture
/// element takes. The rate is kept as the fraction the camera stated —
/// `30000/1001` is a real mode and is not `30/1`, so rounding it here would
/// stop it matching what the device offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoCaptureMode {
    pub width: u32,
    pub height: u32,
    pub framerate_numerator: u32,
    pub framerate_denominator: u32,
}

impl VideoCaptureMode {
    /// Frames per second as a number to show, which is all this is for. Two
    /// modes are compared by their stored fraction, never by this.
    pub fn framerate(self) -> f32 {
        if self.framerate_denominator == 0 {
            return 0.0;
        }
        self.framerate_numerator as f32 / self.framerate_denominator as f32
    }
}

/// A camera played into the Scene.
///
/// # Not attached is not failure
///
/// The device is stored as the symbolic link it was picked by and is never
/// resolved to anything else. A camera that is unplugged, switched off, or
/// held by another application is an ordinary state the same way a closed
/// window is: the Source waits for it and is opened again when it comes back.
/// The name is stored beside the link so a camera that is *not* there can
/// still be shown as itself rather than as a device path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoCaptureSettings {
    /// The device's symbolic link, which is the only identity that survives a
    /// restart. Enumeration order is not one: unplugging one camera renumbers
    /// the rest.
    pub device: String,
    /// The name the device had when it was picked, for the dock and the
    /// properties panel to show while it is not attached.
    pub device_name: String,
    /// The mode to ask the camera for, or `None` to take whichever it offers
    /// first. Stored rather than resolved for the same reason the link is: a
    /// camera that is not attached right now still has a mode that was chosen
    /// for it.
    pub mode: Option<VideoCaptureMode>,
    /// The negotiated picture size when the camera was added, or `None` where
    /// none was read. A hint like a display's — the mode can be changed, and
    /// the capture layer replaces this with what it actually opened.
    pub size_hint: Option<[u32; 2]>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SourceSettings {
    Color(ColorSourceSettings),
    Rtsp(RtspSourceSettings),
    VideoCapture(VideoCaptureSettings),
    Drawing(DrawingSourceSettings),
    DisplayCapture(DisplayCaptureSettings),
    WindowCapture(WindowCaptureSettings),
    MediaFile(MediaFileSettings),
    Image(ImageSourceSettings),
    Text(TextSourceSettings),
}

impl SourceSettings {
    /// The Source's own size in Canvas units, before the SceneItem's own
    /// Transform scales it.
    ///
    /// A Color Source carries its size directly. A capture source has none
    /// until the capture layer opens it and reports one, so until then it
    /// stands in at Canvas size rather than having no size at all: an item
    /// with no rectangle cannot be selected, moved, or resized, and the editor
    /// has to work before any frame exists.
    /// The size that was *stored* for this Source, as opposed to the one
    /// [`SourceSettings::source_size`] falls back to.
    ///
    /// `None` where none was ever read — a capture added while its target was
    /// not there — and for the kinds that carry their own size rather than a
    /// hint. What this answers is "does the project already know", which is a
    /// different question from "what shape is the item".
    pub fn size_hint(&self) -> Option<[u32; 2]> {
        match self {
            Self::DisplayCapture(settings) => settings.size_hint,
            Self::WindowCapture(settings) => settings.size_hint,
            Self::VideoCapture(settings) => settings.size_hint,
            Self::MediaFile(settings) => settings.size_hint,
            Self::Rtsp(settings) => settings.size_hint,
            Self::Image(settings) => settings.size_hint,
            Self::Color(_) | Self::Drawing(_) | Self::Text(_) => None,
        }
    }

    pub fn source_size(&self, canvas: SceneCanvas) -> [f32; 2] {
        match self {
            Self::Color(settings) => settings.size,
            Self::Drawing(settings) => settings.size,
            Self::Text(settings) => settings.size,
            Self::DisplayCapture(settings) => settings
                .size_hint
                .map_or([canvas.width, canvas.height], |[width, height]| {
                    [width as f32, height as f32]
                }),
            Self::WindowCapture(settings) => settings
                .size_hint
                .map_or([canvas.width, canvas.height], |[width, height]| {
                    [width as f32, height as f32]
                }),
            Self::MediaFile(settings) => settings
                .size_hint
                .map_or([canvas.width, canvas.height], |[width, height]| {
                    [width as f32, height as f32]
                }),
            Self::Rtsp(settings) => settings
                .size_hint
                .map_or([canvas.width, canvas.height], |[width, height]| {
                    [width as f32, height as f32]
                }),
            Self::VideoCapture(settings) => settings
                .size_hint
                .map_or([canvas.width, canvas.height], |[width, height]| {
                    [width as f32, height as f32]
                }),
            Self::Image(settings) => settings
                .size_hint
                .map_or([canvas.width, canvas.height], |[width, height]| {
                    [width as f32, height as f32]
                }),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Source {
    pub id: SourceId,
    pub name: String,
    pub kind: SourceKind,
    pub settings: SourceSettings,
    /// What is done to this Source's picture before it reaches the Canvas,
    /// in the order it is done — see [`Filter`](super::Filter).
    ///
    /// On the Source and not on the SceneItem, so a camera used in two
    /// Scenes is keyed in both. Empty for every kind and every Source that
    /// has never been given one, which is most of them.
    pub filters: Vec<Filter>,
    /// What is done to this Source's own sound before its fader, in order —
    /// see [`AudioFilterOwner::Source`](super::AudioFilterOwner::Source).
    /// Empty for every kind without sound.
    pub audio_filters: Vec<super::AudioFilter>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every kind survives being written to the project file and read back.
    ///
    /// The other half of this — that `ALL` lists every kind, which is what
    /// the Add Source dialog is built from — needs no test any more:
    /// `stored_by_name!` writes the enum and the list from one declaration,
    /// so a kind that exists is a kind in `ALL`. What is left to check is
    /// that no two kinds are stored under the same name, which the macro
    /// cannot see: duplicate names compile, and the second kind then reads
    /// back as the first.
    #[test]
    fn every_source_kind_survives_a_round_trip_through_storage() {
        let mut seen = std::collections::HashSet::new();
        for kind in SourceKind::ALL {
            assert!(
                seen.insert(kind.storage_name()),
                "{kind:?} shares its storage name with another kind"
            );
            assert_eq!(
                SourceKind::from_storage_name(kind.storage_name()),
                Some(kind)
            );
        }
        assert_eq!(SourceKind::from_storage_name("nothing-like-this"), None);
    }

    /// And the same for the four the Text Source stores.
    #[test]
    fn every_text_setting_survives_a_round_trip_through_storage() {
        for alignment in TextAlignment::ALL {
            assert_eq!(
                TextAlignment::from_storage_name(alignment.storage_name()),
                Some(alignment)
            );
        }
        for mode in TextMode::ALL {
            assert_eq!(TextMode::from_storage_name(mode.storage_name()), Some(mode));
        }
        for format in ClockFormat::ALL {
            assert_eq!(
                ClockFormat::from_storage_name(format.storage_name()),
                Some(format)
            );
        }
        for format in TimerFormat::ALL {
            assert_eq!(
                TimerFormat::from_storage_name(format.storage_name()),
                Some(format)
            );
        }
    }
}
