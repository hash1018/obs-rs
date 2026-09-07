/// Declares an enum whose variants are stored in the project file by name.
///
/// Four lists become one. Written out by hand, such an enum is its variants,
/// the `ALL` array something iterates, the name each variant is written as,
/// and the match that reads those names back — and every one of the four has
/// to be extended together, by whoever remembers to.
///
/// Three of the four announce a mistake: the compiler refuses an unfinished
/// `match`, and a round-trip test catches a name that reads back as a
/// different variant. `ALL` is the one that says nothing. A variant missing
/// from it compiles, passes its round trip, and is simply absent from
/// whatever the list is for — which for `SourceKind` means a kind of Source
/// nobody can add, with no error anywhere.
///
/// So the macro writes all four from one list, and there is no way to add a
/// variant to some of them.
macro_rules! stored_by_name {
    (
        $(#[$enum_meta:meta])*
        $vis:vis enum $name:ident {
            $($(#[$variant_meta:meta])* $variant:ident => $stored:literal),* $(,)?
        }
    ) => {
        $(#[$enum_meta])*
        $vis enum $name {
            $($(#[$variant_meta])* $variant,)*
        }

        impl $name {
            /// Every variant there is, in declaration order.
            pub const ALL: [Self; [$(stringify!($variant)),*].len()] =
                [$(Self::$variant,)*];

            /// What this variant is written as in the project file.
            pub(crate) fn storage_name(self) -> &'static str {
                match self {
                    $(Self::$variant => $stored,)*
                }
            }

            /// The variant one of those names stands for, or `None` for a
            /// name this build does not know — which is what a project
            /// written by a newer one looks like.
            pub(crate) fn from_storage_name(name: &str) -> Option<Self> {
                match name {
                    $($stored => Some(Self::$variant),)*
                    _ => None,
                }
            }
        }
    };
}

mod audio;
mod canvas;
mod filter;
mod scene;
mod scene_item;
mod source;

pub use audio::{AudioSource, AudioSourceId, AudioSourceKind, MAX_GAIN_DB, MIN_GAIN_DB};
pub use canvas::SceneCanvas;
pub use filter::{
    ChromaKeyMethod, ChromaKeySettings, Filter, FilterId, FilterKind, FilterSettings,
};
pub use scene::{Scene, SceneId};
pub use scene_item::{Crop, SceneItem, SceneItemId, Transform};
pub use source::{
    ClockFormat, ColorSourceSettings, DEFAULT_FONT_SIZE, DisplayCaptureSettings,
    DisplayCaptureTarget, DrawingSourceSettings, ImageSourceSettings, MediaFileSettings,
    RtspSourceSettings, RtspTransport, Source, SourceId, SourceKind, SourceSettings, Stroke,
    TextAlignment, TextMode, TextSourceSettings, TextTimer, TimerFormat, VideoCaptureMode,
    VideoCaptureSettings, WindowCaptureSettings, WindowCaptureTarget,
};
