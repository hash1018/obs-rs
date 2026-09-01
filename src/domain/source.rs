use std::path::PathBuf;

use super::SceneCanvas;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SourceId(pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    DisplayCapture,
    WindowCapture,
    VideoCapture,
    MediaFile,
    Image,
    Color,
    Drawing,
}

impl SourceKind {
    pub(crate) fn storage_name(self) -> &'static str {
        match self {
            Self::DisplayCapture => "display_capture",
            Self::WindowCapture => "window_capture",
            Self::VideoCapture => "video_capture",
            Self::MediaFile => "media_file",
            Self::Image => "image",
            Self::Color => "color",
            Self::Drawing => "drawing",
        }
    }

    pub(crate) fn from_storage_name(name: &str) -> Option<Self> {
        match name {
            "display_capture" => Some(Self::DisplayCapture),
            "window_capture" => Some(Self::WindowCapture),
            "video_capture" => Some(Self::VideoCapture),
            "media_file" => Some(Self::MediaFile),
            "image" => Some(Self::Image),
            "color" => Some(Self::Color),
            "drawing" => Some(Self::Drawing),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColorSourceSettings {
    pub size: [f32; 2],
    pub rgba: [u8; 4],
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
    /// Whether this file's sound is muted.
    ///
    /// Only what the mute button set. Hiding the SceneItem also silences it,
    /// but that is not recorded here: hiding is one state with two effects,
    /// not two states to keep in step, and unhiding must not have to remember
    /// what the mute was before.
    pub muted: bool,
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

#[derive(Debug, Clone, PartialEq)]
pub enum SourceSettings {
    Color(ColorSourceSettings),
    Drawing(DrawingSourceSettings),
    DisplayCapture(DisplayCaptureSettings),
    WindowCapture(WindowCaptureSettings),
    MediaFile(MediaFileSettings),
    Image(ImageSourceSettings),
    None,
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
    pub fn source_size(&self, canvas: SceneCanvas) -> [f32; 2] {
        match self {
            Self::Color(settings) => settings.size,
            Self::Drawing(settings) => settings.size,
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
            Self::Image(settings) => settings
                .size_hint
                .map_or([canvas.width, canvas.height], |[width, height]| {
                    [width as f32, height as f32]
                }),
            Self::None => [canvas.width, canvas.height],
        }
    }
}

#[derive(Debug, Clone)]
pub struct Source {
    pub id: SourceId,
    pub name: String,
    pub kind: SourceKind,
    pub settings: SourceSettings,
}
