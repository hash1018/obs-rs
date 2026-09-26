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
use media_pp::elements::{AppSink, FileDemuxer, FileDemuxerHandle, MixerHandle, Pacer};
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
fn choose(demuxer: &FileDemuxer, mixer: Option<&MixerHandle>) -> Result<Chosen, BackendError> {
    // FFmpeg's own pick rather than the first of a kind: a file can carry
    // cover art as a still video stream ahead of the picture it is of.
    let video = demuxer
        .best(ffmpeg::media::Type::Video)
        .map_err(|_| "the file has no video stream")?;
    Ok(Chosen {
        video: video.index,
        video_params: video.parameters.clone(),
        video_time_base: video.time_base,
        audio: mixer
            .and(demuxer.best(ffmpeg::media::Type::Audio).ok())
            .map(|audio| Track::of(&audio)),
    })
}

/// Starts the pipeline — paused from the outset where the Source is stored
/// paused, so not one frame plays before it stops.
///
/// A Source that is paused the moment it opens has produced nothing, and a
/// compositor layer with no frame draws nothing at all — so a clip paused
/// before the application closed would come back as an empty rectangle. The
/// seek is what fixes that: it costs a flush and a preroll, and a preroll is
/// exactly "put one frame through every terminal", after which the pipeline
/// stays paused. The picture appears and stays where it is.
///
/// To the start rather than to where it was: where a clip is playing from is
/// not written down — see `SourceCommand::SetMediaPaused` for what is.
fn start(pipeline: &Arc<Pipeline>, settings: &MediaFileSettings) -> Result<(), BackendError> {
    let rate = settings.rate();
    // Backwards is played from the end: sought there and turned round while
    // paused, so nothing of the start plays forwards in between.
    if settings.paused || settings.backwards {
        pipeline.pause();
    }
    // A speed forwards is taken before the pipeline runs, so it starts at
    // it; backwards needs a running pipeline to turn.
    if !settings.backwards && rate != 1.0 {
        pipeline.set_rate(rate)?;
    }
    pipeline.run()?;
    if settings.backwards {
        // Reported and carried on, as below: a file that will not turn
        // shows where it is rather than failing to open.
        if let Some(end) = settings.duration
            && let Err(error) = pipeline.seek(end, media_pp::pipeline::SeekMode::Accurate)
        {
            tracing::warn!("could not go to the end to play backwards: {error}");
        }
        if let Err(error) = pipeline.set_rate(rate) {
            tracing::warn!("could not play backwards: {error}");
        }
        if !settings.paused {
            pipeline.resume();
        }
    } else if settings.paused
        && let Err(error) = pipeline.seek(
            std::time::Duration::ZERO,
            media_pp::pipeline::SeekMode::Keyframe,
        )
    {
        // Reported and carried on. What was lost is the first frame, so
        // the layer stays empty until someone presses play — which is a
        // Source that opened, not one that failed to.
        tracing::warn!("could not show the first frame while paused: {error}");
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
    let tee = context
        .tee("video-tee")
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
    gpu: &media_pp::elements::D3d11Gpu,
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
    let (demuxer, _) = FileDemuxer::open(name.clone(), &settings.path)?;
    let chosen = choose(&demuxer, mixer)?;

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
            gpu: gpu.clone(),
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
        gpu,
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
    start(&pipeline, settings)?;

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
            rate: settings.rate(),
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
    let (demuxer, _) = FileDemuxer::open(name.clone(), &settings.path)?;
    let chosen = choose(&demuxer, mixer)?;

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
    start(&pipeline, settings)?;

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
            rate: settings.rate(),
            looping: Some(looping),
            volume,
            meters,
            pipeline: Arc::clone(&pipeline),
            sound: routing,
        }),
    }))
}

/// A media file Source opened and sought the way the engine does it — the
/// seek `EngineCommand::MediaSeek` makes, the pause and resume a Source's
/// play button makes — on the pipeline `open` builds, with this machine's GPU
/// decoding and a mixer taking the sound. Each seek has to put one picture
/// through to the compositor and hold it while paused, or play on from it,
/// and nothing from before a seek may be shown after it.
// Both backends: the D3D11 compositor with D3D11VA on Windows, the CUDA one
// with NVDEC on Linux — each `open` as the engine calls it there.
#[cfg(all(test, any(target_os = "windows", target_os = "linux")))]
mod tests {
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    #[cfg(target_os = "windows")]
    use media_pp::elements::D3d11VideoCompositor;
    use media_pp::elements::{
        AudioCodec, AudioMixer, AudioMixerOptions, FileMuxer, SwAudioEncoder,
        SwAudioEncoderOptions, SwEncoder, SwEncoderOptions, SwScaler, TestAudioOptions,
        TestAudioSource, TestVideoOptions, TestVideoSource, VideoCodec, VideoCompositorOptions,
        VideoLayer, VideoRect,
    };
    #[cfg(target_os = "linux")]
    use media_pp::elements::{CudaDevice, CudaVideoCompositor};
    use media_pp::pipeline::{PipelineBuilder, SeekMode};

    use super::*;
    use crate::domain::{SceneItemId, SourceKind, SourceSettings, Transform};
    use crate::engine::source::OpenOutcome;

    const WIDTH: u32 = 320;
    const HEIGHT: u32 = 240;
    /// A keyframe every 40 frames at 30 a second: 1.333 s apart, so no round
    /// target lands on one and a keyframe seek always has somewhere earlier
    /// to go.
    const GOP: u32 = 40;
    const SECONDS: f64 = 8.0;

    /// A device, a compositor on it and a running mixer, as the engine has
    /// them when it opens a file — or a skip where there is no device.
    macro_rules! rig {
        ($gpu:ident, $compositor_element:ident, $compositor:ident, $mix:ident, $mixer_handle:ident) => {
            #[cfg(target_os = "windows")]
            let Ok($gpu) = crate::engine::backend::create_device() else {
                eprintln!("skipping: no Direct3D 11 device");
                return;
            };
            #[cfg(target_os = "linux")]
            let $gpu = match CudaDevice::new() {
                Ok(device) => Arc::new(device),
                Err(error) => {
                    eprintln!("skipping: no CUDA device ({error})");
                    return;
                }
            };
            let options = VideoCompositorOptions {
                width: WIDTH,
                height: HEIGHT,
                frame_rate: ffmpeg::Rational::new(30, 1),
                background: media_pp::color::Color::BLACK,
                background_alpha: 255,
            };
            #[cfg(target_os = "windows")]
            let ($compositor_element, $compositor) =
                D3d11VideoCompositor::new("test-compositor", &$gpu, options).expect("compositor");
            #[cfg(target_os = "linux")]
            let ($compositor_element, $compositor) =
                CudaVideoCompositor::new("test-compositor", &$gpu, options).expect("compositor");
            let (mixer, $mixer_handle) = AudioMixer::new(
                "test-mixer",
                AudioMixerOptions {
                    sample_rate: 48_000,
                    channels: 2,
                },
            );
            let ($mix, ()) = Pipeline::new("test-mix", mixer, |source, context| {
                let (branch, _tee) = context.tee("test-mix-tee").build_dynamic()?;
                context.attach(source, 0, branch)?;
                Ok(())
            })
            .expect("mixer pipeline");
            $mix.run().expect("run the mixer");
        };
    }

    /// Eight seconds of picture and tone, made here as media-pp's own tests
    /// make theirs: nothing of the kind is checked in. Made once a process.
    fn fixture() -> PathBuf {
        static MADE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
        MADE.get_or_init(|| {
            let directory = std::env::temp_dir().join("obs-rs-fixtures");
            std::fs::create_dir_all(&directory).expect("fixture directory");
            let path = directory.join(format!("media-file-seek.{}.mp4", std::process::id()));
            let rate = ffmpeg::Rational::new(30, 1);
            let video = TestVideoSource::new(
                "fixture-video",
                TestVideoOptions {
                    width: WIDTH,
                    height: HEIGHT,
                    frame_rate: rate,
                },
            );
            let audio = TestAudioSource::new(
                "fixture-audio",
                TestAudioOptions {
                    sample_rate: 48_000,
                    channels: 2,
                    frequency: 440.0,
                },
            );
            let video_encoder = SwEncoder::new(
                "fixture-video-encoder",
                SwEncoderOptions {
                    codec: VideoCodec::OpenH264,
                    width: WIDTH,
                    height: HEIGHT,
                    pixel_format: ffmpeg::format::Pixel::YUV420P,
                    frame_rate: rate,
                    bit_rate: 1_000_000,
                    gop_size: GOP,
                    max_b_frames: None,
                },
            )
            .expect("video encoder");
            let audio_encoder = SwAudioEncoder::new(
                "fixture-audio-encoder",
                SwAudioEncoderOptions {
                    codec: AudioCodec::Aac,
                    sample_rate: 48_000,
                    channels: 2,
                    bit_rate: 128_000,
                },
            )
            .expect("audio encoder");
            let mut muxer = FileMuxer::create(&path).expect("muxer");
            let video_track = muxer.add_stream("video", &video_encoder).expect("track");
            let audio_track = muxer.add_stream("audio", &audio_encoder).expect("track");
            let mut sinks = muxer.open().expect("open muxer");
            let video_sink = sinks.take(video_track).expect("video sink");
            let audio_sink = sinks.take(audio_track).expect("audio sink");
            let scaler = SwScaler::new(
                "fixture-to-yuv",
                ffmpeg::format::Pixel::YUV420P,
                WIDTH,
                HEIGHT,
                ffmpeg::software::scaling::Flags::BILINEAR,
            );
            let builder = PipelineBuilder::new("fixture");
            let (builder, ()) = builder
                .add_source(video, move |source, context| {
                    let branch = context
                        .branch()
                        .pipe(scaler)
                        .pipe(video_encoder)
                        .to(video_sink)?;
                    context.attach(source, 0, branch)?;
                    Ok(())
                })
                .expect("video branch");
            let (builder, ()) = builder
                .add_source(audio, move |source, context| {
                    let branch = context.branch().pipe(audio_encoder).to(audio_sink)?;
                    context.attach(source, 0, branch)?;
                    Ok(())
                })
                .expect("audio branch");
            let pipeline = builder.build();
            pipeline.run().expect("run the fixture");
            std::thread::sleep(Duration::from_secs_f64(SECONDS));
            pipeline.stop();
            drop(pipeline);
            path
        })
        .clone()
    }

    fn item(path: PathBuf) -> SceneItemSnapshot {
        SceneItemSnapshot {
            id: SceneItemId(1),
            name: "clip".into(),
            kind: SourceKind::MediaFile,
            settings: SourceSettings::MediaFile(MediaFileSettings {
                path,
                looping: false,
                size_hint: Some([WIDTH, HEIGHT]),
                has_audio: true,
                gain_db: 0.0,
                duration: None,
                paused: true,
                muted: false,
                monitored: false,
                speed_percent: 100,
                backwards: false,
            }),
            filters: Vec::new(),
            audio_filters: Vec::new(),
            source_size: [WIDTH as f32, HEIGHT as f32],
            visible: true,
            locked: false,
            transform: Transform::default(),
            crop: crate::domain::Crop::default(),
            opacity: 1.0,
            fades: Default::default(),
            peak_db: None,
            position: None,
        }
    }

    /// Where the picture has reached, in seconds — what the dock's progress
    /// bar reads — or `None` before the first frame.
    fn position(meters: &MediaMeters) -> Option<f64> {
        let micros = meters.position.load(Ordering::Relaxed);
        (micros >= 0).then(|| micros as f64 / 1e6)
    }

    /// Waits up to `limit` for `until` to hold.
    fn wait(limit: Duration, mut until: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + limit;
        while Instant::now() < deadline {
            if until() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        until()
    }

    /// Seeks as the engine does, and says how long it took.
    fn seek(pipeline: &Pipeline, target: f64) -> Duration {
        let started = Instant::now();
        pipeline
            .seek(Duration::from_secs_f64(target), SeekMode::Keyframe)
            .unwrap_or_else(|error| panic!("seek to {target}s: {error}"));
        started.elapsed()
    }

    /// Played at a speed the picture goes that much faster, and changed
    /// while it plays it carries on from where it is; turned round it goes
    /// down from there. Opened backwards, it plays from the end.
    #[test]
    fn a_file_plays_at_its_speed_and_backwards() {
        rig!(gpu, _compositor, compositor, mix, mixer_handle);
        let path = fixture();
        let open_as = |tweak: &dyn Fn(&mut MediaFileSettings)| {
            let mut item = item(path.clone());
            if let SourceSettings::MediaFile(settings) = &mut item.settings {
                settings.paused = false;
                settings.duration = Some(Duration::from_secs_f64(SECONDS));
                tweak(settings);
            }
            let outcome = open(
                &gpu,
                &compositor,
                Some(&mixer_handle),
                &MeterWake::new(|| {}),
                &item,
                VideoLayer::new(VideoRect::new(0, 0, WIDTH, HEIGHT)),
            )
            .expect("open the clip");
            let OpenOutcome::Open(source) = outcome else {
                panic!("the fixture is there");
            };
            (source, item)
        };
        // How fast the picture goes, as media seconds a second.
        let pace = |meters: &MediaMeters| {
            let (from, started) = (position(meters).unwrap(), Instant::now());
            std::thread::sleep(Duration::from_millis(800));
            (position(meters).unwrap() - from) / started.elapsed().as_secs_f64()
        };

        let (mut source, mut item) = open_as(&|_| {});
        let meters = Arc::clone(&source.media_file.as_ref().unwrap().meters);
        assert!(wait(Duration::from_secs(5), || position(&meters) > Some(0.3)));
        if let SourceSettings::MediaFile(settings) = &mut item.settings {
            settings.speed_percent = 200;
        }
        super::super::refresh_media_file(&mut source, &item, None);
        let media = source.media_file.as_ref().unwrap();
        assert_eq!(media.pipeline.rate(), 2.0);
        std::thread::sleep(Duration::from_millis(200));
        let twice = pace(&meters);
        assert!((1.6..2.4).contains(&twice), "{twice:.2}x at 200%");

        if let SourceSettings::MediaFile(settings) = &mut item.settings {
            settings.backwards = true;
        }
        super::super::refresh_media_file(&mut source, &item, None);
        assert_eq!(source.media_file.as_ref().unwrap().pipeline.rate(), -2.0);
        std::thread::sleep(Duration::from_millis(200));
        let back = pace(&meters);
        assert!((-2.4..-1.6).contains(&back), "{back:.2}x turned round");
        source.media_file.as_ref().unwrap().pipeline.stop();

        let (source, _item) = open_as(&|settings| settings.backwards = true);
        let meters = Arc::clone(&source.media_file.as_ref().unwrap().meters);
        assert!(
            wait(Duration::from_secs(5), || position(&meters)
                > Some(SECONDS - 2.0)),
            "from the end: {:?}",
            position(&meters)
        );
        let down = pace(&meters);
        assert!((-1.3..-0.7).contains(&down), "{down:.2}x backwards");
        source.media_file.as_ref().unwrap().pipeline.stop();
        mix.stop();
    }

    #[test]
    fn every_seek_puts_one_picture_through_and_holds_it_while_paused() {
        rig!(gpu, _compositor, compositor, mix, mixer_handle);
        let path = fixture();

        let outcome = open(
            &gpu,
            &compositor,
            Some(&mixer_handle),
            &MeterWake::new(|| {}),
            &item(path),
            VideoLayer::new(VideoRect::new(0, 0, WIDTH, HEIGHT)),
        )
        .expect("open the clip");
        let OpenOutcome::Open(source) = outcome else {
            panic!("the fixture is there");
        };
        let media = source.media_file.as_ref().expect("a media file");
        let pipeline = &media.pipeline;
        let meters = &media.meters;
        let shown = || source.layer.latest_frame().and_then(|frame| frame.pts());

        // Opened paused: one picture, the first, and it stays.
        assert!(
            wait(Duration::from_secs(5), || position(meters).is_some()
                && shown().is_some()),
            "opened paused, the compositor has a picture"
        );
        let first = position(meters).unwrap();
        assert!(first < 0.1, "the first picture, not {first}s");
        let held = shown();
        std::thread::sleep(Duration::from_millis(400));
        assert_eq!(position(meters), Some(first), "and holds it");
        assert_eq!(shown(), held);

        // Sought while paused: the keyframe before 3 s, which is 2.667 s,
        // and it stays.
        let took = seek(pipeline, 3.0);
        assert!(took < Duration::from_secs(2), "the preroll took {took:?}");
        let landed = position(meters).expect("a picture");
        assert!(
            (2.6..=3.0).contains(&landed),
            "a paused seek to 3s landed on {landed}s"
        );
        let held = shown();
        assert!(held.is_some() && held != Some(0));
        std::thread::sleep(Duration::from_millis(400));
        assert_eq!(position(meters), Some(landed), "held while paused");
        assert_eq!(shown(), held);

        // Played on from there.
        pipeline.resume();
        assert!(
            wait(Duration::from_secs(2), || position(meters)
                > Some(landed + 0.2)),
            "resumed, it plays on from {landed}s"
        );

        // Sought forward while playing: the keyframe before 5.5 s, playing on.
        let took = seek(pipeline, 5.5);
        assert!(took < Duration::from_secs(2), "the preroll took {took:?}");
        let landed = position(meters).expect("a picture");
        assert!(
            (5.3..=5.8).contains(&landed),
            "a playing seek to 5.5s landed on {landed}s"
        );
        assert!(
            wait(Duration::from_secs(2), || position(meters)
                > Some(landed + 0.2)),
            "and plays on"
        );

        // Sought back while playing: to the start, and nothing from the
        // stretch left behind comes after it.
        let took = seek(pipeline, 1.0);
        assert!(took < Duration::from_secs(2), "the preroll took {took:?}");
        let landed = position(meters).expect("a picture");
        assert!(landed < 1.1, "a playing seek to 1s landed on {landed}s");
        let deadline = Instant::now() + Duration::from_millis(600);
        while Instant::now() < deadline {
            let now = position(meters).unwrap();
            assert!(now < 2.5, "{now}s shown after a seek back to 1s");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(position(meters) > Some(landed), "and plays on");

        // Paused, then sought past the end: the last of it, held.
        pipeline.pause();
        let took = seek(pipeline, 30.0);
        assert!(took < Duration::from_secs(2), "the preroll took {took:?}");
        let landed = position(meters).expect("a picture");
        assert!(landed > 6.0, "a seek past the end landed on {landed}s");
        let held = shown();
        std::thread::sleep(Duration::from_millis(400));
        assert_eq!(position(meters), Some(landed), "held while paused");
        assert_eq!(shown(), held);

        pipeline.stop();
        mix.stop();
        // One file a process, and no other test reads it.
        drop(source);
        let _ = std::fs::remove_file(fixture());
    }
}
