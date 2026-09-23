//! A media file: one video file into the compositor, and its own sound into
//! the audio mixer.
//!
//! # Missing is not failure
//!
//! A path is stored as it was picked and never resolved to anything else, so
//! a file on a drive that is not mounted, or one that has been moved, is an
//! ordinary state rather than an error — the same standing a closed window
//! has. Opening one answers `OpenOutcome::Absent` for it and the engine keeps the Source
//! [`SourceState::Missing`] and looks again. A file that is *there* and will
//! not demux is a real failure and still `Err`.
//!
//! [`SourceState::Missing`]: crate::engine::SourceState
//!
//! # Shape
//!
//! ```text
//! FileDemuxer ┬ video ─ Queue ─ VideoDecodeBin ─ Queue ─ Pacer ─ compositor input
//!             └ audio ─ SwDecoder ─ Queue ────────────── Pacer ─ mixer input
//! ```
//!
//! One pipeline, two branches off one demuxer. That is what keeps the picture
//! and the sound together: both `Pacer`s wait against the *same* clock — the
//! pipeline's own — so each branch is released at its own media timestamp
//! measured from one shared origin. Two pipelines would each anchor their own
//! t=0 at whenever they happened to start, which is A/V drift built in.
//!
//! Neither branch decodes the same way. Video is decoded on the GPU straight
//! into the surfaces the compositor draws from wherever the GPU takes the
//! stream, so its frames never reach system memory; both compositors take NV12
//! device frames directly, so there is nothing to convert between the decoder
//! and the layer — until the file has filters, whose rack sits just before the
//! compositor input and bridges to BGRA for as long as it holds any. What the
//! GPU does not take — a codec it has no decoder for, 10-bit or 4:4:4, a
//! profile it refuses once asked — `VideoDecodeBin` decodes in software and
//! uploads, as NV12, or as BGRA where the file has alpha to keep, which then
//! reaches the compositor as the transparency it was made with. Audio has no
//! such path and no reason to want one.
//!
//! The `Queue` in each branch is where decode runs ahead: a `Pacer` sleeps
//! until a frame is due, and without a queue in front of it that sleep would
//! be the demuxer's too — one read cursor serves both streams, so a stalled
//! video branch would starve the audio one.
//!
//! That is also why the video branch has two. Its decoded frames are decoder
//! surfaces, which are counted and few, so that queue has to stay shallow;
//! but a shallow queue there is a short leash on the cursor, and the cursor
//! is what feeds the audio. The first queue holds packets instead — still
//! compressed, still host memory — so the read-ahead that keeps sound coming
//! costs neither a surface nor the decode happening on the cursor's thread.
//! See `PACKET_LOOKAHEAD`.
//!
//! # Playing once is a state, not an end
//!
//! A file that is not looping reaches its end, sends EOS, and its layer goes
//! with it. Nothing here reopens it: `notice_closed_windows` asks only about
//! Window Captures, deliberately, so a finished file stays finished until
//! someone asks for it again. Looping is what makes it not finish, and it is
//! switched where it is rather than by reopening — see
//! [`super::refresh_media_file`].

use std::sync::Arc;
use std::sync::atomic::Ordering;

use media_pp::element::Context;
use media_pp::element::Sink;
use media_pp::elements::{
    AppSink, FileDemuxer, FileDemuxerHandle, MixerHandle, Pacer, StreamInfo, TeeBuilder,
};
use media_pp::ffmpeg;
use media_pp::pipeline::Pipeline;

use crate::domain::MediaFileSettings;
use crate::engine::audio::MeterWake;
use crate::engine::backend::BackendError;
use crate::engine::source::decode_policy::{self, Playback};
use crate::engine::source::sound::{self, Sound, Track};
use crate::engine::source::{FilledRack, MediaMeters, PictureEnd, input_name};
use crate::snapshots::SceneItemSnapshot;

/// How much either branch may hold while the other is being read.
///
/// Small on purpose. This is not a jitter buffer — the file is not live and
/// nothing arrives late — it is only enough room for decode to keep working
/// while a `Pacer` waits out a frame's presentation time. Every frame parked
/// here is also a decoder surface that cannot be reused, which is what the
/// budget below has to cover.
const QUEUE_DEPTH: usize = 8;

/// Packets the demuxer may read ahead of the video decoder.
///
/// One read cursor serves both streams, and until this queue existed the
/// video decoder ran on the cursor's own thread: a keyframe took longer to
/// decode than the frames around it, and the audio packets behind it in the
/// file waited for it. The mixer does not wait — an input short for a tick
/// contributes silence for the shortfall — so every keyframe became a hole
/// in the mix, and since the mixer emits what arrives rather than what a
/// timestamp asks for, each hole also pushed the rest of the file's sound
/// permanently later against its picture.
///
/// Packets, not frames: these are still compressed and in host memory, so
/// reading a second ahead costs a few megabytes rather than a decoder
/// surface each.
const PACKET_LOOKAHEAD: usize = 64;

/// Decoded frames the hardware decoder must have surfaces for beyond its own
/// reference frames.
///
/// A hardware decoder's pool is fixed at construction and cannot grow, so
/// this has to cover everything downstream may hold at once: the queue above,
/// the frame a `Pacer` is sitting on, and the one or two the compositor keeps
/// per layer. NVDEC caps the whole pool — reference frames included — at 32,
/// so this is also a number that has to stay well clear of it.
const HW_FRAME_BUDGET: i32 = 16;

/// Which of a file's streams are played, and what from.
struct Chosen {
    video: usize,
    video_params: ffmpeg::codec::Parameters,
    video_time_base: ffmpeg::Rational,
    /// `None` for a file with no audio, and for a machine whose mixer never
    /// started — the picture is worth showing either way.
    audio: Option<Track>,
}

/// The settings this item is — or why the file it names cannot be read right
/// now.
fn settings(item: &SceneItemSnapshot) -> Result<Result<&MediaFileSettings, String>, BackendError> {
    let crate::domain::SourceSettings::MediaFile(settings) = &item.settings else {
        return Err("scene item is not a media file".into());
    };
    Ok(super::present_file(&settings.path).map(|()| settings))
}

/// Picks the streams to play and reads what each branch is built from.
///
/// A video stream is required. This is a Scene Source — it occupies a
/// rectangle on the Canvas — so a file with only sound in it is not something
/// that can be placed, and saying so is better than composing nothing.
fn choose(
    demuxer: &FileDemuxer,
    streams: &[StreamInfo],
    mixer: Option<&MixerHandle>,
) -> Result<Chosen, BackendError> {
    // FFmpeg's own pick rather than the first of a kind: a file can carry
    // cover art as a still video stream ahead of the picture it is of.
    let best = |kind| {
        demuxer
            .best_stream(kind)
            .and_then(|index| streams.iter().find(|stream| stream.index == index))
    };
    let video = best(ffmpeg::media::Type::Video).ok_or("the file has no video stream")?;
    Ok(Chosen {
        video: video.index,
        video_params: video.parameters.clone(),
        video_time_base: video.time_base,
        audio: mixer.and(best(ffmpeg::media::Type::Audio)).map(Track::of),
    })
}

/// Starts the pipeline, and stops it again where the Source is stored paused.
///
/// A Source that is paused the moment it opens has produced nothing, and a
/// compositor layer with no frame draws nothing at all — so a clip paused
/// before the application closed would come back as an empty rectangle. The
/// seek is what fixes that: it costs a flush and a preroll, and a preroll is
/// exactly "put one frame through every terminal", after which the pipeline
/// restores the state that was asked for. The picture appears and stays
/// where it is.
///
/// To the start rather than to where it was: where a clip is playing from is
/// not written down — see `SourceCommand::SetMediaPaused` for what is.
fn start(pipeline: &Arc<Pipeline>, paused: bool) -> Result<(), BackendError> {
    pipeline.run()?;
    if paused {
        pipeline.pause();
        if let Err(error) = pipeline.seek(
            std::time::Duration::ZERO,
            media_pp::pipeline::SeekMode::Keyframe,
        ) {
            // Reported and carried on. What was lost is the first frame, so
            // the layer stays empty until someone presses play — which is a
            // Source that opened, not one that failed to.
            tracing::warn!("could not show the first frame while paused: {error}");
        }
    }
    Ok(())
}

/// The sink that records where playback has reached, and how it is wired.
///
/// On the *video* branch rather than the audio one, because every media file
/// has a picture and only some have sound — and because what a progress bar
/// means is where the picture is.
///
/// The loop's offset is taken off here rather than by the reader: the two are
/// only comparable at the moment a frame is stamped, and doing it anywhere
/// else would mean sampling them apart and subtracting numbers from different
/// instants.
fn position_sink(
    name: &str,
    time_base: ffmpeg::Rational,
    looping: FileDemuxerHandle,
    meters: Arc<MediaMeters>,
) -> Box<dyn Sink> {
    let micros = f64::from(time_base.numerator()) / f64::from(time_base.denominator()) * 1e6;
    Box::new(AppSink::new(format!("{name}-position"), move |buffer| {
        if let media_pp::buffer::MediaBuffer::Video(frame) = &buffer
            && let Some(pts) = frame.pts()
        {
            let offset = looping.lap_offset().as_micros() as i64;
            meters
                .position
                .store((pts as f64 * micros) as i64 - offset, Ordering::Relaxed);
        }
        Ok(())
    }))
}

/// This file's sound, if it has any and there is a mixer to take it.
fn audio(
    name: &str,
    track: Option<Track>,
    mixer: Option<&MixerHandle>,
    settings: &MediaFileSettings,
    item: &SceneItemSnapshot,
    meters: &Arc<MediaMeters>,
    meter_wake: &MeterWake,
) -> Result<Option<Sound>, BackendError> {
    sound::build(
        name,
        track,
        mixer,
        sound::SoundSettings {
            gain_db: settings.gain_db,
            muted: super::muted(settings.muted, item.visible),
            filters: &item.audio_filters,
        },
        meters,
        meter_wake,
    )
}

/// Attaches it to the demuxer's audio pad.
///
/// The `Tee` hangs off the *fader*, so a meter shows what the fader let
/// through rather than what arrived at it — pulling one down empties its
/// meter, and so does muting. `to_branch` rather than `to`, because a `Tee`
/// is a finished branch rather than a `Sink`: attaching it to the fader's pad
/// on its own would link the same buffers but record the fan-out as the
/// demuxer's.
/// Attaches the video pad: decoded on the GPU, paced, then split between what
/// draws it and what records where it has reached.
///
/// The filters are on the drawing side of the split, so what records the
/// position sees every frame whatever a filter does with it.
fn attach_video(
    context: &Arc<Context>,
    source: &mut FileDemuxer,
    index: usize,
    decoder: impl media_pp::element::Filter + 'static,
    picture: PictureEnd,
    position: Box<dyn Sink>,
) -> media_pp::error::Result<()> {
    let draw = picture.branch(context)?;
    let record = context.branch().to(position)?;
    let tee = TeeBuilder::new("video-tee", context.clone())
        .branch(draw)
        .branch(record)
        .build()?;
    let paced = context
        .branch()
        .queue("video-packets", PACKET_LOOKAHEAD)
        .pipe(decoder)
        .queue("video", QUEUE_DEPTH)
        .pipe(Pacer::new("video-pacer"))
        .to_branch(tee)?;
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

    let settings = match settings(item)? {
        Ok(settings) => settings,
        Err(absent) => return Ok(super::OpenOutcome::Absent(absent)),
    };
    let name = input_name(item);
    let (demuxer, streams) = FileDemuxer::open(name.clone(), &settings.path)?;
    let chosen = choose(&demuxer, &streams, mixer)?;

    // Set before the pipeline runs, so a file stored as looping never plays
    // its end once without it.
    let looping = demuxer.looping_handle();
    looping.set_looping(settings.looping);

    // Both decoders are built out here rather than in the builder below: they
    // fail for ordinary reasons — a codec this FFmpeg was not built with, a
    // GPU that does not decode this profile — and that is an error to report,
    // not something to unwrap on the engine thread.
    // Read before the parameters are moved into the decoder, which is
    // also the only place they describe a picture rather than a stream.
    let size = super::decoded_size(&chosen.video_params);
    let codec = chosen.video_params.id();
    let threading = decode_policy::threading(codec, size, Playback::File);
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
    // NV12 from the decoder, bridged to BGRA only while there are filters —
    // the camera's arrangement, and the reason an unfiltered file still goes
    // to the compositor without a conversion.
    let FilledRack { rack, filters } = super::filled_rack(
        &name,
        device,
        context,
        super::decoded_chain_format(&video_decoder),
        item,
    )?;
    let meters = Arc::new(MediaMeters::default());
    let audio = audio(
        &name,
        chosen.audio,
        mixer,
        settings,
        item,
        &meters,
        meter_wake,
    )?;
    let volume = audio.as_ref().map(|audio| audio.volume.clone());
    let position = position_sink(
        &name,
        chosen.video_time_base,
        looping.clone(),
        Arc::clone(&meters),
    );

    let D3d11VideoCompositorInput { sink, layer } = handle.add_source(name.clone(), layer)?;

    let video_index = chosen.video;
    let sound_name = name.clone();
    let (pipeline, routing) = Pipeline::new(name.clone(), demuxer, move |source, context| {
        attach_video(
            context,
            source,
            video_index,
            video_decoder,
            PictureEnd { rack, sink },
            position,
        )?;

        audio
            .map(|audio| sound::attach(context, source, audio, &sound_name))
            .transpose()
    })?;
    start(&pipeline, settings.paused)?;

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
        running: !settings.paused,
        pushed: None,
        negotiated_size: size,
        page: None,
        media_file: Some(MediaFile {
            looping: Some(looping),
            volume,
            meters,
            pipeline: Arc::clone(&pipeline),
            sound: routing,
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

    let settings = match settings(item)? {
        Ok(settings) => settings,
        Err(absent) => return Ok(super::OpenOutcome::Absent(absent)),
    };
    let name = input_name(item);
    let (demuxer, streams) = FileDemuxer::open(name.clone(), &settings.path)?;
    let chosen = choose(&demuxer, &streams, mixer)?;

    let looping = demuxer.looping_handle();
    looping.set_looping(settings.looping);

    // NV12 in CUDA memory, from NVDEC or uploaded after a software decode,
    // is one of the two the compositor draws from — so there is no
    // `CudaConverter` here, unlike the Sources that upload BGRA of their own;
    // a file with alpha arrives as BGRA, the other one.
    // Read before the parameters are moved into the decoder, which is
    // also the only place they describe a picture rather than a stream.
    let size = super::decoded_size(&chosen.video_params);
    let codec = chosen.video_params.id();
    let threading = decode_policy::threading(codec, size, Playback::File);
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
    let audio = audio(
        &name,
        chosen.audio,
        mixer,
        settings,
        item,
        &meters,
        meter_wake,
    )?;
    let volume = audio.as_ref().map(|audio| audio.volume.clone());
    let position = position_sink(
        &name,
        chosen.video_time_base,
        looping.clone(),
        Arc::clone(&meters),
    );

    // No `Option` here, unlike the Direct3D half: the CUDA compositor
    // answers with the input itself or with an error.
    let CudaVideoCompositorInput { sink, layer } = handle.add_source(name.clone(), layer)?;

    let video_index = chosen.video;
    let sound_name = name.clone();
    let (pipeline, routing) = Pipeline::new(name.clone(), demuxer, move |source, context| {
        attach_video(
            context,
            source,
            video_index,
            video_decoder,
            PictureEnd { rack, sink },
            position,
        )?;

        audio
            .map(|audio| sound::attach(context, source, audio, &sound_name))
            .transpose()
    })?;
    start(&pipeline, settings.paused)?;

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
        running: !settings.paused,
        pushed: None,
        negotiated_size: size,
        page: None,
        media_file: Some(MediaFile {
            looping: Some(looping),
            volume,
            meters,
            pipeline: Arc::clone(&pipeline),
            sound: routing,
        }),
    }))
}
