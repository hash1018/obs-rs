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

use media_pp::element::BoxSink;
use media_pp::element::Context;
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
///
/// Backwards, the file is not looped while it is sought to its end: reaching
/// the end there would start it again, and the picture the seek shows — the
/// one playback turns round from — would be the start of a second lap.
fn start(
    pipeline: &Arc<Pipeline>,
    settings: &MediaFileSettings,
    looping: &FileDemuxerHandle,
) -> Result<(), BackendError> {
    let rate = settings.rate();
    // Backwards is played from the end: sought there and turned round while
    // paused, so nothing of the start plays forwards in between.
    if settings.paused || settings.backwards {
        pipeline.pause();
    }
    if settings.backwards {
        looping.set_looping(false);
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
        looping.set_looping(settings.looping);
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

/// Starts a looping file played backwards again from its end, where it has
/// played back to its start. Answers whether it did, which is the only case
/// a file played to its start does not end.
///
/// The file's own demuxer carries backwards over the start of every lap but
/// the first, as it carries forwards over the end of every one, with the
/// timeline going down; before the first lap there is no timeline left to go
/// down into, and the stream ends. Going round from there is a seek to the
/// end — a moment's preroll, where a forward lap joins without one.
pub(in crate::engine) fn go_round_backwards(
    media: &super::MediaFile,
    settings: &MediaFileSettings,
) -> bool {
    let (true, true, Some(end)) = (settings.looping, settings.backwards, settings.duration) else {
        return false;
    };
    let mut finished = false;
    while let Some(message) = media.pipeline.bus().try_recv_message() {
        finished |= matches!(message.event, media_pp::bus::BusEvent::Finished);
    }
    if !finished {
        return false;
    }
    if let Err(error) = media
        .pipeline
        .seek(end, media_pp::pipeline::SeekMode::Accurate)
    {
        tracing::warn!("could not go round to the end again: {error}");
        return false;
    }
    true
}

/// The sink that records where playback has reached, and how it is wired.
///
/// On the *video* branch rather than the audio one, because every media file
/// has a picture and only some have sound — and because what a progress bar
/// means is where the picture is.
///
/// Each frame's own lap is taken off here, from its timestamp, rather than
/// the lap the demuxer is reading: that one is ahead of the picture by what
/// the queues and the decoder hold — seconds of the file, played fast — and
/// the bar went blank at the end of every lap forwards, and read a lap too
/// far at the start of one backwards.
fn position_sink(
    name: &str,
    time_base: ffmpeg::Rational,
    looping: FileDemuxerHandle,
    meters: Arc<MediaMeters>,
) -> BoxSink {
    let micros = f64::from(time_base.numerator()) / f64::from(time_base.denominator()) * 1e6;
    BoxSink::new(AppSink::new(format!("{name}-position"), move |buffer| {
        if let media_pp::buffer::MediaBuffer::Video(frame) = &buffer
            && let Some(pts) = frame.pts()
        {
            let at = (pts as f64 * micros).max(0.0) as u64;
            let in_lap = looping.in_lap(std::time::Duration::from_micros(at));
            meters
                .position
                .store(in_lap.as_micros() as i64, Ordering::Relaxed);
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
    decoder: impl media_pp::element::RawFilter + 'static,
    picture: PictureEnd,
    position: BoxSink,
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
    start(&pipeline, settings, &looping)?;

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

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(in crate::engine) fn open(
    gpu: &crate::engine::backend::Gpu,
    handle: &crate::engine::backend::Compositor,
    mixer: Option<&MixerHandle>,
    meter_wake: &MeterWake,
    item: &SceneItemSnapshot,
    layer: media_pp::elements::VideoLayer,
) -> Result<super::OpenOutcome, BackendError> {
    use media_pp::elements::{CompositorInput, VideoDecodeBin};

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

    // NV12 on the GPU, decoded there or uploaded after a software decode,
    // is one of the two the compositor draws from — so there is no
    // converter here, unlike the Sources that upload BGRA of their own;
    // a file with alpha arrives as BGRA, the other one.
    // Read before the parameters are moved into the decoder, which is
    // also the only place they describe a picture rather than a stream.
    let size = super::decoded_size(&chosen.video_params);
    let codec = chosen.video_params.id();
    let threading = decode_policy::threading(codec, size, Playback::File);
    let video_decoder = VideoDecodeBin::open(
        format!("{name}-video"),
        chosen.video_params,
        gpu.decode_target(HW_FRAME_BUDGET),
        threading,
    )?;
    decode_policy::log(&item.name, codec, size, threading, &video_decoder);
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

    // No `Option` here, unlike the Direct3D half: a Linux compositor
    // answers with the input itself or with an error.
    let CompositorInput { sink, layer } = handle.add_source(name.clone(), layer)?;

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
    start(&pipeline, settings, &looping)?;

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
// Every backend: the D3D11 compositor with D3D11VA on Windows, CUDA or
// Vulkan on Linux, Metal with VideoToolbox on macOS — each `open` as the
// engine calls it there.
#[cfg(all(
    test,
    any(target_os = "windows", target_os = "linux", target_os = "macos")
))]
mod tests {
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use media_pp::buffer::MediaBuffer;
    use media_pp::bus::BusEvent;
    #[cfg(target_os = "windows")]
    use media_pp::elements::D3d11VideoCompositor;
    use media_pp::elements::{
        AppSource, AudioCodec, AudioMixer, AudioMixerOptions, FileMuxer, SwAudioEncoder,
        SwAudioEncoderOptions, SwEncoder, SwEncoderOptions, VideoCodec, VideoCompositorOptions,
        VideoLayer, VideoRect,
    };
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
            // Before the device, so it is let go of after it.
            #[cfg(target_os = "linux")]
            let _turn = crate::engine::backend::Gpu::test_turn(
                crate::engine::backend::Gpu::asked_for_vulkan(),
            );
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            let $gpu = match crate::engine::backend::Gpu::open() {
                Ok(gpu) => gpu,
                Err(error) => {
                    eprintln!("skipping: no GPU ({error})");
                    return;
                }
            };
            let options = VideoCompositorOptions {
                mode: media_pp::elements::RenderMode::Live,
                width: WIDTH,
                height: HEIGHT,
                frame_rate: ffmpeg::Rational::new(30, 1),
                background: media_pp::color::Color::BLACK,
                background_alpha: 255,
            };
            #[cfg(target_os = "windows")]
            let ($compositor_element, $compositor) =
                D3d11VideoCompositor::new("test-compositor", &$gpu, options).expect("compositor");
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            let ($compositor_element, $compositor) = $gpu
                .compositor(
                    "test-compositor".to_owned(),
                    options,
                    crate::engine::source::filters::ChainFormat::Nv12,
                )
                .expect("compositor");
            let (mixer, $mixer_handle) = AudioMixer::new(
                "test-mixer",
                AudioMixerOptions {
                    mode: media_pp::elements::RenderMode::Live,
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

    /// The fixture while a test holds it — see [`fixture`].
    struct Fixture(PathBuf);

    /// How many tests hold the fixture, and where it is while any does.
    static HELD: std::sync::Mutex<(usize, Option<PathBuf>)> = std::sync::Mutex::new((0, None));

    /// Eight seconds of picture and tone, made here as media-pp's own tests
    /// make theirs: nothing of the kind is checked in.
    ///
    /// Made by the first test to ask and shared, then removed when the last
    /// one holding it lets go. Both tests here read it and either can finish
    /// first, so the one that ends cannot simply remove it: on a runner where
    /// the seek test finished before the speed test's last open, that open
    /// found no file.
    fn fixture() -> Fixture {
        let mut held = HELD.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let path = held.1.get_or_insert_with(make_fixture).clone();
        held.0 += 1;
        Fixture(path)
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let mut held = HELD.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            held.0 -= 1;
            if held.0 == 0 {
                let _ = std::fs::remove_file(&self.0);
                held.1 = None;
            }
        }
    }

    /// Every picture and every sample is made here and handed over as fast
    /// as the encoders take them, so the file is the same however busy the
    /// machine is. Recorded from live test sources for `SECONDS` instead, a
    /// loaded runner made fewer pictures in that time: a file of a second or
    /// two, whose keyframes and end were not where the tests look for them.
    fn make_fixture() -> PathBuf {
        let directory = std::env::temp_dir().join("obs-rs-fixtures");
        std::fs::create_dir_all(&directory).expect("fixture directory");
        let path = directory.join(format!("media-file-seek.{}.mp4", std::process::id()));
        let rate = ffmpeg::Rational::new(30, 1);
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
                sample_rate: SAMPLE_RATE,
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
        let (video, pictures) = AppSource::new("fixture-video", 4);
        let (audio, sound) = AppSource::new("fixture-audio", 4);
        let builder = PipelineBuilder::new("fixture");
        let (builder, ()) = builder
            .add_source(video, move |source, context| {
                let branch = context.branch().pipe(video_encoder).to(video_sink)?;
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

        // Each picture's sound just ahead of it, worked out from the total
        // so far so that it stays exact.
        let frames = (SECONDS * 30.0).round() as i64;
        let mut samples = 0;
        for index in 0..frames {
            let owed = (index + 1) * i64::from(SAMPLE_RATE) / 30;
            sound
                .push(MediaBuffer::Audio(
                    Arc::new(tone(samples, owed - samples)).into(),
                ))
                .expect("push sound");
            samples = owed;
            pictures
                .push(MediaBuffer::video(picture(index, rate)))
                .expect("push picture");
        }
        pictures.finish().expect("end the picture");
        sound.finish().expect("end the sound");
        let finished = loop {
            match pipeline.bus().recv_timeout(Duration::from_secs(30)) {
                Ok(BusEvent::Finished) => break true,
                Ok(BusEvent::Error { error, .. }) => panic!("writing the fixture: {error}"),
                Ok(_) => {}
                Err(_) => break false,
            }
        };
        assert!(finished, "the fixture was written");
        pipeline.stop();
        drop(pipeline);
        path
    }

    const SAMPLE_RATE: u32 = 48_000;

    /// Picture `index` of the fixture: a diagonal ramp that moves a step a
    /// picture, so no two are alike.
    fn picture(index: i64, rate: ffmpeg::Rational) -> ffmpeg::frame::Video {
        let mut frame = ffmpeg::frame::Video::new(ffmpeg::format::Pixel::YUV420P, WIDTH, HEIGHT);
        let stride = frame.stride(0);
        let plane = frame.data_mut(0);
        for row in 0..HEIGHT as usize {
            for col in 0..WIDTH as usize {
                plane[row * stride + col] = ((col as i64 + row as i64 + index) % 256) as u8;
            }
        }
        frame.data_mut(1).fill(128);
        frame.data_mut(2).fill(128);
        frame.set_pts(Some(index));
        media_pp::buffer::set_time_base(&mut frame, rate.invert());
        frame
    }

    /// `count` samples of a 440 Hz tone from sample `start` on, as the
    /// fixture's sound.
    fn tone(start: i64, count: i64) -> ffmpeg::frame::Audio {
        let mut frame = ffmpeg::frame::Audio::new(
            ffmpeg::format::Sample::F32(ffmpeg::format::sample::Type::Packed),
            count as usize,
            ffmpeg::ChannelLayout::default(2),
        );
        frame.set_rate(SAMPLE_RATE);
        let bytes = frame.data_mut(0);
        for index in 0..count as usize {
            let t = (start + index as i64) as f64 / f64::from(SAMPLE_RATE);
            let sample = ((t * 440.0 * std::f64::consts::TAU).sin() as f32).to_ne_bytes();
            bytes[index * 8..index * 8 + 4].copy_from_slice(&sample);
            bytes[index * 8 + 4..index * 8 + 8].copy_from_slice(&sample);
        }
        frame.set_pts(Some(start));
        media_pp::buffer::set_time_base(&mut frame, ffmpeg::Rational::new(1, SAMPLE_RATE as i32));
        frame
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
        let fixture = fixture();
        let path = fixture.0.clone();
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

    /// Looping backwards is looping: opened that way it plays from the end,
    /// and at the start it goes round to the end again rather than ending.
    #[test]
    fn a_looping_file_played_backwards_goes_round_from_its_start() {
        rig!(gpu, _compositor, compositor, mix, mixer_handle);
        let fixture = fixture();
        let mut item = item(fixture.0.clone());
        if let SourceSettings::MediaFile(settings) = &mut item.settings {
            settings.paused = false;
            settings.looping = true;
            settings.backwards = true;
            settings.speed_percent = 400;
            settings.duration = Some(Duration::from_secs_f64(SECONDS));
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
        let media = source.media_file.as_ref().unwrap();
        let settings = match &item.settings {
            SourceSettings::MediaFile(settings) => settings.clone(),
            _ => unreachable!(),
        };
        let meters = Arc::clone(&media.meters);
        assert!(
            wait(Duration::from_secs(5), || position(&meters)
                > Some(SECONDS - 2.0)),
            "from the end: {:?}",
            position(&meters)
        );
        // Down to the start at four times its speed, then round — as the
        // engine's loop sends it round — somewhere near the end again, and
        // still going down.
        assert!(
            wait(Duration::from_secs(5), || position(&meters) < Some(2.0)),
            "down to the start: {:?}",
            position(&meters)
        );
        assert!(
            wait(Duration::from_secs(5), || {
                go_round_backwards(media, &settings);
                position(&meters) > Some(SECONDS - 3.0)
            }),
            "round from the start: {:?}",
            position(&meters)
        );
        let (from, started) = (position(&meters).unwrap(), Instant::now());
        std::thread::sleep(Duration::from_millis(400));
        let pace = (position(&meters).unwrap() - from) / started.elapsed().as_secs_f64();
        assert!((-4.8..-3.2).contains(&pace), "{pace:.2}x still backwards");
        media.pipeline.stop();
        mix.stop();
    }

    /// Sought on a lap after the first, a looping file shows where it was
    /// sought to and plays on from there at once.
    #[test]
    fn a_looping_file_sought_on_a_later_lap_plays_on_from_there() {
        rig!(gpu, _compositor, compositor, mix, mixer_handle);
        let fixture = fixture();
        let mut item = item(fixture.0.clone());
        if let SourceSettings::MediaFile(settings) = &mut item.settings {
            settings.paused = false;
            settings.looping = true;
            settings.speed_percent = 400;
            settings.duration = Some(Duration::from_secs_f64(SECONDS));
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
        let media = source.media_file.as_ref().unwrap();
        let meters = Arc::clone(&media.meters);
        assert!(wait(Duration::from_secs(5), || position(&meters)
            > Some(SECONDS - 3.0)));
        assert!(wait(Duration::from_secs(5), || position(&meters) < Some(1.0)));
        assert!(wait(Duration::from_secs(5), || position(&meters) > Some(2.0)));
        seek(&media.pipeline, 5.0);
        std::thread::sleep(Duration::from_millis(100));
        let (from, started) = (position(&meters), Instant::now());
        assert!(
            from.is_some_and(|from| (3.9..6.5).contains(&from)),
            "sought to 5s, shows {from:?}"
        );
        std::thread::sleep(Duration::from_millis(300));
        let pace =
            (position(&meters).unwrap_or(0.0) - from.unwrap()) / started.elapsed().as_secs_f64();
        assert!((3.2..4.8).contains(&pace), "{pace:.2}x after the seek");
        media.pipeline.stop();
        mix.stop();
    }

    /// Turned round on a lap after the first, a looping file goes back from
    /// where it is in that lap — not from the file's end, and not after a
    /// wait.
    #[test]
    fn a_looping_file_turned_round_on_a_later_lap_goes_back_from_where_it_is() {
        rig!(gpu, _compositor, compositor, mix, mixer_handle);
        let fixture = fixture();
        let mut item = item(fixture.0.clone());
        if let SourceSettings::MediaFile(settings) = &mut item.settings {
            settings.paused = false;
            settings.looping = true;
            settings.speed_percent = 400;
            settings.duration = Some(Duration::from_secs_f64(SECONDS));
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
        let OpenOutcome::Open(mut source) = outcome else {
            panic!("the fixture is there");
        };
        let meters = Arc::clone(&source.media_file.as_ref().unwrap().meters);
        // Past the end once, and a few seconds into the second lap.
        assert!(wait(Duration::from_secs(5), || position(&meters)
            > Some(SECONDS - 3.0)));
        assert!(wait(Duration::from_secs(5), || position(&meters) < Some(1.0)));
        assert!(wait(Duration::from_secs(5), || position(&meters) > Some(4.0)));
        let turned_at = position(&meters).unwrap();
        if let SourceSettings::MediaFile(settings) = &mut item.settings {
            settings.backwards = true;
        }
        super::super::refresh_media_file(&mut source, &item, None);
        std::thread::sleep(Duration::from_millis(300));
        let (from, started) = (position(&meters).unwrap(), Instant::now());
        assert!(
            (turned_at - 2.0..turned_at + 0.5).contains(&from),
            "went back from {from:.2}s, turned at {turned_at:.2}s"
        );
        std::thread::sleep(Duration::from_millis(400));
        let pace = (position(&meters).unwrap() - from) / started.elapsed().as_secs_f64();
        assert!((-4.8..-3.2).contains(&pace), "{pace:.2}x backwards");
        source.media_file.as_ref().unwrap().pipeline.stop();
        mix.stop();
    }

    #[test]
    fn every_seek_puts_one_picture_through_and_holds_it_while_paused() {
        rig!(gpu, _compositor, compositor, mix, mixer_handle);
        let fixture = fixture();
        let path = fixture.0.clone();

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
    }
}
