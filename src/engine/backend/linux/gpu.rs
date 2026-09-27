//! Which GPU API the Linux backend composites with, and the one place that
//! tells the two apart.
//!
//! CUDA where there is an NVIDIA GPU, Vulkan everywhere else — an AMD or
//! Intel GPU, which CUDA cannot reach. The two are whole sets, as the
//! parent's docs say backends are: a CUDA frame cannot enter a Vulkan
//! compositor, so every element a Source builds has to come from the one
//! chosen here. That is what this type is for. A Source asks it for an
//! upload, a decoder target, a filter or a compositor, and never names
//! either API itself.
//!
//! What still differs is what each API is better at, and is said where it
//! matters rather than hidden:
//!
//! - A screen cast reaches CUDA as DMA-BUF and never touches system memory.
//!   Vulkan takes it through system memory and uploads it — see
//!   `source::portal_capture`.
//! - The Preview reaches wgpu through memory CUDA and Vulkan both hold, or,
//!   on Vulkan, as a picture read back and written — see `preview`.
//! - Only CUDA scales: a recording smaller than the Canvas goes through the
//!   CPU on Vulkan — see `output`.
//!
//! # Choosing
//!
//! `OBSRS_GPU=cuda` or `OBSRS_GPU=vulkan` insists on one, which is how the
//! Vulkan path is tried on a machine that has NVIDIA's. Otherwise CUDA is
//! tried first and Vulkan is what a machine without it gets.

use std::sync::Arc;

use media_pp::contract::MemoryDomain;
use media_pp::element::Filter;
use media_pp::elements::{
    ChromaKeyHandle, ChromaKeyOptions, CompositorInput, CudaChromaKey, CudaConverter, CudaDevice,
    CudaDownload, CudaFrameFormat, CudaUpload, CudaVideoCompositor, CudaVideoCompositorHandle,
    CudaVideoEffect, CudaVideoLayerHandle, DecodeTarget, LayerFrame, VideoCompositorControl,
    VideoCompositorOptions, VideoEffect, VideoEffectHandle, VideoLayer, VideoLayerControl,
    VulkanChromaKey, VulkanConverter, VulkanDevice, VulkanDownload, VulkanFrameFormat,
    VulkanUpload, VulkanVideoCompositor, VulkanVideoCompositorHandle, VulkanVideoEffect,
    VulkanVideoLayerHandle,
};
use media_pp::ffmpeg;
use media_pp::pipeline::{DetachedBranch, Pipeline};

use crate::engine::backend::BackendError;
use crate::engine::source::filters::ChainFormat;

/// The environment variable that insists on one API — see the module docs.
const CHOICE: &str = "OBSRS_GPU";

/// The one device this process composites on, and which API it is.
///
/// Cloning is cheap and shares that device. One per process, never a second:
/// a CUDA context created or dropped while another thread encodes can fault
/// inside the NVIDIA driver, and two Vulkan devices could not hand each other
/// a frame.
#[derive(Clone)]
pub(in crate::engine) enum Gpu {
    /// In an `Arc` because a `CudaDevice` is not `Clone`, and a filter rack
    /// builds elements long after the Source that made it was opened.
    Cuda(Arc<CudaDevice>),
    Vulkan(VulkanDevice),
}

impl Gpu {
    /// Opens the device this process will use — see the module docs.
    pub(in crate::engine) fn open() -> Result<Self, BackendError> {
        let choice = std::env::var(CHOICE).ok();
        match choice.as_deref().map(str::trim) {
            Some("cuda") => return Ok(Self::Cuda(Arc::new(CudaDevice::new()?))),
            Some("vulkan") => return Ok(Self::Vulkan(VulkanDevice::new()?)),
            Some(other) if !other.is_empty() => {
                tracing::warn!("{CHOICE}={other} is neither cuda nor vulkan; choosing by the GPU")
            }
            _ => {}
        }
        let cuda = match CudaDevice::new() {
            Ok(device) => return Ok(Self::Cuda(Arc::new(device))),
            Err(error) => error,
        };
        match VulkanDevice::new() {
            Ok(device) => {
                tracing::info!("no CUDA device ({cuda}); compositing with Vulkan");
                Ok(Self::Vulkan(device))
            }
            Err(vulkan) => Err(format!(
                "no GPU to composite with: CUDA said \"{cuda}\", Vulkan said \"{vulkan}\""
            )
            .into()),
        }
    }

    /// A turn at making Vulkan instances, held by a test for as long as it
    /// has one of its own — where `vulkan` says it will, and the instance is
    /// NVIDIA's.
    ///
    /// NVIDIA's Vulkan driver (595.91.07, under the 1.4.341 loader) faults
    /// when one thread creates or destroys an instance while another lists
    /// the instance extensions: five runs in five with nothing but `ash`,
    /// none with lavapipe. The application makes its instances one after
    /// the other — eframe's, then this one on the device eframe made — but
    /// tests run side by side, and a media file test opening this on Vulkan
    /// took the Preview's test down with it. So the tests that make one take
    /// turns there, and only there.
    #[cfg(test)]
    pub(in crate::engine) fn test_turn(vulkan: bool) -> Option<std::sync::MutexGuard<'static, ()>> {
        static TURN: std::sync::Mutex<()> = std::sync::Mutex::new(());
        (vulkan && std::path::Path::new("/proc/driver/nvidia").exists())
            .then(|| TURN.lock().unwrap_or_else(|poisoned| poisoned.into_inner()))
    }

    /// Whether [`Self::open`] will open Vulkan on a machine that has CUDA —
    /// which only asking for it does.
    #[cfg(test)]
    pub(in crate::engine) fn asked_for_vulkan() -> bool {
        std::env::var(CHOICE).is_ok_and(|choice| choice.trim() == "vulkan")
    }

    /// What the log and the Stats dock call it.
    pub(in crate::engine) fn describe(&self) -> String {
        match self {
            Self::Cuda(_) => "CUDA".to_owned(),
            Self::Vulkan(device) => format!("Vulkan ({})", device.name()),
        }
    }

    /// Where this API's frames live, which is what a filter rack declares it
    /// takes.
    pub(in crate::engine) fn memory_domain(&self) -> MemoryDomain {
        match self {
            Self::Cuda(_) => MemoryDomain::Cuda,
            Self::Vulkan(_) => MemoryDomain::Vulkan,
        }
    }

    /// Carries a picture in system memory to the GPU as `format`.
    ///
    /// On CUDA the upload converts to the layout it is told. On Vulkan it
    /// keeps the layout it is handed — planar 4:2:0 becomes NV12 on the way
    /// — so what arrives already has to be `format`: BGRA from everything
    /// this application draws, NV12 from a camera.
    pub(in crate::engine) fn upload(&self, name: String, format: ChainFormat) -> Box<dyn Filter> {
        match self {
            Self::Cuda(device) => Box::new(CudaUpload::new(name, device, cuda_format(format))),
            Self::Vulkan(device) => Box::new(VulkanUpload::new(name, device)),
        }
    }

    /// Brings a `format` picture back to system memory, in the same layout.
    pub(in crate::engine) fn download(&self, name: String, format: ChainFormat) -> Box<dyn Filter> {
        match self {
            Self::Cuda(device) => Box::new(CudaDownload::new(name, device, cuda_format(format))),
            Self::Vulkan(device) => Box::new(VulkanDownload::new(name, device)),
        }
    }

    /// Turns NV12 into BGRA on the GPU, which is what a filter needs in
    /// front of it when the Source hands on NV12.
    pub(in crate::engine) fn to_bgra(&self, name: String) -> Result<Box<dyn Filter>, BackendError> {
        Ok(match self {
            // `CudaScaler` cannot do this — it refuses a YUV/RGB pair either
            // way — which is why `CudaConverter` grew the direction.
            Self::Cuda(device) => {
                Box::new(CudaConverter::new(name, device, CudaFrameFormat::Bgra)?)
            }
            Self::Vulkan(device) => Box::new(VulkanConverter::new(name, device)?),
        })
    }

    pub(in crate::engine) fn chroma_key(
        &self,
        name: String,
        options: ChromaKeyOptions,
    ) -> Result<(Box<dyn Filter>, ChromaKeyHandle), BackendError> {
        Ok(match self {
            Self::Cuda(device) => {
                let (element, handle) = CudaChromaKey::new(name, device, options)?;
                (Box::new(element), handle)
            }
            Self::Vulkan(device) => {
                let (element, handle) = VulkanChromaKey::new(name, device, options)?;
                (Box::new(element), handle)
            }
        })
    }

    pub(in crate::engine) fn video_effect(
        &self,
        name: String,
        effect: VideoEffect,
    ) -> Result<(Box<dyn Filter>, VideoEffectHandle), BackendError> {
        Ok(match self {
            Self::Cuda(device) => {
                let (element, handle) = CudaVideoEffect::new(name, device, effect)?;
                (Box::new(element), handle)
            }
            Self::Vulkan(device) => {
                let (element, handle) = VulkanVideoEffect::new(name, device, effect)?;
                (Box::new(element), handle)
            }
        })
    }

    /// Where a `VideoDecodeBin` puts its pictures: this device, holding
    /// `downstream_hw_frames` for whatever is after it.
    pub(in crate::engine) fn decode_target(&self, downstream_hw_frames: i32) -> DecodeTarget {
        match self {
            Self::Cuda(device) => DecodeTarget::Cuda {
                device: CudaDevice::clone(device),
                downstream_hw_frames,
            },
            Self::Vulkan(device) => DecodeTarget::Vulkan {
                device: device.clone(),
                downstream_hw_frames,
            },
        }
    }

    /// A compositor on this device composing in `format`: NV12 for the
    /// Canvas, which an encoder takes, and BGRA for a Scene laid over
    /// another, which has to be transparent where nothing was drawn.
    pub(in crate::engine) fn compositor(
        &self,
        name: String,
        options: VideoCompositorOptions,
        format: ChainFormat,
    ) -> Result<(CompositorElement, Compositor), BackendError> {
        Ok(match self {
            Self::Cuda(device) => {
                let (element, handle) =
                    CudaVideoCompositor::with_format(name, device, options, cuda_format(format))?;
                (CompositorElement::Cuda(element), Compositor::Cuda(handle))
            }
            Self::Vulkan(device) => {
                let format = match format {
                    ChainFormat::Bgra => VulkanFrameFormat::Bgra,
                    ChainFormat::Nv12 => VulkanFrameFormat::Nv12,
                };
                let (element, handle) =
                    VulkanVideoCompositor::with_format(name, device, options, format)?;
                (
                    CompositorElement::Vulkan(element),
                    Compositor::Vulkan(handle),
                )
            }
        })
    }
}

fn cuda_format(format: ChainFormat) -> CudaFrameFormat {
    match format {
        ChainFormat::Bgra => CudaFrameFormat::Bgra,
        ChainFormat::Nv12 => CudaFrameFormat::Nv12,
    }
}

/// A compositor not yet running, of either API.
///
/// A pipeline is built around its source by type, so this is how one is
/// started without the caller naming which: [`Self::pipeline`].
pub(in crate::engine) enum CompositorElement {
    Cuda(CudaVideoCompositor),
    Vulkan(VulkanVideoCompositor),
}

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
        fn attach<S: media_pp::element::Source, T>(
            source: &mut S,
            context: &Arc<media_pp::element::Context>,
            wire: impl FnOnce(
                &Arc<media_pp::element::Context>,
            ) -> media_pp::error::Result<(DetachedBranch, T)>,
        ) -> media_pp::error::Result<T> {
            let (branch, answer) = wire(context)?;
            context.attach(source, 0, branch)?;
            Ok(answer)
        }
        match self {
            Self::Cuda(element) => Pipeline::new(name, element, |source, context| {
                attach(source, context, wire)
            }),
            Self::Vulkan(element) => Pipeline::new(name, element, |source, context| {
                attach(source, context, wire)
            }),
        }
    }
}

/// A running compositor's control, of either API — what a Source registers
/// its input with.
#[derive(Clone)]
pub(in crate::engine) enum Compositor {
    Cuda(CudaVideoCompositorHandle),
    Vulkan(VulkanVideoCompositorHandle),
}

impl Compositor {
    pub(in crate::engine) fn add_source(
        &self,
        name: String,
        layer: VideoLayer,
    ) -> Result<CompositorInput<Layer>, BackendError> {
        fn add<C: VideoCompositorControl>(
            handle: &C,
            name: String,
            layer: VideoLayer,
            wrap: fn(C::Layer) -> Layer,
        ) -> Result<CompositorInput<Layer>, BackendError> {
            let CompositorInput { sink, layer } = handle.add_source(name, layer)?;
            Ok(CompositorInput {
                sink,
                layer: wrap(layer),
            })
        }
        match self {
            Self::Cuda(handle) => add(handle, name, layer, Layer::Cuda),
            Self::Vulkan(handle) => add(handle, name, layer, Layer::Vulkan),
        }
    }

    pub(in crate::engine) fn remove_source(&self, name: &str) {
        match self {
            Self::Cuda(handle) => handle.remove_source(name),
            Self::Vulkan(handle) => handle.remove_source(name),
        }
    }

    pub(in crate::engine) fn frame_rate(&self) -> Option<ffmpeg::Rational> {
        match self {
            Self::Cuda(handle) => handle.frame_rate(),
            Self::Vulkan(handle) => handle.frame_rate(),
        }
    }

    pub(in crate::engine) fn set_frame_rate(
        &self,
        rate: ffmpeg::Rational,
    ) -> Result<(), BackendError> {
        match self {
            Self::Cuda(handle) => handle.set_frame_rate(rate)?,
            Self::Vulkan(handle) => handle.set_frame_rate(rate)?,
        }
        Ok(())
    }
}

/// One compositor input's control, of either API — the backend's `Layer`.
#[derive(Clone)]
pub(in crate::engine) enum Layer {
    Cuda(CudaVideoLayerHandle),
    Vulkan(VulkanVideoLayerHandle),
}

impl Layer {
    pub(in crate::engine) fn set_layer(&self, layer: VideoLayer) -> media_pp::error::Result<()> {
        match self {
            Self::Cuda(handle) => VideoLayerControl::set_layer(handle, layer),
            Self::Vulkan(handle) => VideoLayerControl::set_layer(handle, layer),
        }
    }

    pub(in crate::engine) fn set_opacity(&self, opacity: f32) -> media_pp::error::Result<()> {
        match self {
            Self::Cuda(handle) => VideoLayerControl::set_opacity(handle, opacity),
            Self::Vulkan(handle) => VideoLayerControl::set_opacity(handle, opacity),
        }
    }

    pub(in crate::engine) fn set_visible(&self, visible: bool) -> media_pp::error::Result<()> {
        match self {
            Self::Cuda(handle) => VideoLayerControl::set_visible(handle, visible),
            Self::Vulkan(handle) => VideoLayerControl::set_visible(handle, visible),
        }
    }

    pub(in crate::engine) fn latest_frame(&self) -> Option<LayerFrame> {
        match self {
            Self::Cuda(handle) => handle.latest_frame(),
            Self::Vulkan(handle) => handle.latest_frame(),
        }
    }
}
