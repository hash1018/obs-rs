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
pub(in crate::engine) mod filters;
pub(in crate::engine) mod image;
pub(in crate::engine) mod media_file;
pub(in crate::engine) mod rtsp;
pub(in crate::engine) mod sound;
pub(in crate::engine) mod text;
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
    /// The frame that was, for pushing again once its filters change — see
    /// [`repush`]. The same buffer rather than a redraw of it, which is what
    /// lets everything on the way recognise it as the picture it already
    /// handled.
    pub(in crate::engine) frame: media_pp::buffer::MediaBuffer,
}

impl PushedSurface {
    /// Pushes `frame` and remembers it as this surface's picture.
    pub(in crate::engine) fn push(
        &mut self,
        frame: media_pp::buffer::MediaBuffer,
    ) -> media_pp::error::Result<()> {
        self.pusher.push(frame.clone())?;
        self.frame = frame;
        Ok(())
    }
}

/// Puts a pushed Source's picture through its filters again.
///
/// What a filter change needs on a Source that sends nothing until it is
/// edited — see the `filters` module. A no-op for every other kind, whose
/// next frame is already on its way.
pub(in crate::engine) fn repush(source: &OpenSource) {
    if let Some(surface) = &source.pushed
        && let Err(error) = surface.pusher.push(surface.frame.clone())
    {
        eprintln!("could not refilter \"{}\": {error}", source.name);
    }
}

/// Opens a rack for a Source and fills it from the item's filters, so the
/// first frame is already filtered: a rack picks its contents up on the next
/// buffer, and there is not one yet.
#[cfg(target_os = "windows")]
pub(in crate::engine) fn filled_rack(
    name: &str,
    device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
    context: Arc<std::sync::Mutex<windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext>>,
    incoming: filters::ChainFormat,
    [width, height]: [u32; 2],
    item: &SceneItemSnapshot,
) -> Result<FilledRack, BackendError> {
    let (rack, filter_rack) = filters::rack(name, device, context, incoming, width, height);
    let open = filter_rack.refill(&item.filters)?;
    Ok(FilledRack {
        rack,
        filters: SourceFilters { filter_rack, open },
    })
}

/// The same, on the CUDA backend.
#[cfg(target_os = "linux")]
pub(in crate::engine) fn filled_rack(
    name: &str,
    device: &Arc<media_pp::elements::CudaDevice>,
    incoming: filters::ChainFormat,
    [width, height]: [u32; 2],
    item: &SceneItemSnapshot,
) -> Result<FilledRack, BackendError> {
    let (rack, filter_rack) = filters::rack(name, device, incoming, width, height);
    let open = filter_rack.refill(&item.filters)?;
    Ok(FilledRack {
        rack,
        filters: SourceFilters { filter_rack, open },
    })
}

/// Where a decoded picture ends up: through the Source's filters, then into
/// its compositor input. What a media file and a stream both build their
/// video branch toward.
pub(in crate::engine) struct PictureEnd {
    pub(in crate::engine) rack: media_pp::elements::Rack,
    pub(in crate::engine) sink: Box<dyn media_pp::element::Sink>,
}

impl PictureEnd {
    /// The branch these two are, ready to attach.
    pub(in crate::engine) fn branch(
        self,
        context: &Arc<media_pp::element::Context>,
    ) -> media_pp::error::Result<media_pp::pipeline::DetachedBranch> {
        context.branch().pipe(self.rack).to(self.sink)
    }
}

/// What [`filled_rack`] answers: the element for the branch, and what the
/// [`OpenSource`] keeps to reach it.
pub(in crate::engine) struct FilledRack {
    pub(in crate::engine) rack: media_pp::elements::Rack,
    pub(in crate::engine) filters: SourceFilters,
}

/// A running Source's way back to its rack, and the filters in it — the two
/// fields of [`OpenSource`] that [`OpenSource::filters`] and
/// [`OpenSource::filter_rack`] are, carried together until one is built.
pub(in crate::engine) struct SourceFilters {
    pub(in crate::engine) filter_rack: filters::FilterRack,
    pub(in crate::engine) open: Vec<filters::OpenFilter>,
}

/// Whether the file a Source was pointed at is there to be read, and if not,
/// the sentence the Sources list says instead.
///
/// `is_file` rather than `exists`: a directory picked through some other
/// route is not something to hand a demuxer, and it will not become one.
pub(in crate::engine) fn present_file(path: &std::path::Path) -> Result<(), String> {
    if path.is_file() {
        Ok(())
    } else {
        Err(format!("{} is not there", path.display()))
    }
}

/// A stored size hint as whole, even pixels — what a Source whose size is not
/// known until it runs builds its rack for.
pub(in crate::engine) fn hinted_size(item: &SceneItemSnapshot) -> [u32; 2] {
    item.source_size
        .map(|side| (side.round().max(2.0) as u32) & !1)
}

/// What a decoded picture's rack is built for: the size the decoder was, or
/// the stored hint where the stream's parameters did not say.
pub(in crate::engine) fn rack_size(
    decoded: Option<[u32; 2]>,
    item: &SceneItemSnapshot,
) -> [u32; 2] {
    decoded.unwrap_or_else(|| hinted_size(item))
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
    /// A line of text and everything about how it is drawn. All of it,
    /// rather than the string alone: a colour or an alignment changes the
    /// pixels exactly as the words do, and each has to be noticed the same
    /// way — see [`push_content`].
    Text(crate::domain::TextSourceSettings),
}

/// What opening a Source came to, when nothing went wrong.
///
/// Not an `Option`, which is what it used to be: "not there" has as many
/// causes as a failure has — a file on a drive that is not mounted, a window
/// that is closed, a camera another program holds, a stream that does not
/// answer or will not let this in — and the Sources list is the only place a
/// user can learn which, since a shipped build has no console to read.
///
/// Not boxed, for the reason `SourceState` is not: it is moved once per
/// open, as the `Option` it replaced was, and that was the same size.
#[allow(clippy::large_enum_variant)]
pub(in crate::engine) enum OpenOutcome {
    Open(OpenSource),
    /// What this Source shows is not there right now, and why. A state
    /// rather than a failure: the engine looks again — see
    /// `SourceState::Missing`.
    Absent(String),
}

/// A Source that is running, and the controls for its layer.
pub(in crate::engine) struct OpenSource {
    pub(in crate::engine) source: RunningSource,
    pub(in crate::engine) layer: Layer,
    pub(in crate::engine) name: String,
    /// The token the portal handed back, when it differs from the one it was
    /// given. `None` means the stored token is still current.
    pub(in crate::engine) refreshed_token: Option<Option<String>>,
    /// The picture size this Source actually opened at, for the kinds that
    /// negotiate one — a camera's mode, a capture's stream, a file's frames.
    ///
    /// Written back to the project when it differs from what was stored, for
    /// the same reason a refreshed token is: what the item believes decides
    /// what the editor lets you do to it. A crop is clamped against the
    /// stored size, so a stale one lets a crop point past the frame — and a
    /// layer with nothing left to draw is simply not drawn, which is a
    /// picture that vanishes with nothing said.
    ///
    /// `None` for a Source whose size is its own — a Color, a Drawing — and
    /// for one that could not read it.
    pub(in crate::engine) negotiated_size: Option<[u32; 2]>,
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
    /// The Source's filters as they are running, in chain order, each with
    /// the handle that reaches it.
    ///
    /// Empty for a Source with none, which is most of them. Settings and the
    /// enable flag reach the running elements through these; adding,
    /// removing and reordering go through [`filter_rack`](Self::filter_rack)
    /// instead, and this is replaced with what that answers.
    pub(in crate::engine) filters: Vec<filters::OpenFilter>,
    /// The rack those filters sit in.
    ///
    /// Every kind has one, so its whole filter list can be exchanged without
    /// the Source being reopened — which is the point, since reopening a
    /// camera is a visible stall and on Wayland a portal dialog.
    pub(in crate::engine) filter_rack: filters::FilterRack,
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
    /// What the meter shows, as `f32` bits — see `audio::Meter`. Zero until
    /// the audio branch has measured anything, which is also what a file
    /// with no sound looks like.
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
/// Tells a running media file or stream Source what its settings now say.
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

    // What the Audio Mixer column sets, which a file and a stream both have.
    let (gain_db, mute, monitored) = match &item.settings {
        SourceSettings::MediaFile(settings) => {
            (settings.gain_db, settings.muted, settings.monitored)
        }
        SourceSettings::Rtsp(settings) => (settings.gain_db, settings.muted, settings.monitored),
        _ => return,
    };

    if let Some(routing) = &mut media.sound {
        routing.apply(
            crate::engine::audio::monitors(monitored, monitor.is_some()),
            monitor,
        );
        // A file's and a stream's alike: both carry their sound through
        // the same branch, and it is the Source's list either way.
        routing.apply_filters(&item.audio_filters);
    }

    if let Some(volume) = &media.volume {
        let _ = volume.set_gain_db(gain_db);
        volume.set_muted(muted(mute, item.visible));
    }
    if let (Some(looping), SourceSettings::MediaFile(settings)) = (&media.looping, &item.settings) {
        looping.set_looping(settings.looping);
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

/// One of a Source's own audio filters, while its slider is still held —
/// the fader's split again: the handle now, the project once let go.
pub(in crate::engine) fn set_audio_filter_settings(
    source: &OpenSource,
    id: crate::domain::AudioFilterId,
    settings: &crate::domain::AudioFilterSettings,
) {
    if let Some(media) = &source.media_file
        && let Some(routing) = &media.sound
    {
        routing.retune_filter(id, settings);
    }
}

/// Whether this Source redraws itself as time passes, rather than only when
/// something is edited.
///
/// What the engine's idle tick asks, so that a clock is the only thing it
/// wakes up for: every other pushed Source is redrawn by a Scene change and
/// nothing else, and asking those a hundred times a second what they should
/// be showing would be a hundred rasterizations for an answer that has not
/// moved.
pub(in crate::engine) fn redraws_with_the_clock(item: &SceneItemSnapshot) -> bool {
    use crate::domain::{SourceSettings, TextMode};

    match &item.settings {
        SourceSettings::Text(settings) => match settings.mode {
            TextMode::Static => false,
            TextMode::Clock => true,
            // A stopwatch that is stopped is a fixed number on the screen.
            TextMode::Timer => settings.timer.running(),
        },
        _ => false,
    }
}

pub(in crate::engine) fn refresh_pushed(source: &mut OpenSource, item: &SceneItemSnapshot) {
    use crate::domain::SourceSettings;

    let wanted = match &item.settings {
        SourceSettings::Color(settings) => PushedContent::Color(settings.rgba),
        SourceSettings::Drawing(settings) => PushedContent::Drawing(settings.strokes.clone()),
        // Resolved rather than stored: a clock's settings do not change as
        // it ticks, and what is compared below has to.
        SourceSettings::Text(settings) => PushedContent::Text(text::resolved(settings)),
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
        PushedContent::Text(settings) => match text::text_bgra(width, height, settings) {
            Ok(frame) => frame,
            // A font that has gone missing since the Source opened, most
            // likely. Reported once per change rather than per frame, and
            // what was last drawn stays on the Canvas — which is a better
            // answer for a caption than blanking it.
            Err(error) => {
                eprintln!("could not redraw \"{}\": {error}", source.name);
                return;
            }
        },
        // Nothing asks for this. An Image Source is opened with one file and
        // keeps it, so `refresh_pushed` never names one here — the arm exists
        // because the content is compared like every other kind's. What does
        // push a picture twice is its filters, and [`repush`] sends the frame
        // it kept rather than drawing one.
        PushedContent::Image(_) => return,
    };
    if let Err(error) = surface.push(frame) {
        eprintln!("could not update \"{}\": {error}", source.name);
        return;
    }
    surface.content = wanted;
}

#[cfg(test)]
mod tests {
    use super::{hinted_size, rack_size};
    use crate::domain::{
        ColorSourceSettings, Crop, SceneItemId, SourceKind, SourceSettings, Transform,
    };
    use crate::snapshots::SceneItemSnapshot;

    fn item(source_size: [f32; 2]) -> SceneItemSnapshot {
        SceneItemSnapshot {
            filters: Vec::new(),
            audio_filters: Vec::new(),
            id: SceneItemId(1),
            name: "Window".to_owned(),
            kind: SourceKind::Color,
            settings: SourceSettings::Color(ColorSourceSettings {
                size: source_size,
                rgba: [0, 0, 0, 255],
            }),
            source_size,
            visible: true,
            locked: false,
            transform: Transform::default(),
            crop: Crop::default(),
            peak_db: None,
            position: None,
        }
    }

    /// A hint is whatever the picker reported, and a rack's bridge on the
    /// CUDA backend converts to NV12 sizes: whole, even, and never zero.
    #[test]
    fn a_hinted_size_is_whole_even_pixels_and_never_empty() {
        assert_eq!(hinted_size(&item([1279.6, 721.0])), [1280, 720]);
        assert_eq!(hinted_size(&item([0.0, 1.0])), [2, 2]);
    }

    /// What the decoder said wins; the hint is only for when it said
    /// nothing.
    #[test]
    fn a_decoded_size_is_preferred_to_the_hint() {
        let item = item([640.0, 480.0]);
        assert_eq!(rack_size(Some([1920, 1080]), &item), [1920, 1080]);
        assert_eq!(rack_size(None, &item), [640, 480]);
    }
}
