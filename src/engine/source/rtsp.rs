//! A live network stream: RTSP into the compositor, and its sound into the
//! audio mixer.
//!
//! # Not there is not failure
//!
//! A URL is stored as it was typed and never resolved to anything else, so a
//! camera that is switched off, rebooting, or behind a network that is down is
//! an ordinary state rather than an error — the same standing a closed window
//! has. Opening one answers `OpenOutcome::Absent` for it and the engine holds the Source
//! [`SourceState::Missing`], which is what its own reconnect interval is then
//! measured from. A server that answers and then will not demux is a real
//! failure and still `Err`.
//!
//! [`SourceState::Missing`]: crate::engine::SourceState
//!
//! # Shape
//!
//! ```text
//! RtspSource ┬ video ─ Queue ─ VideoDecodeBin ─ Queue ─ Pacer ─ compositor input
//!            └ audio ─ SwDecoder ─ Queue ────────────── Pacer ─ mixer input
//! ```
//!
//! The same two branches off one source a media file has, and for the same
//! reason: both `Pacer`s wait against the pipeline's own clock, so the picture
//! and the sound are released from one shared origin instead of each anchoring
//! its own t=0 wherever it happened to start.
//!
//! A live source is paced rather than drawn on arrival. What that buys is
//! A/V sync; what it costs is that the stream is played at the rate its own
//! timestamps describe, so a camera whose clock disagrees with this machine's
//! drifts against it. Dropping to catch up is a jitter buffer's job and this
//! is not one — see `QUEUE_DEPTH`.
//!
//! Drift does not accumulate quietly forever, and it is worth knowing which
//! way it ends. A camera that runs *slow* falls further behind live, and
//! nothing stops it. One that runs *fast* fills the queues instead — a
//! couple of seconds of them — and then the source blocks where it reads,
//! which over TCP is this end no longer draining the socket until the server
//! gives up on it. That is a read error, and a read error is a disconnection
//! like any other: the pipeline ends and `retry_missing` opens a new one.
//! Visible, and recovered from, rather than silent.
//!
//! # Ending is a disconnection
//!
//! `RtspSource` does not reconnect. A read that fails ends the source with an
//! error, the pipeline finishes, and — since a pipeline is one-shot — coming
//! back means building a new one. That is the engine's `notice_dropped_streams`
//! and `retry_missing`, which is also why the interval is a setting rather
//! than a constant here: how long to wait before troubling a camera again is
//! not something this file can know.

use std::sync::Arc;

use media_pp::element::Context;
use media_pp::elements::{MixerHandle, Pacer, RtspOptions, RtspSource, RtspTransport, StreamInfo};
use media_pp::ffmpeg;
use media_pp::pipeline::Pipeline;

use crate::domain::RtspSourceSettings;
use crate::engine::audio::MeterWake;
use crate::engine::backend::BackendError;
use crate::engine::source::decode_policy::{self, Playback};
use crate::engine::source::sound::{self, Sound, Track};
use crate::engine::source::{FilledRack, MediaMeters, PictureEnd, input_name};
use crate::snapshots::SceneItemSnapshot;

/// Decoded frames held between the decoder and the `Pacer`.
///
/// Shallow, and deliberately: every frame parked here is a decoder surface
/// that cannot be reused, and a deep queue on a live stream is latency that
/// never comes back — it is filled once when the stream starts and stays
/// filled for as long as it runs.
const QUEUE_DEPTH: usize = 8;

/// Packets the source may read ahead of the video decoder.
///
/// One read serves both streams here as it does for a file, so a video
/// decoder that stalls on a keyframe would otherwise starve the sound behind
/// it — see the media file source, where that showed up as a hole in the mix
/// per keyframe. Packets are compressed and in host memory, so the read-ahead
/// costs megabytes rather than a decoder surface each.
const PACKET_LOOKAHEAD: usize = 64;

/// Decoded frames the hardware decoder must have surfaces for beyond its own
/// reference frames — the queue above, the frame a `Pacer` is sitting on, and
/// what the compositor keeps per layer. NVDEC caps the whole pool at 32.
const HW_FRAME_BUDGET: i32 = 16;

/// The longest a single frame may hold the `Pacer` before its timestamp is
/// read as a new timeline rather than a distant one.
///
/// A camera that reboots, or an RTP timestamp base that wraps, hands over a
/// timestamp with no relation to the one before it. Paced literally that is a
/// still picture for as long as the jump says — no error, nothing to
/// reconnect from, because as far as the pipeline is concerned it is working.
/// Past this the `Pacer` re-anchors on the new timeline instead.
///
/// Five seconds because it has to sit above the longest gap a working stream
/// can have — a camera that sends nothing for that long is already a problem
/// rather than a pause — and below anything a viewer would sit through.
const TIMELINE_JUMP: std::time::Duration = std::time::Duration::from_secs(5);

/// How long to wait for a server that is not answering.
///
/// Short, because this is a live address rather than a file: a camera that is
/// there answers in well under a second, and one that is not is not going to
/// answer at all. The wait is off the engine loop either way — see
/// `SourceOpener` — so what this decides is only how quickly a Source that is
/// not there reaches the state that says so.
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// Which of the session's streams are played, and what from.
struct Chosen {
    video: usize,
    video_params: ffmpeg::codec::Parameters,
    /// `None` for a stream with no sound, and for a machine whose mixer never
    /// started — the picture is worth showing either way.
    audio: Option<Track>,
}

/// The settings this item is.
fn settings(item: &SceneItemSnapshot) -> Result<&RtspSourceSettings, BackendError> {
    let crate::domain::SourceSettings::Rtsp(settings) = &item.settings else {
        return Err("scene item is not a network stream".into());
    };
    Ok(settings)
}

/// What the element takes, from what the project stored.
fn options(settings: &RtspSourceSettings) -> RtspOptions {
    RtspOptions {
        transport: match settings.transport {
            crate::domain::RtspTransport::Tcp => RtspTransport::Tcp,
            crate::domain::RtspTransport::Udp => RtspTransport::Udp,
        },
        timeout: CONNECT_TIMEOUT,
    }
}

/// Connects, or answers why the server is not there.
///
/// Every failure to connect is read as "not there", which is what makes an
/// unreachable camera a state rather than an error: a wrong address, a
/// refused connection and a switched-off camera are indistinguishable from
/// here, and treating any of them as fatal would leave a Source that never
/// comes back on its own. What the server said is the difference — a refused
/// password reads very differently from a camera that is off — and it is what
/// the Sources list shows.
fn connect(
    name: &str,
    settings: &RtspSourceSettings,
    item_name: &str,
) -> Result<(RtspSource, Vec<StreamInfo>), String> {
    RtspSource::open(name.to_owned(), &settings.url, options(settings)).map_err(|error| {
        tracing::warn!("\"{item_name}\" is not answering: {error}");
        format!("not answering: {error}")
    })
}

/// Picks the streams to play and reads what each branch is built from.
///
/// A video stream is required, as it is for a media file: this is a Scene
/// Source and occupies a rectangle on the Canvas, so a session with only
/// sound in it is not something that can be placed.
fn choose(
    source: &RtspSource,
    streams: &[StreamInfo],
    mixer: Option<&MixerHandle>,
) -> Result<Chosen, BackendError> {
    // FFmpeg's own pick rather than the first of a kind, as for a file.
    let best = |kind| {
        source
            .best_stream(kind)
            .and_then(|index| streams.iter().find(|stream| stream.index == index))
    };
    let video = best(ffmpeg::media::Type::Video).ok_or("the stream carries no video")?;
    Ok(Chosen {
        video: video.index,
        video_params: video.parameters.clone(),
        audio: mixer.and(best(ffmpeg::media::Type::Audio)).map(Track::of),
    })
}

/// Attaches the video pad: decoded on the GPU, paced, filtered and drawn.
///
/// No `Tee` here, unlike a media file's: there is no position to record for a
/// stream that has no end to measure against.
fn attach_video(
    context: &Arc<Context>,
    source: &mut RtspSource,
    index: usize,
    decoder: impl media_pp::element::Filter + 'static,
    picture: PictureEnd,
) -> media_pp::error::Result<()> {
    let paced = context
        .branch()
        .queue("video-packets", PACKET_LOOKAHEAD)
        .pipe(decoder)
        .queue("video", QUEUE_DEPTH)
        .pipe(Pacer::with_discontinuity_limit(
            "video-pacer",
            TIMELINE_JUMP,
        ))
        .pipe(picture.rack)
        .to(picture.sink)?;
    context.attach(source, index, paced)?;
    Ok(())
}

#[cfg(target_os = "windows")]
pub(in crate::engine) fn open(
    device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
    context: Arc<std::sync::Mutex<windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext>>,
    handle: &media_pp::elements::D3d11VideoCompositorHandle,
    mixer: Option<&MixerHandle>,
    meter_wake: &MeterWake,
    item: &SceneItemSnapshot,
    layer: media_pp::elements::VideoLayer,
) -> Result<super::OpenOutcome, BackendError> {
    use media_pp::elements::{D3d11VideoCompositorInput, DecodeTarget, VideoDecodeBin};

    use crate::engine::backend::RunningSource;
    use crate::engine::source::{MediaFile, OpenSource};

    let settings = settings(item)?;
    let name = input_name(item);
    let (source, streams) = match connect(&name, settings, &item.name) {
        Ok(connected) => connected,
        Err(absent) => return Ok(super::OpenOutcome::Absent(absent)),
    };
    let chosen = choose(&source, &streams, mixer)?;

    // Read before the parameters are moved into the decoder, which is
    // also the only place they describe a picture rather than a stream.
    let size = super::decoded_size(&chosen.video_params);
    // Live: a software decode must not hold pictures back to go faster,
    // which would be latency added to the camera's own.
    let codec = chosen.video_params.id();
    let threading = decode_policy::threading(codec, size, Playback::Live);
    let video_decoder = VideoDecodeBin::open(
        format!("{name}-video"),
        chosen.video_params,
        DecodeTarget::D3d11 {
            device: device.clone(),
            context: context.clone(),
            downstream_hw_frames: HW_FRAME_BUDGET,
        },
        threading,
    )?;
    decode_policy::log(&item.name, codec, size, threading, &video_decoder);
    // Bridged to BGRA only while there are filters, as a media file's is.
    let FilledRack { rack, filters } = super::filled_rack(
        &name,
        device,
        context,
        super::decoded_chain_format(&video_decoder),
        item,
    )?;
    let meters = Arc::new(MediaMeters::default());
    let audio = sound::build(
        &name,
        chosen.audio,
        mixer,
        sound::SoundSettings {
            gain_db: settings.gain_db,
            muted: super::muted(settings.muted, item.visible),
            filters: &item.audio_filters,
        },
        &meters,
        meter_wake,
    )?
    // The same limit the picture is paced with, so a sender that restarts
    // its timeline re-anchors both branches together — see `TIMELINE_JUMP`.
    .map(|sound| sound.with_discontinuity_limit(TIMELINE_JUMP));
    let volume = audio.as_ref().map(|audio| audio.volume.clone());

    let D3d11VideoCompositorInput { sink, layer } = handle.add_source(name.clone(), layer)?;

    let (pipeline, sound) = build(
        name.clone(),
        source,
        chosen.video,
        video_decoder,
        PictureEnd { rack, sink },
        audio,
    )?;
    Ok(super::OpenOutcome::Open(OpenSource {
        source: RunningSource::Owned(Arc::clone(&pipeline)),
        layer,
        name,
        refreshed_token: None,
        filters: filters.open,
        filter_rack: filters.filter_rack,
        // Set by the engine where it is opened into a Scene's own
        // composition — see `Target`.
        nested_in: None,
        showing: true,
        running: true,
        pushed: None,
        negotiated_size: size,
        page: None,
        media_file: Some(MediaFile {
            looping: None,
            volume,
            meters,
            pipeline,
            sound,
        }),
    }))
}

#[cfg(target_os = "linux")]
pub(in crate::engine) fn open(
    device: &Arc<media_pp::elements::CudaDevice>,
    handle: &media_pp::elements::CudaVideoCompositorHandle,
    mixer: Option<&MixerHandle>,
    meter_wake: &MeterWake,
    item: &SceneItemSnapshot,
    layer: media_pp::elements::VideoLayer,
) -> Result<super::OpenOutcome, BackendError> {
    use media_pp::elements::{CudaVideoCompositorInput, DecodeTarget, VideoDecodeBin};

    use crate::engine::backend::RunningSource;
    use crate::engine::source::{MediaFile, OpenSource};

    let settings = settings(item)?;
    let name = input_name(item);
    let (source, streams) = match connect(&name, settings, &item.name) {
        Ok(connected) => connected,
        Err(absent) => return Ok(super::OpenOutcome::Absent(absent)),
    };
    let chosen = choose(&source, &streams, mixer)?;

    // NV12 in CUDA memory, from NVDEC or uploaded after a software decode,
    // is one of the two the compositor draws from — so there is no
    // `CudaConverter` here, unlike the Sources that upload BGRA of their own;
    // a file with alpha arrives as BGRA, the other one.
    // Read before the parameters are moved into the decoder, which is
    // also the only place they describe a picture rather than a stream.
    let size = super::decoded_size(&chosen.video_params);
    // Live: a software decode must not hold pictures back to go faster,
    // which would be latency added to the camera's own.
    let codec = chosen.video_params.id();
    let threading = decode_policy::threading(codec, size, Playback::Live);
    let video_decoder = VideoDecodeBin::open(
        format!("{name}-video"),
        chosen.video_params,
        DecodeTarget::Cuda {
            device: media_pp::elements::CudaDevice::clone(device),
            downstream_hw_frames: HW_FRAME_BUDGET,
        },
        threading,
    )?;
    decode_policy::log(&item.name, codec, size, threading, &video_decoder);
    let FilledRack { rack, filters } = super::filled_rack(
        &name,
        device,
        super::decoded_chain_format(&video_decoder),
        item,
    )?;
    let meters = Arc::new(MediaMeters::default());
    let audio = sound::build(
        &name,
        chosen.audio,
        mixer,
        sound::SoundSettings {
            gain_db: settings.gain_db,
            muted: super::muted(settings.muted, item.visible),
            filters: &item.audio_filters,
        },
        &meters,
        meter_wake,
    )?
    // The same limit the picture is paced with, so a sender that restarts
    // its timeline re-anchors both branches together — see `TIMELINE_JUMP`.
    .map(|sound| sound.with_discontinuity_limit(TIMELINE_JUMP));
    let volume = audio.as_ref().map(|audio| audio.volume.clone());

    let CudaVideoCompositorInput { sink, layer } = handle.add_source(name.clone(), layer)?;

    let (pipeline, sound) = build(
        name.clone(),
        source,
        chosen.video,
        video_decoder,
        PictureEnd { rack, sink },
        audio,
    )?;
    Ok(super::OpenOutcome::Open(OpenSource {
        source: RunningSource::Owned(Arc::clone(&pipeline)),
        layer,
        name,
        refreshed_token: None,
        filters: filters.open,
        filter_rack: filters.filter_rack,
        // Set by the engine where it is opened into a Scene's own
        // composition — see `Target`.
        nested_in: None,
        showing: true,
        running: true,
        pushed: None,
        negotiated_size: size,
        page: None,
        media_file: Some(MediaFile {
            looping: None,
            volume,
            meters,
            pipeline,
            sound,
        }),
    }))
}

/// The pipeline both platforms build, which differs only in its decoder.
fn build(
    name: String,
    source: RtspSource,
    video_index: usize,
    decoder: impl media_pp::element::Filter + 'static,
    picture: PictureEnd,
    audio: Option<Sound>,
) -> Result<(Arc<Pipeline>, Option<sound::SoundRouting>), BackendError> {
    let sound_name = name.clone();
    let (pipeline, routing) = Pipeline::new(name, source, move |source, context| {
        attach_video(context, source, video_index, decoder, picture)?;
        audio
            .map(|audio| sound::attach(context, source, audio, &sound_name))
            .transpose()
    })?;
    pipeline.run()?;
    Ok((pipeline, routing))
}
