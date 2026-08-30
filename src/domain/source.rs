use super::SceneCanvas;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SourceId(pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    DisplayCapture,
    WindowCapture,
    VideoCapture,
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

#[derive(Debug, Clone, PartialEq)]
pub enum SourceSettings {
    Color(ColorSourceSettings),
    Drawing(DrawingSourceSettings),
    DisplayCapture(DisplayCaptureSettings),
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
