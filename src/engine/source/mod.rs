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
pub(in crate::engine) mod image;
pub(in crate::engine) mod media_file;
pub(in crate::engine) mod rtsp;
pub(in crate::engine) mod sound;
pub(in crate::engine) mod video_capture;
pub(in crate::engine) mod window_capture;

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU32};

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
    /// A picture, by the path it was decoded from. Nothing changes it today —
    /// an Image Source is opened with one file and keeps it — so this is what
    /// keeps a Scene change from decoding and uploading it again.
    Image(std::path::PathBuf),
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
    /// Whether the Source is running, which is no longer the same question.
    /// A media file can be paused while its item is in the Scene, so what
    /// decides this is [`paused`] rather than `showing` alone.
    pub(in crate::engine) running: bool,
    /// Set for a Source this side pushes frames into — see [`PushedSurface`].
    pub(in crate::engine) pushed: Option<PushedSurface>,
    /// Set for a media file Source — see [`MediaFile`].
    pub(in crate::engine) media_file: Option<MediaFile>,
}

/// The part of a media file Source that can be changed while it plays.
///
/// One switch so far, and it is the reason this exists at all: turning
/// looping on or off must not restart what is playing, so it is a handle call
/// rather than a reopen. Everything else about a media file — which file,
/// where it sits — is either fixed for the life of the Source or belongs to
/// the SceneItem rather than to it.
pub(in crate::engine) struct MediaFile {
    /// A file's own end-of-file behaviour, and `None` for a live stream —
    /// which fills this struct for its sound and its meter and has no
    /// timeline to loop. The name is the file's because a file is what it
    /// was written for; what the two share is that they carry their own
    /// sound, which is the half a stream uses.
    pub(in crate::engine) looping: Option<media_pp::elements::FileDemuxerHandle>,
    /// The file's own fader, and `None` for a file with no sound — or one
    /// opened on a machine whose mixer never started, which is the same
    /// thing from here: there is nothing to turn down.
    pub(in crate::engine) volume: Option<media_pp::elements::AudioVolumeHandle>,
    /// What the dock's meter and its progress bar read.
    pub(in crate::engine) meters: Arc<MediaMeters>,
    /// This Source's own pipeline, kept for the one thing `RunningSource`
    /// cannot do: seek. Every other control it has goes through that, so
    /// there is one path for pausing and one for repositioning rather than
    /// two for either.
    pub(in crate::engine) pipeline: Arc<media_pp::pipeline::Pipeline>,
    /// Which mixes this Source's own sound is in, and what puts it in them.
    ///
    /// `None` for one with no sound. Unlike the fields above this is
    /// *changed* from the engine loop rather than only read — see
    /// [`refresh_media_file`].
    pub(in crate::engine) sound: Option<sound::SoundRouting>,
}

/// What a running media file measures about itself, written by whichever
/// thread its own branches push on and read by the UI thread.
///
/// Two numbers with one lifetime — both start when the Source opens and stop
/// when it closes — so they are published as one thing rather than as two
/// maps that would always have the same keys. Atomics rather than a lock for
/// the same reason `audio::Levels` uses them: a reading one frame stale is a
/// reading that is right a frame later, and nothing here is ordered against
/// anything else.
#[derive(Default)]
pub(in crate::engine) struct MediaMeters {
    /// The loudest sample since the last read, as `f32` bits. Zero until the
    /// audio branch has measured one, which is also what a file with no
    /// sound looks like.
    pub(in crate::engine) peak: AtomicU32,
    /// Where playback is *in the file*, in microseconds — the frame's own
    /// timestamp with the loop's accumulated offset taken back off, so a
    /// second lap reads from the start again rather than from the end of the
    /// first. Negative until the first frame arrives.
    pub(in crate::engine) position: AtomicI64,
}

/// The name a SceneItem's compositor input is registered under.
#[allow(dead_code)]
pub(in crate::engine) fn input_name(item: &SceneItemSnapshot) -> String {
    format!("scene-item-{}", item.id.0)
}

/// Convenience for a backend that has no Source of a given kind yet.
///
/// Unused on Windows, where the D3D11 backend now opens every kind there is —
/// which is why its own match has no fallback arm any more, and why adding a
/// ninth kind will stop that build until somebody decides what it does.
#[allow(dead_code)]
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
/// Tells a running media file Source what its settings now say.
///
/// No comparison against what was last set, unlike [`refresh_pushed`]: what
/// that guards is a redraw and a re-upload, and this is a single atomic
/// store. There is nothing here that would be cheaper to skip than to do.
pub(in crate::engine) fn refresh_media_file(
    source: &mut OpenSource,
    item: &SceneItemSnapshot,
    monitor: Option<&media_pp::elements::MixerHandle>,
) {
    use crate::domain::SourceSettings;

    let Some(media) = &mut source.media_file else {
        return;
    };

    // Whether this Source's sound is played back, asked of every Source that
    // has one rather than of media files alone. A live stream has no such
    // setting yet and answers `false`, which is where it has always been.
    if let Some(routing) = &mut media.sound {
        let monitored = match &item.settings {
            SourceSettings::MediaFile(settings) => settings.monitored,
            _ => false,
        };
        routing.apply(
            crate::engine::audio::monitors(monitored, monitor.is_some()),
            monitor,
        );
    }

    let SourceSettings::MediaFile(settings) = &item.settings else {
        return;
    };
    if let Some(looping) = &media.looping {
        looping.set_looping(settings.looping);
    }
    if let Some(volume) = &media.volume {
        let _ = volume.set_gain_db(settings.gain_db);
        volume.set_muted(muted(settings.muted, item.visible));
    }
}

/// Whether this media file is stopped, from the two things that can stop it.
///
/// Hiding the SceneItem stops it as well as taking it out of the picture, for
/// the same reason hiding mutes it: a Source that is not in the Scene has no
/// business playing on, and unhiding must not have to remember what the pause
/// button was before.
pub(in crate::engine) fn paused(item: &SceneItemSnapshot, showing: bool) -> bool {
    use crate::domain::SourceSettings;

    !showing || matches!(&item.settings, SourceSettings::MediaFile(settings) if settings.paused)
}

/// Whether this file's sound is off, from the two things that can turn it off.
///
/// Hiding the SceneItem silences it. One state with two effects rather than
/// two states to keep in step: unhiding must not have to remember what the
/// mute button was before, and a Source that is not in the picture has no
/// channel in the Audio Mixer dock to unmute it from either.
pub(in crate::engine) fn muted(muted: bool, visible: bool) -> bool {
    muted || !visible
}

/// One media file Source's gain, while the fader is still held.
///
/// Straight to the handle, which is what makes it audible under the pointer;
/// the project hears the same value once, when the gesture ends, and
/// [`refresh_media_file`] then sets it again to no effect.
pub(in crate::engine) fn set_media_gain_db(source: &OpenSource, gain_db: f32) {
    if let Some(media) = &source.media_file
        && let Some(volume) = &media.volume
    {
        let _ = volume.set_gain_db(gain_db);
    }
}

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
        // Nothing asks for this. An Image Source is opened with one file and
        // keeps it, so `refresh_pushed` never names one here — the arm exists
        // because the content is compared like every other kind's, not
        // because a picture is ever pushed twice.
        PushedContent::Image(_) => return,
    };
    if let Err(error) = surface.pusher.push(frame) {
        eprintln!("could not update \"{}\": {error}", source.name);
        return;
    }
    surface.content = wanted;
}
