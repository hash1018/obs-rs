//! Opening the encoder a recording's video track is written with, and wiring
//! its branch onto the compositor's `Tee`.
//!
//! The backend's half of a recording. `super` owns the file — both tracks go
//! into one muxer and an MP4's tracks are fixed before its header is written
//! — but it cannot open this encoder, because which encoder and which frame
//! format are the backend's own. So the work splits: this end opens the
//! encoder and says what stream it needs, and the branch is built once the
//! sink for it exists.

use media_pp::elements::{
    CudaCodec, CudaDownload, CudaEncoder, CudaEncoderOptions, CudaFrameFormat, CudaScaler,
    CudaScalerInterp, PauseGate, SwEncoder, SwEncoderOptions, SwScaler, TimestampOrigin,
};
use media_pp::ffmpeg;
use media_pp::queue::OverflowPolicy;

use crate::engine::backend::{
    Backend, BackendError, PROBE_FPS, RECORDING_QUEUE_DEPTH, RECORDING_SEND_TIMEOUT, VideoTrack,
    software_codec,
};
use crate::settings::{RecordingEncoder, RecordingSettings};

/// A video encoder opened and ready, waiting only for the muxer sink it
/// writes into.
///
/// It exists because an mp4's tracks are fixed before its header is written,
/// and the audio track is added by `engine::recording` — which cannot open
/// this one, since which encoder and which frame format are the backend's
/// own. So the work splits: this end opens the encoder and says what stream
/// it needs, and the branch is built once the sink for it exists.
pub(in crate::engine) struct PreparedRecording {
    encoder: RecordEncoder,
    /// What the file's video track is stamped in — the reciprocal of the
    /// rate the compositor is running at, which is the only rate frames can
    /// arrive at.
    time_base: ffmpeg::Rational,
    /// What the file is written at, which is the Scene Canvas unless the
    /// settings asked for less. The encoder was opened for it, so the branch
    /// has to deliver it.
    size: [u32; 2],
}

impl PreparedRecording {
    /// What `Mp4Muxer::add_stream` needs to describe this track.
    pub(in crate::engine) fn parameters(&self) -> ffmpeg::codec::Parameters {
        match &self.encoder {
            RecordEncoder::Hardware(encoder) => encoder.parameters(),
            RecordEncoder::Software(encoder) => encoder.parameters(),
        }
    }

    pub(in crate::engine) fn time_base(&self) -> ffmpeg::Rational {
        self.time_base
    }
}

/// One opened encoder, and which kind of chain it needs in front of it.
enum RecordEncoder {
    /// Takes the compositor's frames as they are.
    Hardware(CudaEncoder),
    /// Needs them copied back from the GPU and converted first.
    Software(SwEncoder),
}

impl Backend {
    pub(in crate::engine) fn prepare_recording(
        &self,
        fps: u32,
        settings: &crate::settings::RecordingSettings,
    ) -> Result<PreparedRecording, BackendError> {
        // The compositor's own rate, which the settings have already been
        // applied to — a recording is written at what is being composited,
        // and there is nothing in between to re-rate it. Read from the
        // compositor rather than from the setting so that a rate it refused
        // cannot produce a file claiming frames nothing is making.
        Ok(PreparedRecording {
            encoder: self.open_encoder(fps, settings)?,
            time_base: ffmpeg::Rational::new(1, fps as i32),
            size: settings.output_size(self.size),
        })
    }

    /// Builds the recording's video branch onto the compositor's `Tee` and
    /// starts it writing into `sink`.
    ///
    /// Separate from [`Backend::prepare_recording`] only because the sink
    /// cannot exist until every track has been declared — see
    /// [`PreparedRecording`].
    ///
    /// No colour conversion anywhere: the compositor draws NV12 and NVENC
    /// takes NV12 as its own native input.
    ///
    /// # What the queue's policy has to be
    ///
    /// Not the Preview's `DropNewest` — a dropped frame there is one stale
    /// repaint, here it is a frame missing from the file. Not an unbounded
    /// wait either: an encoder that stops answering would then wedge the
    /// compositor, and with it the Preview and every other branch. So it
    /// blocks, but only for a bounded time, and a timeout arrives on the bus
    /// as an error naming this branch rather than as silence.
    ///
    /// # Verified on this backend
    ///
    /// The commits that built this could only reason about the Linux half —
    /// the host they were written on cannot build the CUDA backend — so it is
    /// worth writing down that it was afterwards run. Two live display
    /// captures, 1920x1080 H.264 at 60:
    ///
    /// - 717 frames over 11.95 s, 959 over 15.98 s, 1198 over 19.97 s: 60.0
    ///   throughout, and the compositor held 59.99 of its 60.00 while
    ///   recording. A decoded frame from the middle is the composited Canvas.
    /// - `start_time` is 0.050000 on every file, including a second recording
    ///   started 24 s into the same session — three frames of B-frame reorder
    ///   delay, which is what [`TimestampOrigin`] leaves behind rather than
    ///   the compositor's uptime.
    /// - 1439 frames over 23.98 s across a twelve-second minimise, so the
    ///   Preview going idle takes nothing from this branch.
    /// - A directory it cannot write reaches the status bar as "Recording
    ///   could not start — ffmpeg error: Permission denied", the clock stays
    ///   at `--:--:--`, and the next attempt clears it.
    pub(in crate::engine) fn attach_recording(
        &self,
        prepared: PreparedRecording,
        sink: Box<dyn media_pp::element::Sink>,
    ) -> Result<VideoTrack, BackendError> {
        let PreparedRecording { encoder, size, .. } = prepared;
        let [width, height] = size;

        let mut branch = self
            .tee
            .branch()
            .ok_or("the compositor's Tee is gone")?
            .queue_with_policy(
                "record-queue",
                RECORDING_QUEUE_DEPTH,
                OverflowPolicy::Block(RECORDING_SEND_TIMEOUT),
            );
        // The gate first, so a paused span is gone before anything downstream
        // has to reason about it.
        let (gate, pause) = PauseGate::new("record-pause");
        branch = branch.pipe(gate);
        // Only when the file is smaller than the canvas.
        if size != self.size {
            branch = branch.pipe(CudaScaler::new(
                "record-scale",
                &self.device,
                width,
                height,
                // Downscaling a screen recording is exactly the quality-sensitive
                // path that enum documents: nearest on 1080p text is
                // visibly worse, and this runs on the GPU either way.
                CudaScalerInterp::Lanczos,
            ));
        }
        branch = match encoder {
            RecordEncoder::Hardware(encoder) => branch.pipe(encoder),
            // A software encoder is not on the GPU and does not take NV12, so
            // the frames have to come back across the bus and be converted
            // before it sees them. That is the cost the choice carries, and it
            // is why the hardware path is the default.
            RecordEncoder::Software(encoder) => branch
                .pipe(CudaDownload::new(
                    "record-download",
                    &self.device,
                    CudaFrameFormat::Nv12,
                    width,
                    height,
                ))
                .pipe(SwScaler::new(
                    "record-convert",
                    ffmpeg::format::Pixel::YUV420P,
                    width,
                    height,
                    ffmpeg::software::scaling::Flags::BILINEAR,
                ))
                .pipe(encoder),
        };
        let branch = branch
            // The compositor has been running since the application started, and
            // its timeline says so. Without this the file is written as
            // beginning that far in, and a player shows the lead-in as empty.
            .pipe(TimestampOrigin::new("record-origin"))
            .to(sink)?;
        Ok(VideoTrack {
            branch: self.tee.attach(branch)?,
            pause,
        })
    }

    /// Opens whichever encoder the settings name.
    fn open_encoder(
        &self,
        fps: u32,
        settings: &RecordingSettings,
    ) -> Result<RecordEncoder, BackendError> {
        let [width, height] = settings.output_size(self.size);
        let time_base = ffmpeg::Rational::new(1, fps as i32);
        let frame_rate = ffmpeg::Rational::new(fps as i32, 1);
        let bit_rate = settings.bit_rate_bits();
        let gop_size = fps * settings.keyframe_seconds_clamped();
        match settings.encoder {
            RecordingEncoder::Nvenc => Ok(RecordEncoder::Hardware(CudaEncoder::new(
                "record-encode",
                &self.device,
                CudaEncoderOptions {
                    codec: CudaCodec::H264,
                    input_format: CudaFrameFormat::Nv12,
                    width,
                    height,
                    time_base,
                    frame_rate,
                    bit_rate,
                    gop_size,
                },
            )?)),
            other => Ok(RecordEncoder::Software(SwEncoder::new(
                "record-encode",
                SwEncoderOptions {
                    codec: software_codec(other),
                    width,
                    height,
                    time_base,
                    frame_rate,
                    bit_rate,
                    gop_size,
                },
            )?)),
        }
    }

    /// Which H.264 encoders this machine can actually open, in the order the
    /// list should offer them.
    ///
    /// Probed by opening each one, not by asking whether FFmpeg knows the
    /// name. A build can carry `h264_nvenc` on a machine with no NVIDIA
    /// encoder, and `libx264` is missing from a good many builds — this
    /// machine's included — so neither question is answered by the encoder
    /// list alone.
    ///
    /// Probed once. Opening NVENC is not free, and the answer cannot change
    /// while the application is running.
    pub(in crate::engine) fn available_encoders(&self) -> &[RecordingEncoder] {
        self.encoders.get_or_init(|| {
            RecordingEncoder::ALL
                .into_iter()
                .filter(|encoder| {
                    let probe = RecordingSettings {
                        encoder: *encoder,
                        ..RecordingSettings::default()
                    };
                    // The Canvas's own size, not a token one: an encoder that
                    // opens at 320x240 and refuses 4K would be offered and
                    // then fail at the moment it was used.
                    self.open_encoder(PROBE_FPS, &probe).is_ok()
                })
                .collect()
        })
    }

    /// Ends the recording's video track.
    ///
    /// `finish_branch` rather than `detach`: an mp4 is unplayable until its
    /// trailer is written, and that happens when the muxer sees the branch's
    /// `Eos`. Detaching would drop the branch instead, leaving the file
    /// exactly as long as it is useless. `finish_branch` detaches too, so the
    /// branch id is spent either way.
    ///
    /// Only *this* track: the trailer is written once every track has
    /// reported done, so a file with audio in it stays unplayable until the
    /// audio branch is finished too. Ending both is `engine::recording`'s
    /// job, and the reason it rather than this owns them.
    ///
    /// Returns once the `Eos` is on its way, not once the file is closed: the
    /// encoder flush and the trailer happen on a thread the `Tee` owns, so
    /// this does not block the engine. The file is complete a moment after
    /// this returns rather than at the instant it does.
    pub(in crate::engine) fn detach_recording(
        &self,
        track: VideoTrack,
    ) -> Result<(), BackendError> {
        self.tee.finish_branch(track.branch)?;
        Ok(())
    }
}
