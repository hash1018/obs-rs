//! What the macOS backend composites with, in the shape the Linux one has.
//!
//! One API rather than two: VideoToolbox frames — Core Video pixel buffers —
//! drawn with Metal. A capture, a decoder and an upload all hand those over,
//! and the compositor, the filters and the encoder all take them, so every
//! element a Source builds comes from here, as on Linux. The type is the
//! Linux backend's `Gpu` with one kind in it, which is what lets the Sources
//! written against that — a Color Source, a media file, a Scene — be the same
//! code on both.
//!
//! A pixel buffer belongs to no Metal device, so there is no device to share
//! the way D3D11 and CUDA share theirs: each Metal element opens the system's
//! GPU for itself, and what they have in common is the `VideoToolboxDevice`
//! their frames are made on.

use std::sync::Arc;

use media_pp::contract::MemoryDomain;
use media_pp::element::RawFilter;
use media_pp::elements::{
    ChromaKeyHandle, ChromaKeyOptions, CompositorInput, DecodeTarget, MetalChromaKey,
    MetalConverter, MetalVideoCompositor, MetalVideoCompositorHandle, MetalVideoEffect,
    MetalVideoLayerHandle, VideoCompositorOptions, VideoEffect, VideoEffectHandle, VideoLayer,
    VideoToolboxDevice, VideoToolboxDownload, VideoToolboxFrameFormat, VideoToolboxUpload,
};
use media_pp::ffmpeg;
use media_pp::pipeline::{DetachedBranch, Pipeline};

use crate::engine::backend::BackendError;
use crate::engine::source::filters::ChainFormat;

/// The one VideoToolbox context this process makes its frames on.
///
/// Cloning is cheap and shares that context.
#[derive(Clone)]
pub(in crate::engine) struct Gpu(VideoToolboxDevice);

impl Gpu {
    pub(in crate::engine) fn open() -> Result<Self, BackendError> {
        Ok(Self(VideoToolboxDevice::new()?))
    }

    /// What the log and the Stats dock call it.
    pub(in crate::engine) fn describe(&self) -> String {
        "Metal".to_owned()
    }

    /// The VideoToolbox context itself, for what takes one directly: a
    /// capture handing over its own pixel buffers, an encoder.
    pub(in crate::engine) fn device(&self) -> &VideoToolboxDevice {
        &self.0
    }

    /// Where these frames live, which is what a filter rack declares it
    /// takes.
    pub(in crate::engine) fn memory_domain(&self) -> MemoryDomain {
        MemoryDomain::VideoToolbox
    }

    /// Carries a picture in system memory to the GPU. The upload keeps the
    /// layout it is handed, as Vulkan's does, so what arrives already has to
    /// be `format`: BGRA from everything this application draws, NV12 from a
    /// camera.
    pub(in crate::engine) fn upload(
        &self,
        name: String,
        _format: ChainFormat,
    ) -> Box<dyn RawFilter> {
        Box::new(VideoToolboxUpload::new(name, &self.0))
    }

    /// Brings a picture back to system memory, in the layout it is in. A
    /// pixel buffer says which, so the format is only the Linux signature's.
    pub(in crate::engine) fn download(
        &self,
        name: String,
        _format: ChainFormat,
    ) -> Box<dyn RawFilter> {
        Box::new(VideoToolboxDownload::new(name))
    }

    /// Turns NV12 into BGRA on the GPU, which is what a filter needs in
    /// front of it when the Source hands on NV12.
    pub(in crate::engine) fn to_bgra(
        &self,
        name: String,
    ) -> Result<Box<dyn RawFilter>, BackendError> {
        Ok(Box::new(MetalConverter::new(name, &self.0)?))
    }

    pub(in crate::engine) fn chroma_key(
        &self,
        name: String,
        options: ChromaKeyOptions,
    ) -> Result<(Box<dyn RawFilter>, ChromaKeyHandle), BackendError> {
        let (element, handle) = MetalChromaKey::new(name, &self.0, options)?;
        Ok((Box::new(element), handle))
    }

    pub(in crate::engine) fn video_effect(
        &self,
        name: String,
        effect: VideoEffect,
    ) -> Result<(Box<dyn RawFilter>, VideoEffectHandle), BackendError> {
        let (element, handle) = MetalVideoEffect::new(name, &self.0, effect)?;
        Ok((Box::new(element), handle))
    }

    /// Where a `VideoDecodeBin` puts its pictures. Core Video's pool grows
    /// as frames are held, so there is no budget of surfaces to give and
    /// `downstream_hw_frames` is only the Linux signature's.
    pub(in crate::engine) fn decode_target(&self, _downstream_hw_frames: i32) -> DecodeTarget {
        DecodeTarget::VideoToolbox {
            device: self.0.clone(),
        }
    }

    /// A compositor composing in `format`: NV12 for the Canvas, which an
    /// encoder takes, and BGRA for a Scene laid over another, which has to be
    /// transparent where nothing was drawn.
    pub(in crate::engine) fn compositor(
        &self,
        name: String,
        options: VideoCompositorOptions,
        format: ChainFormat,
    ) -> Result<(CompositorElement, Compositor), BackendError> {
        let format = match format {
            ChainFormat::Bgra => VideoToolboxFrameFormat::Bgra,
            ChainFormat::Nv12 => VideoToolboxFrameFormat::Nv12,
        };
        let (element, handle) = MetalVideoCompositor::with_format(name, &self.0, options, format)?;
        Ok((CompositorElement(element), Compositor(handle)))
    }
}

/// A compositor not yet running — see the Linux twin, which has two kinds of
/// one behind the same [`Self::pipeline`].
pub(in crate::engine) struct CompositorElement(MetalVideoCompositor);

impl CompositorElement {
    /// Builds the pipeline this compositor feeds, with the branch `wire`
    /// makes attached to its one output. Whatever else `wire` answers comes
    /// back beside the pipeline, as `Pipeline::new` hands it back.
    pub(in crate::engine) fn pipeline<T>(
        self,
        name: String,
        wire: impl FnOnce(
            &Arc<media_pp::element::Context>,
        ) -> media_pp::error::Result<(DetachedBranch, T)>,
    ) -> media_pp::error::Result<(Arc<Pipeline>, T)> {
        Pipeline::new(name, self.0, |source, context| {
            let (branch, answer) = wire(context)?;
            context.attach(source, 0, branch)?;
            Ok(answer)
        })
    }
}

/// A running compositor's control — what a Source registers its input with.
#[derive(Clone)]
pub(in crate::engine) struct Compositor(MetalVideoCompositorHandle);

impl Compositor {
    pub(in crate::engine) fn add_source(
        &self,
        name: String,
        layer: VideoLayer,
    ) -> Result<CompositorInput<Layer>, BackendError> {
        Ok(self.0.add_source(name, layer)?)
    }

    pub(in crate::engine) fn remove_source(&self, name: &str) {
        self.0.remove_source(name);
    }

    pub(in crate::engine) fn frame_rate(&self) -> Option<ffmpeg::Rational> {
        self.0.frame_rate()
    }

    pub(in crate::engine) fn set_frame_rate(
        &self,
        rate: ffmpeg::Rational,
    ) -> Result<(), BackendError> {
        Ok(self.0.set_frame_rate(rate)?)
    }
}

/// The compositor's layer control already offers exactly what a backend
/// must, as the D3D11 one's does.
pub(in crate::engine) type Layer = MetalVideoLayerHandle;
