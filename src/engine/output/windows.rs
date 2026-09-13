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
    D3d11Download, D3d11Scaler, D3d11ScalerFormat, D3d11VideoCodec, D3d11VideoEncoder,
    D3d11VideoEncoderOptions, D3d11VideoInputFormat, PauseGate, SwEncoder, SwEncoderOptions,
    SwScaler, TimestampOrigin,
};
use std::sync::Arc;

use media_pp::ffmpeg;
use media_pp::queue::OverflowPolicy;

use crate::engine::backend::{
    Backend, BackendError, OUTPUT_QUEUE_DEPTH, OUTPUT_SEND_TIMEOUT, PROBE_FPS, VideoTrack,
    software_codec,
};
use crate::settings::{RecordingEncoder, RecordingSettings};

use super::{OutputEncoding, OutputKind};

/// A video encoder opened and ready, waiting only for the muxer sink it
/// writes into.
///
/// It exists because an mp4's tracks are fixed before its header is written,
/// and the audio track is added by `engine::output` — which cannot open
/// this one, since which encoder and which frame format are the backend's
/// own. So the work splits: this end opens the encoder and says what stream
/// it needs, and the branch is built once the sink for it exists.
pub(in crate::engine) struct PreparedOutput {
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

impl PreparedOutput {
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
    Hardware(D3d11VideoEncoder),
    /// Needs them copied back from the GPU and converted first.
    Software(SwEncoder),
}

impl Backend {
    pub(in crate::engine) fn prepare_output(
        &self,
        kind: OutputKind,
        fps: u32,
        encoding: &OutputEncoding,
    ) -> Result<PreparedOutput, BackendError> {
        // The compositor's own rate, which the settings have already been
        // applied to — a recording is written at what is being composited,
        // and there is nothing in between to re-rate it. Read from the
        // compositor rather than from the setting so that a rate it refused
        // cannot produce a file claiming frames nothing is making.
        Ok(PreparedOutput {
            encoder: self.open_encoder(kind, fps, encoding)?,
            time_base: ffmpeg::Rational::new(1, fps as i32),
            size: encoding.size,
        })
    }

    /// Builds the recording's video branch onto the compositor's `Tee` and
    /// starts it writing into `sink`.
    ///
    /// Separate from [`Backend::prepare_output`] only because the sink
    /// cannot exist until every track has been declared — see
    /// [`PreparedOutput`].
    ///
    /// No colour conversion anywhere: the compositor draws BGRA and NVENC
    /// takes BGRA directly, converting to its own YUV as part of encoding.
    ///
    /// # What the queue's policy has to be
    ///
    /// Not the Preview's `DropNewest` — a dropped frame there is one stale
    /// repaint, here it is a frame missing from the file. Not an unbounded
    /// wait either: an encoder that stops answering would then wedge the
    /// compositor, and with it the Preview and every other branch. So it
    /// blocks, but only for a bounded time, and a timeout arrives on the bus
    /// as an error naming this branch rather than as silence.
    pub(in crate::engine) fn attach_output(
        &self,
        kind: OutputKind,
        prepared: PreparedOutput,
        sink: Box<dyn media_pp::element::Sink>,
    ) -> Result<VideoTrack, BackendError> {
        let PreparedOutput { encoder, size, .. } = prepared;
        let [width, height] = size;

        let mut branch = self
            .tee
            .branch()
            .ok_or("the compositor's Tee is gone")?
            .queue_with_policy(
                format!("{}-queue", kind.prefix()),
                OUTPUT_QUEUE_DEPTH,
                OverflowPolicy::Block(OUTPUT_SEND_TIMEOUT),
            );
        // The gate first, so a paused span is gone before anything downstream
        // has to reason about it.
        let (gate, pause) = PauseGate::new(format!("{}-pause", kind.prefix()));
        branch = branch.pipe(gate);
        // Only when the file is smaller than the canvas. `Preserve` because
        // this is a resize and nothing more — the compositor draws BGRA and
        // the encoder takes BGRA, so a format change here would be work
        // neither end asked for.
        if size != self.size {
            branch = branch.pipe(D3d11Scaler::new(
                format!("{}-scale", kind.prefix()),
                &self.device,
                Arc::clone(&self.context),
                D3d11ScalerFormat::Preserve,
                width,
                height,
            )?);
        }
        branch = match encoder {
            RecordEncoder::Hardware(encoder) => branch.pipe(encoder),
            // A software encoder is not on the GPU and does not take BGRA, so
            // the frames have to come back across the bus and be converted
            // before it sees them. That is the cost the choice carries, and it
            // is why the hardware path is the default.
            RecordEncoder::Software(encoder) => branch
                .pipe(D3d11Download::new(
                    format!("{}-download", kind.prefix()),
                    &self.device,
                    Arc::clone(&self.context),
                    width,
                    height,
                )?)
                .pipe(SwScaler::new(
                    format!("{}-convert", kind.prefix()),
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
            .pipe(TimestampOrigin::new(format!("{}-origin", kind.prefix())))
            .to(sink)?;
        Ok(VideoTrack {
            branch: self.tee.attach(branch)?,
            pause,
        })
    }

    /// Opens whichever encoder the output asks for.
    fn open_encoder(
        &self,
        kind: OutputKind,
        fps: u32,
        encoding: &OutputEncoding,
    ) -> Result<RecordEncoder, BackendError> {
        let [width, height] = encoding.size;
        let time_base = ffmpeg::Rational::new(1, fps as i32);
        let frame_rate = ffmpeg::Rational::new(fps as i32, 1);
        let bit_rate = encoding.bit_rate_bits;
        let gop_size = fps * encoding.keyframe_seconds.max(1);
        match encoding.encoder {
            RecordingEncoder::Nvenc | RecordingEncoder::MediaFoundation => {
                Ok(RecordEncoder::Hardware(D3d11VideoEncoder::new(
                    format!("{}-encode", kind.prefix()),
                    &self.device,
                    Arc::clone(&self.context),
                    D3d11VideoEncoderOptions {
                        codec: if encoding.encoder == RecordingEncoder::Nvenc {
                            D3d11VideoCodec::H264Nvenc
                        } else {
                            D3d11VideoCodec::H264MediaFoundation
                        },
                        // The compositor's own output, so neither hardware
                        // path converts anything: both take BGRA directly.
                        input_format: D3d11VideoInputFormat::Bgra,
                        width,
                        height,
                        time_base,
                        frame_rate,
                        bit_rate,
                        gop_size,
                        // The encoder's own default, which on NVENC does use
                        // B-frames: better quality at this bitrate, and the
                        // reason not to was measured and did not hold. What
                        // they delay is packets out, not frames in — 1080p60
                        // BGRA takes ~2ms a frame to submit whether they are
                        // on or off, and the frame comes straight back to its
                        // pool either way.
                        //
                        // Kept for a live send too, now that this opens the
                        // streaming encoder as well. Reordering costs two or
                        // three frames steadily — 33 to 50ms at 60fps — and
                        // the first packet arrives 15 frames in rather than
                        // 3, once, at connect. Both disappear into what an
                        // ingest adds: a platform is seconds behind live
                        // whatever this does. Turning them off would be
                        // tuning latency, which `output`'s own HLS notes say
                        // this application does not do, and would cost
                        // quality at the same bitrate against the encoder
                        // everyone compares it to.
                        max_b_frames: None,
                    },
                )?))
            }
            other => Ok(RecordEncoder::Software(SwEncoder::new(
                format!("{}-encode", kind.prefix()),
                SwEncoderOptions {
                    codec: software_codec(other),
                    width,
                    height,
                    time_base,
                    frame_rate,
                    bit_rate,
                    gop_size,
                    max_b_frames: None,
                },
            )?)),
        }
    }

    /// Which H.264 encoders this machine can actually open — see the CUDA
    /// backend's own copy for why this is probed rather than assumed.
    pub(in crate::engine) fn available_encoders(&self) -> &[RecordingEncoder] {
        self.encoders.get_or_init(|| {
            RecordingEncoder::ALL
                .into_iter()
                .filter(|encoder| {
                    // The Canvas's own size, not a token one: an encoder
                    // that opens at 320x240 and refuses 4K would be offered
                    // and then fail at the moment it was used.
                    let probe = RecordingSettings {
                        encoder: *encoder,
                        ..RecordingSettings::default()
                    }
                    .encoding(self.size);
                    self.open_encoder(OutputKind::Recording, PROBE_FPS, &probe)
                        .is_ok()
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
    /// audio branch is finished too. Ending both is `engine::output`'s
    /// job, and the reason it rather than this owns them.
    ///
    /// Returns once the `Eos` is on its way, not once the file is closed: the
    /// encoder flush and the trailer happen on a thread the `Tee` owns, so
    /// this does not block the engine. The file is complete a moment after
    /// this returns rather than at the instant it does.
    pub(in crate::engine) fn detach_output(&self, track: VideoTrack) -> Result<(), BackendError> {
        self.tee.finish_branch(track.branch)?;
        Ok(())
    }

    /// Hangs a screenshot's branch off the compositor's `Tee`, ending at
    /// `sink` with the Canvas as RGB24 in system memory — see
    /// `output::screenshot` for why this and not the Preview.
    ///
    /// Downloaded at the Canvas's own size, whatever a recording is scaled
    /// to: a screenshot is of what is composited, not of what a file keeps.
    pub(in crate::engine) fn attach_screenshot(
        &self,
        sink: Box<dyn media_pp::element::Sink>,
    ) -> Result<media_pp::graph::BranchId, BackendError> {
        let [width, height] = self.size;
        let branch = self
            .tee
            .branch()
            .ok_or("the compositor's Tee is gone")?
            .queue_with_policy("screenshot-queue", 1, OverflowPolicy::DropNewest)
            .pipe(D3d11Download::new(
                "screenshot-download",
                &self.device,
                Arc::clone(&self.context),
                width,
                height,
            )?)
            .pipe(SwScaler::new(
                "screenshot-convert",
                ffmpeg::format::Pixel::RGB24,
                width,
                height,
                ffmpeg::software::scaling::Flags::BILINEAR,
            ))
            .to(sink)?;
        Ok(self.tee.attach(branch)?)
    }

    /// Takes a screenshot's branch off again. `detach` rather than
    /// `finish_branch`: there is no file waiting for an `Eos`, only a sink
    /// that has already written what it came for.
    pub(in crate::engine) fn detach_screenshot(
        &self,
        branch: media_pp::graph::BranchId,
    ) -> Result<(), BackendError> {
        self.tee.detach(branch)?;
        Ok(())
    }
}
