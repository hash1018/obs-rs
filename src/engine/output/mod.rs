//! One recording: the file, and the two branches that write it.
//!
//! # Why this is not part of either half
//!
//! A recording is made of both. The video comes from the compositor's `Tee`,
//! which the [`Backend`] owns and which is platform-specific down to its
//! frame format; the audio comes from the mixer's `Tee`, which
//! [`crate::engine::audio`] owns and which is the same on every platform.
//! Neither can build the file, because a container's tracks are fixed before its
//! header is written — so *both* encoders have to exist, and have to be
//! described to the muxer, before a single frame is written to either.
//!
//! ```text
//! compositor Tee ─ Queue ─ PauseGate ─ [FrameRateLimiter] ─ H.264 ─ Origin ─┐
//!                                                                           ├─ muxer
//!     mixer Tee ─── Queue ─ PauseGate ─────────────────────── AAC ─ Origin ─┘
//! ```
//!
//! That is also why this owns both branches rather than leaving each half to
//! keep its own: **the trailer is written once every track has reported
//! done**, so ending one and not the other leaves the file exactly as long as
//! it is unplayable. One owner, one [`Recording::stop`], both tracks.
//!
//! # The two timelines
//!
//! Each track gets its own [`TimestampOrigin`], because there is no shared
//! clock to give them. The compositor counts composed frames and the mixer
//! counts emitted samples; both have been running since the application
//! started, on unrelated counters, and neither records when that was. So each
//! is zeroed at its own first packet, and the two agree only to within
//! whatever separates those — about one AAC frame, 1024 samples at 48 kHz, so
//! roughly 21 ms. That is inside the usual tolerance for lip sync and is
//! meant to be measured rather than assumed; closing it properly would mean
//! stamping both against one clock, which is a change to `media-pp` and not
//! to this.
//!
//! # Audio is optional, video is not
//!
//! A machine with no usable audio still records. The mixer failing to start
//! is not a reason to refuse a recording, so the track list is decided here,
//! per recording, from what is actually running.

pub(in crate::engine) mod disk;
mod session;
mod streaming;

pub(in crate::engine) use session::Broadcast;
pub(in crate::engine) use session::{OutputState, describe, start_recording};
pub(in crate::engine) use streaming::{BroadcastRequest, connect};

#[cfg_attr(target_os = "linux", path = "linux.rs")]
#[cfg_attr(target_os = "windows", path = "windows.rs")]
mod platform;

pub(in crate::engine) use platform::PreparedOutput;

use std::path::Path;
use std::time::Duration;

use media_pp::{
    element::Sink,
    elements::{
        AudioCodec, FileMuxer, HlsMode, HlsMuxer, HlsOptions, HlsSegmentFormat, MixFormat,
        MixerHandle, PauseGate, PauseGateHandle, SegmentPolicy, SegmentedFileMuxer, SwAudioEncoder,
        SwAudioEncoderOptions, TeeHandle, TimestampOrigin,
    },
    ffmpeg,
    graph::BranchId,
    queue::OverflowPolicy,
};

use super::audio::DEFAULT_MIX_FORMAT;
use super::backend::{Backend, BackendError, OUTPUT_QUEUE_DEPTH, OUTPUT_SEND_TIMEOUT, VideoTrack};
use crate::settings::{DEFAULT_AUDIO_BIT_RATE_KBPS, RecordingAudioCodec, RecordingSplit};

/// Which audio codecs this FFmpeg build can actually open, at the mix format
/// the encoder would be given.
///
/// Probed rather than assumed, the same as the video list and for the same
/// reason: `libopus` is an external library a stripped build may not carry,
/// and a dialog offering it there would be offering a recording that fails to
/// start. AAC is built in and is expected to be here, but it is opened too — a
/// list where one entry is checked and the other is trusted is a list that
/// lies about half of itself.
///
/// Cheap enough to do at startup: no device, no GPU, one `avcodec_open2` each.
pub(super) fn available_audio_codecs(format: MixFormat) -> Vec<RecordingAudioCodec> {
    RecordingAudioCodec::ALL
        .into_iter()
        .filter(|codec| {
            SwAudioEncoder::new(
                "probe-audio-encode",
                SwAudioEncoderOptions {
                    codec: media_codec(*codec),
                    sample_rate: format.sample_rate,
                    channels: format.channels,
                    time_base: ffmpeg::Rational::new(1, format.sample_rate as i32),
                    bit_rate: DEFAULT_AUDIO_BIT_RATE_KBPS as usize * 1_000,
                },
            )
            .is_ok()
        })
        .collect()
}

/// This crate's name for a codec, in `media-pp`'s.
fn media_codec(codec: RecordingAudioCodec) -> AudioCodec {
    match codec {
        RecordingAudioCodec::Aac => AudioCodec::Aac,
        RecordingAudioCodec::Opus => AudioCodec::Opus,
    }
}

/// Which of the two outputs a set of elements belongs to.
///
/// It names them, and that is the whole of what it is for: a failure inside
/// either reaches the bus as the *queue's* error rather than the muxer's —
/// a queue catches what its sink returned and reports it as its own — so the
/// only thing that says which output has stopped is what the queue is
/// called. Sharing one name between the two, which is what this replaced,
/// made a failed recording and a dropped broadcast indistinguishable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::engine) enum OutputKind {
    Recording,
    Broadcast,
}

impl OutputKind {
    /// What every element of this output is named after. Read back by
    /// `trouble::drain`, which is why the two live in one place.
    pub(in crate::engine) const fn prefix(self) -> &'static str {
        match self {
            Self::Recording => "record",
            Self::Broadcast => "stream",
        }
    }
}

/// What an output encodes with, whatever it does with the packets.
///
/// Read off a `RecordingSettings` or a `StreamingSettings` — see their
/// `encoding` methods — so that the encoders and the backend never have to
/// know which of the two they are serving. The two differ in where the
/// packets go and in nothing before that.
pub struct OutputEncoding {
    pub encoder: crate::settings::RecordingEncoder,
    /// The picture size to encode at, already resolved against the Canvas.
    pub size: [u32; 2],
    pub bit_rate_bits: usize,
    /// How often a keyframe is written, in seconds. A broadcast wants these
    /// often — a viewer joining cannot see anything until one arrives.
    pub keyframe_seconds: u32,
    pub audio_codec: crate::settings::RecordingAudioCodec,
    pub audio_bit_rate_kbps: u32,
}

/// One track an output will carry, as the muxer has to be told about it.
///
/// Every muxer here takes the same `add_stream(name, parameters, time_base)`,
/// so the tracks are described once and the choice below is only about which
/// muxer hears them.
pub(in crate::engine) struct TrackDef {
    pub(in crate::engine) name: String,
    pub(in crate::engine) parameters: ffmpeg::codec::Parameters,
    pub(in crate::engine) time_base: ffmpeg::Rational,
}

/// An output's tracks by what they carry: always a picture, and sound when
/// the mixer is running — see this module's docs.
///
/// The same shape at every step from description to sink — a [`TrackDef`],
/// the muxer's `MuxerTrack` for it, then the sink that opens from that — so
/// nothing between the two ends has to remember which position was which.
pub(in crate::engine) struct Tracks<T> {
    pub(in crate::engine) video: T,
    pub(in crate::engine) audio: Option<T>,
}

impl<T> Tracks<T> {
    /// Each track through `f`, the picture first: the order a muxer's header
    /// lists them in.
    fn map<U>(self, mut f: impl FnMut(T) -> U) -> Tracks<U> {
        Tracks {
            video: f(self.video),
            audio: self.audio.map(f),
        }
    }

    /// [`Tracks::map`], stopping at the first track `f` fails on.
    fn try_map<U, E>(self, mut f: impl FnMut(T) -> Result<U, E>) -> Result<Tracks<U>, E> {
        Ok(Tracks {
            video: f(self.video)?,
            audio: self.audio.map(f).transpose()?,
        })
    }
}

/// Opens whichever muxer the settings ask for, into one [`Sink`] per track.
///
/// Three cases, and the container is only one of them: `.mp4` and `.mkv` are
/// the same [`FileMuxer`] told a different path — FFmpeg guesses the muxer
/// from the file name — so what actually branches here is whether anything
/// cuts the recording into more than one file, and who does the cutting.
fn open_muxer(
    path: &Path,
    settings: &crate::settings::RecordingSettings,
    tracks: Tracks<TrackDef>,
) -> Result<Tracks<Box<dyn Sink>>, BackendError> {
    // Nothing else makes it, and no muxer will: the recordings folder before
    // the first recording, a folder chosen in Settings that does not exist
    // yet, and for HLS the recording's own directory, which is new every time.
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let split = settings.effective_split();
    if settings.format.segments_itself() {
        return open_hls_muxer(path, tracks);
    }
    let policy = match split {
        RecordingSplit::Off => None,
        RecordingSplit::Time => Some(SegmentPolicy::Duration(Duration::from_secs(
            settings.split_minutes_clamped() as u64 * 60,
        ))),
        RecordingSplit::Size => Some(SegmentPolicy::Size(settings.split_bytes())),
    };
    let Some(policy) = policy else {
        let mut muxer = FileMuxer::create(path)?;
        let added = tracks
            .try_map(|track| muxer.add_stream(track.name, track.parameters, track.time_base))?;
        let mut sinks = muxer.open()?;
        return Ok(added.try_map(|track| sinks.take(track))?);
    };

    // `path` named the recording; each segment is that name with its index
    // before the extension, so a listing keeps them together and in order.
    // Three digits because a thousand segments of the shortest split this
    // offers is more than ten days of recording.
    let directory = path.parent().unwrap_or(Path::new(".")).to_path_buf();
    let stem = path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let extension = settings.format.extension().to_owned();
    let mut muxer = SegmentedFileMuxer::create(policy, move |index| {
        directory.join(format!("{stem}_{index:03}.{extension}"))
    });
    let added = tracks.map(|track| muxer.add_stream(track.name, track.parameters, track.time_base));
    let mut sinks = muxer.open()?;
    Ok(added.try_map(|track| sinks.take(track))?)
}

/// The HLS case: a playlist at `path` and its segments beside it.
///
/// [`HlsMode::Vod`] rather than a live window, because this is a recording:
/// a sliding window would delete the beginning of it while the end was still
/// being written. fMP4 segments rather than MPEG-TS for the audio's sake —
/// Opus is one of the codecs this application offers, and TS carries it
/// badly where fMP4 carries it as readily as AAC.
///
/// The segment length is fixed rather than settable. Six seconds is what
/// Apple's own guidance asks for, and this is a recording format here rather
/// than a delivery one — nothing in this application is tuning latency
/// against segment count.
fn open_hls_muxer(
    path: &Path,
    tracks: Tracks<TrackDef>,
) -> Result<Tracks<Box<dyn Sink>>, BackendError> {
    let directory = path.parent().unwrap_or(Path::new("."));
    let mut muxer = HlsMuxer::create(HlsOptions {
        playlist_path: path.to_path_buf(),
        segment_pattern: directory.join("segment_%05d.m4s"),
        segment_duration: HLS_SEGMENT_DURATION,
        mode: HlsMode::Vod,
        segment_format: HlsSegmentFormat::Fmp4,
        init_filename: String::from("init.mp4"),
        base_url: None,
    })?;
    let added =
        tracks.try_map(|track| muxer.add_stream(track.name, track.parameters, track.time_base))?;
    let mut sinks = muxer.open()?;
    Ok(added.try_map(|track| sinks.take(track))?)
}

/// See [`open_hls_muxer`] for why this is a constant.
const HLS_SEGMENT_DURATION: Duration = Duration::from_secs(6);

/// One running output — a file being written, or a broadcast being
/// published — and everything needed to end it.
///
/// Both are the same two branches: the compositor's `Tee` through a video
/// encoder, and the mixer's through an audio one, meeting at a muxer. What
/// differs is only which muxer, which is why [`Output::start`] takes one
/// rather than opening it.
pub(in crate::engine) struct Output {
    video: Option<VideoTrack>,
    audio: Option<AudioTrack>,
}

/// The output's audio branch, on the mixer's `Tee`.
struct AudioTrack {
    /// Cloned rather than borrowed: this outlives the call that made it, and
    /// the mixer's `Tee` is reached from nowhere else on this thread.
    tee: TeeHandle,
    branch: BranchId,
    pause: PauseGateHandle,
}

impl Output {
    /// Opens both encoders, hands their descriptions to `open_muxer`, and
    /// starts both branches writing into what it returns.
    ///
    /// `fps` is the compositor's own rate; what the output is written at
    /// comes from `encoding` and can be less. `mixer` is `None` on a machine
    /// whose mixer never started, which yields a video-only output rather
    /// than an error.
    ///
    /// # Why the muxer arrives as a function
    ///
    /// Because a container's tracks are fixed before its header is written,
    /// and the tracks cannot be described until both encoders are open. So
    /// the muxer cannot be opened by the caller — it does not yet know what
    /// to declare — and it cannot be opened here either, because a file and
    /// a broadcast are opened in entirely different ways. What this knows is
    /// the moment between the two, which is what it hands over.
    pub(in crate::engine) fn start(
        backend: &Backend,
        mixer: Option<&(TeeHandle, MixerHandle)>,
        kind: OutputKind,
        fps: u32,
        encoding: &OutputEncoding,
        open_muxer: impl FnOnce(Tracks<TrackDef>) -> Result<Tracks<Box<dyn Sink>>, BackendError>,
    ) -> Result<Self, BackendError> {
        // Both encoders open before the file does. An encoder that cannot be
        // opened must not leave a zero-length mp4 behind, and the audio one
        // is the more likely of the two to refuse.
        let video: PreparedOutput = backend.prepare_output(kind, fps, encoding)?;
        // The format the mixer is actually summing into, not what the settings
        // asked for: one it refused leaves the old one running, and a track
        // opened for a format nothing is producing is samples that do not fit
        // their own header.
        let mix = mixer
            .and_then(|(_, handle)| handle.mix_format())
            .unwrap_or(DEFAULT_MIX_FORMAT);
        let audio_time_base = ffmpeg::Rational::new(1, mix.sample_rate as i32);
        let audio = mixer
            .map(|(tee, _)| tee)
            .map(|tee| -> Result<_, BackendError> {
                Ok((
                    tee,
                    SwAudioEncoder::new(
                        format!("{}-audio-encode", kind.prefix()),
                        SwAudioEncoderOptions {
                            codec: media_codec(encoding.audio_codec),
                            sample_rate: mix.sample_rate,
                            channels: mix.channels,
                            time_base: audio_time_base,
                            bit_rate: encoding.audio_bit_rate_kbps.max(1) as usize * 1_000,
                        },
                    )?,
                ))
            })
            .transpose()?;

        // Named after the output like everything else in it: a muxer track is
        // the element a failure is now reported under — `media-pp` traces an
        // error back to whatever raised it — and `video` alone would not say
        // which of two running outputs had stopped.
        let tracks = Tracks {
            video: TrackDef {
                name: format!("{}-video", kind.prefix()),
                parameters: video.parameters(),
                time_base: video.time_base(),
            },
            audio: audio.as_ref().map(|(_, encoder)| TrackDef {
                name: format!("{}-audio", kind.prefix()),
                parameters: encoder.parameters(),
                time_base: audio_time_base,
            }),
        };
        let Tracks {
            video: video_sink,
            audio: audio_sink,
        } = open_muxer(tracks)?;

        let audio = match (audio, audio_sink) {
            (Some((tee, encoder)), Some(sink)) => {
                let (gate, pause) = PauseGate::for_audio(format!("{}-audio-pause", kind.prefix()));
                let branch = tee
                    .branch()
                    .ok_or("the mixer's Tee is gone")?
                    // The same thread boundary the video branch has, and for
                    // the same reason: encoding and muxing must not be done
                    // on the mixer's own thread, where a slow write would
                    // stall the mix everything else is listening to.
                    .queue_with_policy(
                        format!("{}-audio-queue", kind.prefix()),
                        OUTPUT_QUEUE_DEPTH,
                        OverflowPolicy::Block(OUTPUT_SEND_TIMEOUT),
                    )
                    .pipe(gate)
                    .pipe(encoder)
                    // The mixer has been running since the application
                    // started and its timeline says so, exactly as the
                    // compositor's does.
                    .pipe(TimestampOrigin::new(format!(
                        "{}-audio-origin",
                        kind.prefix()
                    )))
                    .to(sink)?;
                Some(AudioTrack {
                    branch: tee.attach(branch)?,
                    tee: tee.clone(),
                    pause,
                })
            }
            (None, None) => None,
            // Every opener above answers through `Tracks::map`, which keeps
            // whether there is sound; one that did not would leave a track
            // declared in the header with nothing ever to finish it.
            _ => return Err("the muxer answered for a different set of tracks".into()),
        };

        // Attached last, so a failure above leaves no track running: the
        // video branch is the one that cannot be un-attached without
        // finalizing the file.
        let video = match backend.attach_output(kind, video, video_sink) {
            Ok(video) => video,
            Err(error) => {
                // Whatever was already writing has to be ended, or the file
                // is left open by a branch nothing holds.
                if let Some(audio) = audio {
                    let _ = audio.tee.finish_branch(audio.branch);
                }
                return Err(error);
            }
        };

        Ok(Self {
            video: Some(video),
            audio,
        })
    }

    /// Stops or resumes writing, on every track at once.
    ///
    /// Both gates are told in the same breath because a file whose tracks
    /// removed different spans is one whose audio has drifted from its
    /// picture. Each measures the pause in its own timeline, so they agree to
    /// within a tick of each — about 16 ms for video at 60 fps and a
    /// millisecond for audio — and that much accumulates across repeated
    /// pauses rather than cancelling out.
    pub(in crate::engine) fn set_paused(&self, paused: bool) {
        if let Some(video) = &self.video {
            video.pause.set_paused(paused);
        }
        if let Some(audio) = &self.audio {
            audio.pause.set_paused(paused);
        }
    }

    /// Ends every track, which is what finalizes the file.
    ///
    /// Every one of them: the trailer is written when the last track reports
    /// done, so a failure on one track is not a reason to skip the other —
    /// that would leave the mp4 unplayable rather than merely truncated. The
    /// first error is reported after both have been tried.
    pub(in crate::engine) fn stop(mut self, backend: &Backend) -> Result<(), BackendError> {
        let mut failure = None;
        if let Some(audio) = self.audio.take()
            && let Err(error) = audio.tee.finish_branch(audio.branch)
        {
            failure = Some(BackendError::from(error));
        }
        if let Some(video) = self.video.take()
            && let Err(error) = backend.detach_output(video)
        {
            failure = failure.or(Some(error));
        }
        match failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{RecordingFormat, RecordingSettings};

    /// One track any of these muxers takes. AAC, because it is built into
    /// every FFmpeg; standing in for the picture is fine, since a muxer is
    /// told a track's parameters and nothing about what they are for.
    fn one_track() -> Tracks<TrackDef> {
        let time_base = ffmpeg::Rational::new(1, 48_000);
        let encoder = SwAudioEncoder::new(
            "test-audio-encode",
            SwAudioEncoderOptions {
                codec: AudioCodec::Aac,
                sample_rate: 48_000,
                channels: 2,
                time_base,
                bit_rate: 128_000,
            },
        )
        .expect("AAC is built into FFmpeg");
        Tracks {
            video: TrackDef {
                name: String::from("test-track"),
                parameters: encoder.parameters(),
                time_base,
            },
            audio: None,
        }
    }

    /// A recording goes into a directory nothing may have made yet — for HLS
    /// always, since each recording is a directory of its own. When this was
    /// left to nobody, every HLS recording failed to start, on `init.mp4`.
    #[test]
    fn a_recording_makes_the_directory_it_is_written_into() {
        for format in [RecordingFormat::Mp4, RecordingFormat::Hls] {
            let root = std::env::temp_dir().join(format!(
                "obs-rs-output-dir-{}-{format:?}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&root);
            let settings = RecordingSettings {
                format,
                ..RecordingSettings::default()
            };
            let path = crate::paths::recording_file_in(
                &root,
                "test",
                time::OffsetDateTime::UNIX_EPOCH,
                format,
            );

            let sinks = open_muxer(&path, &settings, one_track());
            assert!(sinks.is_ok(), "{format:?}: {:?}", sinks.err());
            assert!(path.parent().is_some_and(Path::is_dir), "{format:?}");
            drop(sinks);
            let _ = std::fs::remove_dir_all(&root);
        }
    }
}
