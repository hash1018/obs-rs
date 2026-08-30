//! What attaches to the compositor: a Source, and the parts of one that are
//! the same on every backend.
//!
//! Opening a Source needs a device, so the per-platform half of each kind
//! lives beside it here under a `#[cfg]` rather than in the backend module —
//! the two implementations of a Color Source differ by which upload element
//! carries its frame, and reading them side by side is worth more than
//! keeping each next to its own device.
//!
//! [`display_capture`](super) is the exception this does not cover yet: it is
//! two genuinely unlike implementations and keeps its own directory.

pub(in crate::engine) mod color;
pub(in crate::engine) mod display_capture;
pub(in crate::engine) mod drawing;
pub(in crate::engine) mod window_capture;

use crate::snapshots::SceneItemSnapshot;

use super::backend::{BackendError, Layer, RunningSource};

/// A Source whose pixels this side produces, and its way back to the
/// compositor.
///
/// Kept for two reasons, and the first is not optional. An `AppSource` runs
/// only while a handle to it exists: drop the last one and it sends `Eos` and
/// finishes, and its compositor input takes that as the end of the layer. A
/// Color Source pushed its one frame and dropped its handle in the same
/// breath, so it composited nothing at all — a Drawing worked only because
/// this held its handle for the second reason.
///
/// That second reason is pushing again. A Drawing has a list of strokes and a
/// Color a colour, and either can change without the Source being reopened.
pub(in crate::engine) struct PushedSurface {
    pub(in crate::engine) pusher: media_pp::elements::AppSourceHandle,
    pub(in crate::engine) size: [u32; 2],
    /// What was last pushed. A Scene change that left it alone — a move, a
    /// rename, anything else in the Scene at all — must not cost a redraw and
    /// a re-upload.
    pub(in crate::engine) content: PushedContent,
}

/// What a [`PushedSurface`] last put on the compositor.
#[derive(PartialEq)]
pub(in crate::engine) enum PushedContent {
    Color([u8; 4]),
    Drawing(Vec<crate::domain::Stroke>),
}

/// A Source that is running, and the controls for its layer.
pub(in crate::engine) struct OpenSource {
    pub(in crate::engine) source: RunningSource,
    pub(in crate::engine) layer: Layer,
    pub(in crate::engine) name: String,
    /// The token the portal handed back, when it differs from the one it was
    /// given. `None` means the stored token is still current.
    pub(in crate::engine) refreshed_token: Option<Option<String>>,
    /// Whether the Source is in the Scene being shown. One whose item left the
    /// Scene stays open but stops running, so coming back is a resume rather
    /// than another portal round trip.
    pub(in crate::engine) showing: bool,
    /// Set for a Source this side pushes frames into — see [`PushedSurface`].
    pub(in crate::engine) pushed: Option<PushedSurface>,
}

/// The name a SceneItem's compositor input is registered under.
#[allow(dead_code)]
pub(in crate::engine) fn input_name(item: &SceneItemSnapshot) -> String {
    format!("scene-item-{}", item.id.0)
}

/// Convenience for a backend that has no Source of a given kind yet.
pub(in crate::engine) fn unsupported_kind(item: &SceneItemSnapshot) -> BackendError {
    format!("{:?} is not connected to the compositor yet", item.kind).into()
}

/// Puts a Source's own pixels on the compositor again, when what it should be
/// showing has changed.
///
/// One function for both kinds this side produces. It runs on every reconcile
/// pass, which is every Scene change, so the comparison inside is what keeps a
/// move or a rename from costing a redraw and a re-upload of something nobody
/// touched.
pub(in crate::engine) fn refresh_pushed(source: &mut OpenSource, item: &SceneItemSnapshot) {
    use crate::domain::SourceSettings;

    let wanted = match &item.settings {
        SourceSettings::Color(settings) => PushedContent::Color(settings.rgba),
        SourceSettings::Drawing(settings) => PushedContent::Drawing(settings.strokes.clone()),
        _ => return,
    };
    push_content(source, wanted);
}

/// The push itself, which the mid-gesture drawing path needs on its own: it
/// has the strokes in hand and no snapshot to read them back out of, because
/// the project has not been told about them yet.
pub(in crate::engine) fn push_content(source: &mut OpenSource, wanted: PushedContent) {
    let Some(surface) = source.pushed.as_mut() else {
        return;
    };
    if surface.content == wanted {
        return;
    }
    let [width, height] = surface.size;
    let frame = match &wanted {
        PushedContent::Color(rgba) => color::flat_bgra(width, height, *rgba),
        PushedContent::Drawing(strokes) => drawing::drawing_bgra(width, height, strokes),
    };
    if let Err(error) = surface.pusher.push(frame) {
        eprintln!("could not update \"{}\": {error}", source.name);
        return;
    }
    surface.content = wanted;
}
