//! Opening the encoder a recording's video track is written with, and wiring
//! its branch onto the compositor's `Tee` — the macOS half, which is the
//! Linux one with VideoToolbox where NVENC and Vulkan Video are.
//!
//! See the Linux twin for why the work splits between this and `super`, and
//! why the recording queue blocks for a bounded time rather than dropping.

use media_pp::color::ColorDescription;
use media_pp::elements::{
    MetalScaler, MetalScalerInterp, PauseGate, SwEncoder, SwEncoderOptions, SwScaler,
    TimestampOrigin, VideoToolboxCodec, VideoToolboxEncoder, VideoToolboxEncoderOptions,
    VideoToolboxFrameFormat,
};
use media_pp::ffmpeg;
use media_pp::queue::OverflowPolicy;

use crate::engine::backend::{
    Backend, BackendError, OUTPUT_QUEUE_DEPTH, OUTPUT_SEND_TIMEOUT, PROBE_FPS, VideoTrack,
    software_codec,
};
use crate::engine::source::filters::ChainFormat;
use crate::settings::{RecordingEncoder, RecordingSettings};

use super::{OutputEncoding, OutputKind};

/// A video encoder opened and ready, waiting only for the muxer sink it
/// writes into — see the Linux twin.
pub(in crate::engine) struct PreparedOutput {
    encoder: RecordEncoder,
    /// What the file is written at, which is the Scene Canvas unless the
    /// settings asked for less.
    size: [u32; 2],
}

impl PreparedOutput {
    /// What `Mp4Muxer::add_stream` needs to describe this track.
    pub(in crate::engine) fn parameters(&self) -> ffmpeg::codec::Parameters {
        match &self.encoder {
            RecordEncoder::VideoToolbox(encoder) => encoder.parameters(),
            RecordEncoder::Software(encoder) => encoder.parameters(),
        }
    }

    /// What the video track is stamped in: the encoder's own unit.
    pub(in crate::engine) fn time_base(&self) -> ffmpeg::Rational {
        match &self.encoder {
            RecordEncoder::VideoToolbox(encoder) => encoder.time_base(),
            RecordEncoder::Software(encoder) => encoder.time_base(),
        }
    }
}

/// One opened encoder, and which kind of chain it needs in front of it.
enum RecordEncoder {
    /// The media engine, taking the compositor's NV12 pixel buffers as they
    /// are.
    VideoToolbox(VideoToolboxEncoder),
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
        Ok(PreparedOutput {
            encoder: self.open_encoder(kind, fps, encoding)?,
            size: encoding.size,
        })
    }

    /// Builds the recording's video branch onto the compositor's `Tee` and
    /// starts it writing into `sink` — see the Linux twin.
    ///
    /// No colour conversion on the hardware path: the compositor draws NV12
    /// at BT.709 limited range, which the VideoToolbox encoder takes as its
    /// own native input.
    pub(in crate::engine) fn attach_output(
        &self,
        kind: OutputKind,
        prepared: PreparedOutput,
        sink: media_pp::element::BoxSink,
    ) -> Result<VideoTrack, BackendError> {
        let PreparedOutput { encoder, size, .. } = prepared;
        let [width, height] = size;

        let mut branch = self.tee.branch()?.queue_with_policy(
            format!("{}-queue", kind.prefix()),
            OUTPUT_QUEUE_DEPTH,
            OverflowPolicy::Block(OUTPUT_SEND_TIMEOUT),
        );
        let (gate, pause) = PauseGate::new(format!("{}-pause", kind.prefix()));
        branch = branch.pipe(gate);
        // Scaled on the GPU with Lanczos, before anything else sees the
        // frame, for the reason the Linux twin gives.
        if size != self.size {
            branch = branch.pipe(MetalScaler::new(
                format!("{}-scale", kind.prefix()),
                self.gpu.device(),
                width,
                height,
                MetalScalerInterp::Lanczos,
            )?);
        }
        branch = match encoder {
            RecordEncoder::VideoToolbox(encoder) => branch.pipe(encoder),
            RecordEncoder::Software(encoder) => branch
                .pipe(
                    self.gpu
                        .download(format!("{}-download", kind.prefix()), ChainFormat::Nv12),
                )
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
        let frame_rate = ffmpeg::Rational::new(fps as i32, 1);
        let bit_rate = encoding.bit_rate_bits;
        let gop_size = fps * encoding.keyframe_seconds.max(1);
        // What the Canvas is, told to the encoder so the file says it — see
        // the Linux twin.
        let color = ColorDescription::BT709_LIMITED;
        let name = format!("{}-encode", kind.prefix());
        match encoding.encoder {
            RecordingEncoder::VideoToolbox => Ok(RecordEncoder::VideoToolbox(
                VideoToolboxEncoder::with_color(
                    name,
                    self.gpu.device(),
                    VideoToolboxEncoderOptions {
                        codec: VideoToolboxCodec::H264,
                        format: VideoToolboxFrameFormat::Nv12,
                        width,
                        height,
                        frame_rate,
                        bit_rate,
                        gop_size,
                        max_b_frames: None,
                    },
                    color,
                )?,
            )),
            // Each is another platform's own.
            RecordingEncoder::Nvenc
            | RecordingEncoder::MediaFoundation
            | RecordingEncoder::Vulkan => Err(format!(
                "{} is not available while compositing with {}",
                encoding.encoder.label(),
                self.gpu.describe()
            )
            .into()),
            other @ (RecordingEncoder::OpenH264 | RecordingEncoder::X264) => {
                Ok(RecordEncoder::Software(SwEncoder::with_color(
                    name,
                    SwEncoderOptions {
                        codec: software_codec(other),
                        width,
                        height,
                        pixel_format: ffmpeg::format::Pixel::YUV420P,
                        frame_rate,
                        bit_rate,
                        gop_size,
                        max_b_frames: None,
                    },
                    color,
                )?))
            }
        }
    }

    /// Which H.264 encoders this machine can actually open, probed once —
    /// see the Linux twin.
    pub(in crate::engine) fn available_encoders(&self) -> &[RecordingEncoder] {
        self.encoders.get_or_init(|| {
            RecordingEncoder::ALL
                .into_iter()
                .filter(|encoder| {
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

    /// Ends the recording's video track — see the Linux twin.
    pub(in crate::engine) fn detach_output(&self, track: VideoTrack) -> Result<(), BackendError> {
        self.tee.finish_branch(track.branch)?;
        Ok(())
    }

    /// Hangs a screenshot's branch off the compositor's `Tee` — see the
    /// Linux twin, whose Canvas is the same NV12.
    pub(in crate::engine) fn attach_screenshot(
        &self,
        sink: media_pp::element::BoxSink,
    ) -> Result<media_pp::graph::BranchId, BackendError> {
        let branch = self
            .tee
            .branch()?
            .queue_with_policy("screenshot-queue", 1, OverflowPolicy::DropNewest)
            .pipe(
                self.gpu
                    .download("screenshot-download".to_owned(), ChainFormat::Nv12),
            )
            .pipe(SwScaler::to_format(
                "screenshot-convert",
                ffmpeg::format::Pixel::RGB24,
                ffmpeg::software::scaling::Flags::BILINEAR,
            ))
            .to(sink)?;
        Ok(self.tee.attach(branch)?)
    }

    /// Writes one Source's picture through `sink` as RGBA — see the Linux
    /// twin.
    pub(in crate::engine) fn screenshot_picture(
        &self,
        frame: std::sync::Arc<media_pp::pool::UnboundObjectPoolRef<ffmpeg::frame::Video>>,
        format: ChainFormat,
        sink: media_pp::element::BoxSink,
    ) -> Result<std::sync::Arc<media_pp::pipeline::Pipeline>, BackendError> {
        use media_pp::elements::AppSource;

        let download = self
            .gpu
            .download("source-screenshot-download".to_owned(), format);
        let convert = SwScaler::to_format(
            "source-screenshot-convert",
            ffmpeg::format::Pixel::RGBA,
            ffmpeg::software::scaling::Flags::BILINEAR,
        );
        let (source, pusher) = AppSource::new("source-screenshot", 1);
        let (pipeline, ()) = media_pp::pipeline::Pipeline::new(
            "source-screenshot",
            source,
            move |source, context| {
                let branch = context.branch().pipe(download).pipe(convert).to(sink)?;
                context.attach(source, 0, branch)?;
                Ok(())
            },
        )?;
        pipeline.run()?;
        pusher.push(media_pp::buffer::MediaBuffer::Video(frame))?;
        Ok(pipeline)
    }

    /// Takes a screenshot's branch off again.
    pub(in crate::engine) fn detach_screenshot(
        &self,
        branch: media_pp::graph::BranchId,
    ) -> Result<(), BackendError> {
        self.tee.detach(branch)?;
        Ok(())
    }
}

/// The virtual camera is Windows' own — a Media Foundation camera — and its
/// button is shown nowhere else; these answer as if it were asked anyway.
impl Backend {
    pub(in crate::engine) fn attach_virtual_camera(
        &self,
    ) -> Result<media_pp::graph::BranchId, BackendError> {
        Err("the virtual camera is available only on Windows".into())
    }

    pub(in crate::engine) fn detach_virtual_camera(
        &self,
        _branch: media_pp::graph::BranchId,
    ) -> Result<(), BackendError> {
        Ok(())
    }
}
