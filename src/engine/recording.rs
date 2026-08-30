//! One recording: the file, and the two branches that write it.
//!
//! # Why this is not part of either half
//!
//! A recording is made of both. The video comes from the compositor's `Tee`,
//! which the [`Backend`] owns and which is platform-specific down to its
//! frame format; the audio comes from the mixer's `Tee`, which
//! [`crate::engine::audio`] owns and which is the same on every platform.
//! Neither can build the file, because an mp4's tracks are fixed before its
//! header is written — so *both* encoders have to exist, and have to be
//! described to the muxer, before a single frame is written to either.
//!
//! ```text
//! compositor Tee ─ Queue ─ PauseGate ─ [FrameRateLimiter] ─ H.264 ─ Origin ─┐
//!                                                                           ├─ Mp4Muxer
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

use std::path::Path;

use media_pp::{
    elements::{
        AudioCodec, Mp4Muxer, PauseGate, PauseGateHandle, SwAudioEncoder, SwAudioEncoderOptions,
        TeeHandle, TimestampOrigin,
    },
    ffmpeg,
    graph::BranchId,
    queue::OverflowPolicy,
};

use super::audio::{MIX_CHANNELS, MIX_SAMPLE_RATE};
use super::backend::{
    Backend, BackendError, PreparedRecording, RECORDING_QUEUE_DEPTH, RECORDING_SEND_TIMEOUT,
    VideoTrack,
};
use crate::settings::{DEFAULT_AUDIO_BIT_RATE_KBPS, RecordingAudioCodec};

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
pub(super) fn available_audio_codecs() -> Vec<RecordingAudioCodec> {
    RecordingAudioCodec::ALL
        .into_iter()
        .filter(|codec| {
            SwAudioEncoder::new(
                "probe-audio-encode",
                SwAudioEncoderOptions {
                    codec: media_codec(*codec),
                    sample_rate: MIX_SAMPLE_RATE,
                    channels: MIX_CHANNELS,
                    time_base: ffmpeg::Rational::new(1, MIX_SAMPLE_RATE as i32),
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

/// A recording that is running, and everything needed to end it.
pub(super) struct Recording {
    video: Option<VideoTrack>,
    audio: Option<AudioTrack>,
}

/// The recording's audio branch, on the mixer's `Tee`.
struct AudioTrack {
    /// Cloned rather than borrowed: this outlives the call that made it, and
    /// the mixer's `Tee` is reached from nowhere else on this thread.
    tee: TeeHandle,
    branch: BranchId,
    pause: PauseGateHandle,
}

impl Recording {
    /// Opens `path` and starts both tracks writing into it.
    ///
    /// `fps` is the compositor's own rate; what the file is written at comes
    /// from `settings` and can be less. `mixer_tee` is `None` on a machine
    /// whose mixer never started, which yields a video-only file rather than
    /// an error.
    pub(super) fn start(
        backend: &Backend,
        mixer_tee: Option<&TeeHandle>,
        path: &Path,
        fps: u32,
        settings: &crate::settings::RecordingSettings,
    ) -> Result<Self, BackendError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Both encoders open before the file does. An encoder that cannot be
        // opened must not leave a zero-length mp4 behind, and the audio one
        // is the more likely of the two to refuse.
        let video: PreparedRecording = backend.prepare_recording(fps, settings)?;
        let audio_time_base = ffmpeg::Rational::new(1, MIX_SAMPLE_RATE as i32);
        let audio = mixer_tee
            .map(|tee| -> Result<_, BackendError> {
                Ok((
                    tee,
                    SwAudioEncoder::new(
                        "record-audio-encode",
                        SwAudioEncoderOptions {
                            codec: media_codec(settings.audio_codec),
                            sample_rate: MIX_SAMPLE_RATE,
                            channels: MIX_CHANNELS,
                            time_base: audio_time_base,
                            bit_rate: settings.audio_bit_rate_kbps.max(1) as usize * 1_000,
                        },
                    )?,
                ))
            })
            .transpose()?;

        // The tracks, in the order their sinks come back.
        let mut muxer = Mp4Muxer::create(path)?;
        muxer.add_stream("video", video.parameters(), video.time_base())?;
        if let Some((_, encoder)) = &audio {
            muxer.add_stream("audio", encoder.parameters(), audio_time_base)?;
        }
        let mut sinks = muxer.open()?;
        // Taken from the front, so each sink is the stream added at that
        // index — `add_stream` order is what `open` answers in.
        if sinks.is_empty() {
            return Err("the muxer produced no track sinks".into());
        }
        let video_sink = sinks.remove(0);

        let audio = match audio {
            Some((tee, encoder)) => {
                let sink = sinks.remove(0);
                let (gate, pause) = PauseGate::for_audio("record-audio-pause");
                let branch = tee
                    .branch()
                    .ok_or("the mixer's Tee is gone")?
                    // The same thread boundary the video branch has, and for
                    // the same reason: encoding and muxing must not be done
                    // on the mixer's own thread, where a slow write would
                    // stall the mix everything else is listening to.
                    .queue_with_policy(
                        "record-audio-queue",
                        RECORDING_QUEUE_DEPTH,
                        OverflowPolicy::Block(RECORDING_SEND_TIMEOUT),
                    )
                    .pipe(gate)
                    .pipe(encoder)
                    // The mixer has been running since the application
                    // started and its timeline says so, exactly as the
                    // compositor's does.
                    .pipe(TimestampOrigin::new("record-audio-origin"))
                    .to(sink)?;
                Some(AudioTrack {
                    branch: tee.attach(branch)?,
                    tee: tee.clone(),
                    pause,
                })
            }
            None => None,
        };

        // Attached last, so a failure above leaves no track running: the
        // video branch is the one that cannot be un-attached without
        // finalizing the file.
        let video = match backend.attach_recording(video, video_sink) {
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
    pub(super) fn set_paused(&self, paused: bool) {
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
    pub(super) fn stop(mut self, backend: &Backend) -> Result<(), BackendError> {
        let mut failure = None;
        if let Some(audio) = self.audio.take()
            && let Err(error) = audio.tee.finish_branch(audio.branch)
        {
            failure = Some(BackendError::from(error));
        }
        if let Some(video) = self.video.take()
            && let Err(error) = backend.detach_recording(video)
        {
            failure = failure.or(Some(error));
        }
        match failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}
