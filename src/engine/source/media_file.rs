//! A media file: one video file into the compositor, and its own sound into
//! the audio mixer.
//!
//! # Missing is not failure
//!
//! A path is stored as it was picked and never resolved to anything else, so
//! a file on a drive that is not mounted, or one that has been moved, is an
//! ordinary state rather than an error — the same standing a closed window
//! has. Opening one answers `Ok(None)` for it and the engine keeps the Source
//! [`SourceState::Missing`] and looks again. A file that is *there* and will
//! not demux is a real failure and still `Err`.
//!
//! [`SourceState::Missing`]: crate::engine::SourceState
//!
//! # Shape
//!
//! ```text
//! FileDemuxer ┬ video ─ hardware decoder ─ Queue ─ Pacer ─ compositor input
//!             └ audio ─ SwDecoder        ─ Queue ─ Pacer ─ mixer input
//! ```
//!
//! One pipeline, two branches off one demuxer. That is what keeps the picture
//! and the sound together: both `Pacer`s wait against the *same* clock — the
//! pipeline's own — so each branch is released at its own media timestamp
//! measured from one shared origin. Two pipelines would each anchor their own
//! t=0 at whenever they happened to start, which is A/V drift built in.
//!
//! Neither branch decodes the same way. Video is decoded on the GPU straight
//! into the surfaces the compositor draws from, so its frames never reach
//! system memory; both compositors take NV12 device frames directly, so there
//! is nothing to convert between the decoder and the layer. Audio has no such
//! path and no reason to want one.
//!
//! The `Queue` in each branch is where decode runs ahead: a `Pacer` sleeps
//! until a frame is due, and without a queue in front of it that sleep would
//! be the demuxer's too — one read cursor serves both streams, so a stalled
//! video branch would starve the audio one.
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
    AppSink, AudioVolume, AudioVolumeHandle, FileDemuxer, FileDemuxerHandle, MixerHandle, Pacer,
    StreamInfo, SwDecoder, TeeBuilder,
};
use media_pp::ffmpeg;
use media_pp::pipeline::Pipeline;

use crate::domain::MediaFileSettings;
use crate::engine::backend::BackendError;
use crate::engine::source::{MediaMeters, input_name};
use crate::snapshots::SceneItemSnapshot;

/// How much either branch may hold while the other is being read.
///
/// Small on purpose. This is not a jitter buffer — the file is not live and
/// nothing arrives late — it is only enough room for decode to keep working
/// while a `Pacer` waits out a frame's presentation time. Every frame parked
/// here is also a decoder surface that cannot be reused, which is what the
/// budget below has to cover.
const QUEUE_DEPTH: usize = 8;

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

/// One decoded stream's index and what is needed to build its branch.
struct Track {
    index: usize,
    params: ffmpeg::codec::Parameters,
    time_base: ffmpeg::Rational,
}

/// The settings this item is, and the file it names — or `None` where the
/// file is not there right now.
fn settings(item: &SceneItemSnapshot) -> Result<Option<&MediaFileSettings>, BackendError> {
    let crate::domain::SourceSettings::MediaFile(settings) = &item.settings else {
        return Err("scene item is not a media file".into());
    };
    // `is_file` rather than `exists`: a directory picked through some other
    // route is not something to hand a demuxer, and it will not become one.
    Ok(settings.path.is_file().then_some(settings))
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
    let video = streams
        .iter()
        .find(|stream| stream.kind == ffmpeg::media::Type::Video)
        .ok_or("the file has no video stream")?
        .index;
    let audio = mixer
        .and(
            streams
                .iter()
                .find(|stream| stream.kind == ffmpeg::media::Type::Audio),
        )
        .and_then(|stream| track(demuxer, stream.index));
    Ok(Chosen {
        video,
        video_params: demuxer
            .stream_parameters(video)
            .ok_or("the video stream disappeared")?,
        video_time_base: demuxer
            .stream_time_base(video)
            .ok_or("the video stream disappeared")?,
        audio,
    })
}

/// One stream's parameters and unit, or `None` for a stream that cannot
/// describe itself.
///
/// `None` rather than an error only because this is the audio half: a file
/// whose sound cannot be read is still a file worth showing.
fn track(demuxer: &FileDemuxer, index: usize) -> Option<Track> {
    Some(Track {
        index,
        params: demuxer.stream_parameters(index)?,
        time_base: demuxer.stream_time_base(index)?,
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
            eprintln!("could not show the first frame while paused: {error}");
        }
    }
    Ok(())
}

/// What this Source registers its audio with the mixer as.
///
/// Distinct from the compositor's input name even though the two registries
/// could not collide, so a log naming one is never ambiguous about which.
fn audio_name(name: &str) -> String {
    format!("{name}-audio")
}

/// The audio branch, built before the pipeline is.
///
/// Everything here can fail for an ordinary reason — a codec this FFmpeg was
/// not built with, a mixer that has gone — so it is built where an error can
/// be reported rather than unwrapped inside the builder closure, on the
/// engine thread.
struct Audio {
    index: usize,
    time_base: ffmpeg::Rational,
    decoder: SwDecoder,
    fader: AudioVolume,
    /// What the Audio Mixer dock moves, and what it reads.
    volume: AudioVolumeHandle,
    /// The mixer input this file's sound is summed into.
    mix: Box<dyn Sink>,
    /// The `AppSink` that measures the level.
    meter: Box<dyn Sink>,
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

/// Builds it, or answers `None` for a file with no sound and for a machine
/// whose mixer never started — the picture is worth showing either way.
fn audio(
    name: &str,
    track: Option<Track>,
    mixer: Option<&MixerHandle>,
    settings: &MediaFileSettings,
    item: &SceneItemSnapshot,
    meters: &Arc<MediaMeters>,
) -> Result<Option<Audio>, BackendError> {
    let (Some(track), Some(mixer)) = (track, mixer) else {
        return Ok(None);
    };
    let decoder = SwDecoder::new(format!("{name}-audio-decoder"), track.params)?;

    // The fader lives in this pipeline rather than the audio thread's,
    // because this file's sound belongs to this Source rather than to a
    // device everything shares.
    let (fader, volume) = AudioVolume::new(format!("{name}-volume"));
    let _ = volume.set_gain_db(settings.gain_db);
    volume.set_muted(super::muted(settings.muted, item.visible));

    let meter = AppSink::new(format!("{name}-meter"), {
        let meters = Arc::clone(meters);
        move |buffer| {
            if let media_pp::buffer::MediaBuffer::Audio(frame) = &buffer {
                meters.peak.store(
                    crate::engine::audio::peak_db(frame).to_bits(),
                    Ordering::Relaxed,
                );
            }
            Ok(())
        }
    });

    let mix = mixer
        .add_source(audio_name(name))
        .ok_or("the audio mixer is gone")?;
    Ok(Some(Audio {
        index: track.index,
        time_base: track.time_base,
        decoder,
        fader,
        volume,
        mix,
        meter: Box::new(meter),
    }))
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
fn attach_video(
    context: &Arc<Context>,
    source: &mut FileDemuxer,
    index: usize,
    time_base: ffmpeg::Rational,
    decoder: impl media_pp::element::Filter + 'static,
    sink: Box<dyn Sink>,
    position: Box<dyn Sink>,
) -> media_pp::error::Result<()> {
    let draw = context.branch().to(sink)?;
    let record = context.branch().to(position)?;
    let tee = TeeBuilder::new("video-tee", context.clone())
        .branch(draw)
        .branch(record)
        .build()?;
    let paced = context
        .branch()
        .pipe(decoder)
        .queue("video", QUEUE_DEPTH)
        .pipe(Pacer::new("video-pacer", time_base)?)
        .to_branch(tee)?;
    context.attach(source, index, paced)?;
    Ok(())
}

fn attach_audio(
    context: &Arc<Context>,
    source: &mut FileDemuxer,
    audio: Audio,
) -> media_pp::error::Result<()> {
    let meter = context.branch().to(audio.meter)?;
    let mix = context.branch().to(audio.mix)?;
    let tee = TeeBuilder::new("audio-tee", context.clone())
        .branch(meter)
        .branch(mix)
        .build()?;
    let faded = context
        .branch()
        .pipe(audio.decoder)
        .queue("audio", QUEUE_DEPTH)
        .pipe(Pacer::new("audio-pacer", audio.time_base)?)
        .pipe(audio.fader)
        .to_branch(tee)?;
    context.attach(source, audio.index, faded)?;
    Ok(())
}

#[cfg(target_os = "windows")]
pub(in crate::engine) fn open(
    device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
    handle: &media_pp::elements::D3d11VideoCompositorHandle,
    mixer: Option<&MixerHandle>,
    item: &SceneItemSnapshot,
    layer: media_pp::elements::VideoLayer,
) -> Result<Option<super::OpenSource>, BackendError> {
    use media_pp::elements::{D3d11Decoder, D3d11VideoCompositorInput};

    use crate::engine::backend::RunningSource;
    use crate::engine::source::{MediaFile, OpenSource};

    let Some(settings) = settings(item)? else {
        return Ok(None);
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
    let video_decoder = D3d11Decoder::new(
        format!("{name}-video-decoder"),
        chosen.video_params,
        device,
        HW_FRAME_BUDGET,
    )?;
    let meters = Arc::new(MediaMeters::default());
    let audio = audio(&name, chosen.audio, mixer, settings, item, &meters)?;
    let volume = audio.as_ref().map(|audio| audio.volume.clone());
    let position = position_sink(
        &name,
        chosen.video_time_base,
        looping.clone(),
        Arc::clone(&meters),
    );

    let D3d11VideoCompositorInput { sink, layer } = handle
        .add_source(name.clone(), layer)?
        .ok_or("the compositor is no longer running")?;

    let video_time_base = chosen.video_time_base;
    let video_index = chosen.video;
    let pipeline = Pipeline::new(name.clone(), demuxer, move |source, context| {
        attach_video(
            context,
            source,
            video_index,
            video_time_base,
            video_decoder,
            sink,
            position,
        )?;

        if let Some(audio) = audio {
            attach_audio(context, source, audio)?;
        }
        Ok(())
    })?;
    start(&pipeline, settings.paused)?;

    Ok(Some(OpenSource {
        source: RunningSource::Owned(Arc::clone(&pipeline)),
        layer,
        name,
        refreshed_token: None,
        showing: true,
        running: !settings.paused,
        pushed: None,
        media_file: Some(MediaFile {
            looping,
            volume,
            meters,
            pipeline: Arc::clone(&pipeline),
        }),
    }))
}

#[cfg(target_os = "linux")]
pub(in crate::engine) fn open(
    device: &media_pp::elements::CudaDevice,
    handle: &media_pp::elements::CudaVideoCompositorHandle,
    mixer: Option<&MixerHandle>,
    item: &SceneItemSnapshot,
    layer: media_pp::elements::VideoLayer,
) -> Result<Option<super::OpenSource>, BackendError> {
    use media_pp::elements::{CudaDecoder, CudaVideoCompositorInput};

    use crate::engine::backend::RunningSource;
    use crate::engine::source::{MediaFile, OpenSource};

    let Some(settings) = settings(item)? else {
        return Ok(None);
    };
    let name = input_name(item);
    let (demuxer, streams) = FileDemuxer::open(name.clone(), &settings.path)?;
    let chosen = choose(&demuxer, &streams, mixer)?;

    let looping = demuxer.looping_handle();
    looping.set_looping(settings.looping);

    // NVDEC hands out NV12 in CUDA memory, which is one of the two the
    // compositor draws from — so there is no `CudaConverter` here, unlike the
    // Sources that upload BGRA of their own.
    let video_decoder = CudaDecoder::new(
        format!("{name}-video-decoder"),
        chosen.video_params,
        device,
        HW_FRAME_BUDGET,
    )?;
    let meters = Arc::new(MediaMeters::default());
    let audio = audio(&name, chosen.audio, mixer, settings, item, &meters)?;
    let volume = audio.as_ref().map(|audio| audio.volume.clone());
    let position = position_sink(
        &name,
        chosen.video_time_base,
        looping.clone(),
        Arc::clone(&meters),
    );

    let CudaVideoCompositorInput { sink, layer } = handle
        .add_source(name.clone(), layer)?
        .ok_or("the compositor is no longer running")?;

    let video_time_base = chosen.video_time_base;
    let video_index = chosen.video;
    let pipeline = Pipeline::new(name.clone(), demuxer, move |source, context| {
        attach_video(
            context,
            source,
            video_index,
            video_time_base,
            video_decoder,
            sink,
            position,
        )?;

        if let Some(audio) = audio {
            attach_audio(context, source, audio)?;
        }
        Ok(())
    })?;
    start(&pipeline, settings.paused)?;

    Ok(Some(OpenSource {
        source: RunningSource::Owned(Arc::clone(&pipeline)),
        layer,
        name,
        refreshed_token: None,
        showing: true,
        running: !settings.paused,
        pushed: None,
        media_file: Some(MediaFile {
            looping,
            volume,
            meters,
            pipeline: Arc::clone(&pipeline),
        }),
    }))
}
