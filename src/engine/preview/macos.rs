//! The compositor's frames reaching egui on macOS: its NV12 planes copied on
//! the GPU into the two textures the NV12 resolve pass reads — see `nv12` —
//! and resolved there, as on Linux.
//!
//! # No system memory, and no import
//!
//! `media-pp`'s `MetalRenderer` hands each frame over as Metal textures made
//! over the `IOSurface` its pixel buffer is in, on whichever device it is
//! asked for — wgpu's own, here. So the planes are copied with a Metal blit
//! from those into the textures wgpu created, on wgpu's own command queue:
//! nothing is read back and nothing has to be shared between two devices,
//! which is what the Windows and CUDA paths spend their interop on. The copy
//! is waited for before the frame goes back to the compositor's pool, and
//! the resolve is submitted to the same queue after it, so it reads what the
//! copy wrote.
//!
//! # Why a copy at all
//!
//! For the reason Windows gives: the compositor hands out whichever pixel
//! buffer the next frame landed in, so there is no one texture to register
//! with egui once.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use media_pp::elements::{MetalFrame, MetalFramePlanes, MetalFrameRenderer, SubmitError};
use objc2::Message;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::{
    MTLBlitCommandEncoder, MTLCommandBuffer, MTLCommandBufferStatus, MTLCommandEncoder,
    MTLCommandQueue, MTLDevice, MTLTexture,
};

use super::Nv12Target;
use crate::engine::backend::BackendError;

/// wgpu's own Metal objects, reached once through its HAL: the device a
/// frame's textures are made on, the queue the copy goes on, and the two
/// plane textures it copies into.
struct Raw {
    device: Retained<ProtocolObject<dyn MTLDevice>>,
    queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
    luma: Retained<ProtocolObject<dyn MTLTexture>>,
    chroma: Retained<ProtocolObject<dyn MTLTexture>>,
}

/// The planes the Preview is resolved from, and whether anyone is looking at
/// it — the Linux twin's, with the frame copied rather than written.
pub(in crate::engine) struct PreviewSurface {
    device: wgpu::Device,
    queue: wgpu::Queue,
    target: Nv12Target,
    raw: Raw,
    size: [u32; 2],
    /// Set when the resolved texture has new content the Preview has not
    /// been told about; the counting sink clears it as it reports.
    drawn: Arc<AtomicBool>,
    visible: AtomicBool,
    /// The last frame that arrived while nobody was looking. The frame
    /// itself, since its textures are only its pixel buffer's for as long as
    /// it is held.
    pending: Mutex<Option<MetalFrame>>,
    /// One copy and resolve at a time: a frame kept while hidden is copied
    /// from the UI's thread, every other from the renderer's.
    copying: Mutex<()>,
}

// SAFETY: Metal's device, command queue and textures are thread-safe objects,
// which Apple documents as usable from any thread; the copy into the textures
// is serialised by `copying`, and the rest is wgpu's own `Send + Sync` types
// and plain data behind locks.
unsafe impl Send for PreviewSurface {}
// SAFETY: as above.
unsafe impl Sync for PreviewSurface {}

impl PreviewSurface {
    /// Starts visible, for the reason the Windows twin gives. `device` and
    /// `queue` are the ones `target` was made on: eframe's.
    pub(in crate::engine) fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target: Nv12Target,
        drawn: Arc<AtomicBool>,
    ) -> Result<Arc<Self>, BackendError> {
        let raw = raw(device, queue, &target)?;
        let size = target.size();
        mark_written(device, queue, &target);
        Ok(Arc::new(Self {
            device: device.clone(),
            queue: queue.clone(),
            target,
            raw,
            size,
            drawn,
            visible: AtomicBool::new(true),
            pending: Mutex::new(None),
            copying: Mutex::new(()),
        }))
    }

    /// Takes one composited frame, copying it only if there is anyone to see
    /// it. Returns whether the frame was accepted; one that is not the
    /// Canvas, or a copy that fails, is the only rejection.
    fn submit(&self, frame: MetalFrame) -> bool {
        if !self.visible.load(Ordering::Relaxed) {
            *self
                .pending
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(frame);
            return true;
        }
        self.copy(&frame)
    }

    /// Tells this whether anyone is looking. Coming back into view copies
    /// whatever arrived while nobody was.
    pub(in crate::engine) fn set_visible(&self, visible: bool) {
        self.visible.store(visible, Ordering::Relaxed);
        if !visible {
            return;
        }
        let pending = self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(frame) = pending {
            self.copy(&frame);
        }
    }

    fn copy(&self, frame: &MetalFrame) -> bool {
        let MetalFramePlanes::Nv12 { luma, chroma } = frame.planes() else {
            // The Canvas is composed in NV12, and nothing else feeds this.
            return false;
        };
        let fits = |from: &ProtocolObject<dyn MTLTexture>, to: &ProtocolObject<dyn MTLTexture>| {
            from.width() == to.width() && from.height() == to.height()
        };
        if [frame.width(), frame.height()] != self.size
            || !fits(luma, &self.raw.luma)
            || !fits(chroma, &self.raw.chroma)
        {
            return false;
        }
        let _copying = self
            .copying
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(commands) = self.raw.queue.commandBuffer() else {
            return false;
        };
        let Some(blit) = commands.blitCommandEncoder() else {
            return false;
        };
        // SAFETY: four live textures of matching size and format — checked
        // above, and the planes' formats are the ones `nv12` creates its
        // textures in — held until the command buffer has completed, which
        // is waited for below.
        unsafe {
            blit.copyFromTexture_toTexture(luma, &self.raw.luma);
            blit.copyFromTexture_toTexture(chroma, &self.raw.chroma);
        }
        blit.endEncoding();
        commands.commit();
        // Waited for, so the frame can go back to the compositor's pool as
        // soon as this returns and the resolve reads a finished copy. A
        // plane copy of the Canvas is well under a millisecond, and this
        // runs on the Preview queue's own thread.
        commands.waitUntilCompleted();
        if commands.status() != MTLCommandBufferStatus::Completed {
            return false;
        }
        self.target.resolve(&self.device, &self.queue);
        self.drawn.store(true, Ordering::Relaxed);
        true
    }
}

/// Writes the two planes once through wgpu, so it knows they hold something.
///
/// wgpu initialises a texture lazily: one it has never seen written is
/// cleared the first time a pass reads it. The copies here are Metal's, which
/// wgpu does not see, so without this the resolve pass had them cleared under
/// it and drew black. Waited for, so the clear cannot land after the first
/// copy.
fn mark_written(device: &wgpu::Device, queue: &wgpu::Queue, target: &Nv12Target) {
    let (luma, chroma) = target.planes();
    for plane in [luma, chroma] {
        let size = plane.size();
        let bytes_per_row = size.width * plane.format().block_copy_size(None).unwrap_or(1);
        queue.write_texture(
            plane.as_image_copy(),
            &vec![0; (bytes_per_row * size.height) as usize],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(size.height),
            },
            size,
        );
    }
    queue.submit([]);
    let _ = device.poll(wgpu::PollType::wait_indefinitely());
}

/// wgpu's device, queue and the target's two plane textures, as Metal sees
/// them.
fn raw(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    target: &Nv12Target,
) -> Result<Raw, BackendError> {
    use wgpu::hal::api::Metal;

    const NOT_METAL: &str = "wgpu is not running on Metal";
    let (luma, chroma) = target.planes();
    // One guard at a time, each let go of before the next is taken: a
    // guard holds wgpu's lock on its resources, which it does not take
    // twice, and holding two at once panics in a debug build.
    //
    // SAFETY: each guard hands back wgpu's live Metal object for as long as
    // it is held, and what is kept of it is a retain of its own, which
    // outlives the guard as any Objective-C reference does. Nothing here is
    // destroyed or used against wgpu's own tracking: the textures are only
    // ever written by a copy on wgpu's queue, which is how wgpu writes them
    // too.
    unsafe {
        let device = device
            .as_hal::<Metal>()
            .ok_or(NOT_METAL)?
            .raw_device()
            .clone();
        let queue = queue.as_hal::<Metal>().ok_or(NOT_METAL)?.as_raw().retain();
        let luma = luma
            .as_hal::<Metal>()
            .ok_or(NOT_METAL)?
            .raw_handle()
            .retain();
        let chroma = chroma
            .as_hal::<Metal>()
            .ok_or(NOT_METAL)?
            .raw_handle()
            .retain();
        Ok(Raw {
            device,
            queue,
            luma,
            chroma,
        })
    }
}

/// Puts each composited frame it is given into the Preview.
///
/// A `MetalFrameRenderer` is `media-pp`'s way to hand frames to an
/// application's own Metal drawing; this one's drawing is egui's. What it is
/// given is the `ChangeGate`'s business, and whether it is copied at all is
/// [`PreviewSurface`]'s — see the Windows twin.
pub(in crate::engine) struct PreviewRenderer {
    surface: Arc<PreviewSurface>,
}

impl PreviewRenderer {
    pub(in crate::engine) fn new(surface: Arc<PreviewSurface>) -> Self {
        Self { surface }
    }
}

impl MetalFrameRenderer for PreviewRenderer {
    /// wgpu's device, so the frame's textures are made where the copy runs.
    fn device(&self) -> Retained<ProtocolObject<dyn MTLDevice>> {
        self.surface.raw.device.clone()
    }

    fn submit(&self, frame: MetalFrame) -> Result<(), SubmitError> {
        if !self.surface.submit(frame) {
            return Err(SubmitError::InvalidFrame);
        }
        Ok(())
    }

    fn resize(&self, _width: u32, _height: u32) -> Result<(), SubmitError> {
        // The target is the Scene Canvas, not the window — see the Windows
        // twin.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use media_pp::{
        color::Color,
        elements::{
            MetalRenderer, MetalVideoCompositor, VideoCompositorOptions, VideoToolboxDevice,
            VideoToolboxFrameFormat,
        },
        ffmpeg,
        pipeline::Pipeline,
    };

    use super::*;

    /// A composited frame, the whole way to what the Preview samples: the
    /// Metal compositor fills its background, `MetalRenderer` hands the
    /// planes over on wgpu's device, they are copied into wgpu's textures
    /// and resolved, and the resolved texture is read back through wgpu.
    ///
    /// A vivid background rather than the black the Preview uses, because a
    /// copy that never happened reads as black too.
    ///
    /// Needs Metal and VideoToolbox. Where the machine has neither this says
    /// what it could not get and returns.
    #[test]
    fn a_composited_frame_reaches_what_the_preview_samples() {
        let Some((device, queue)) = metal_device() else {
            eprintln!("skipped: no Metal adapter on this machine");
            return;
        };
        let Ok(video_toolbox) = VideoToolboxDevice::new() else {
            eprintln!("skipped: no VideoToolbox on this machine");
            return;
        };

        let (width, height) = (64u32, 64u32);
        let drawn = Arc::new(AtomicBool::new(false));
        let surface = PreviewSurface::new(
            &device,
            &queue,
            Nv12Target::new(&device, width, height),
            Arc::clone(&drawn),
        )
        .expect("the Preview's surface");
        let (compositor, _handle) = MetalVideoCompositor::with_format(
            "test-compositor",
            &video_toolbox,
            VideoCompositorOptions {
                mode: media_pp::elements::RenderMode::Live,
                width,
                height,
                frame_rate: ffmpeg::Rational::new(30, 1),
                background: Color::new(255, 0, 0),
                background_alpha: 255,
            },
            VideoToolboxFrameFormat::Nv12,
        )
        .expect("compositor");
        let renderer = MetalRenderer::new(
            "test-out",
            Box::new(PreviewRenderer::new(Arc::clone(&surface))),
        );
        let (pipeline, ()) = Pipeline::new("test", compositor, |source, context| {
            let branch = context.branch().to(renderer)?;
            context.attach(source, 0, branch)?;
            Ok(())
        })
        .expect("pipeline");
        pipeline.run().expect("run");
        let deadline = Instant::now() + Duration::from_secs(5);
        while !drawn.load(Ordering::Relaxed) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        pipeline.stop();
        assert!(
            drawn.load(Ordering::Relaxed),
            "no composited frame reached the Preview"
        );

        // Red again, back in RGB — see the Linux twin.
        let resolved = read_texture(&device, &queue, surface.target.output_texture());
        for corner in [0, (width - 1) * 4, ((height - 1) * width + width - 1) * 4] {
            let pixel = &resolved[corner as usize..corner as usize + 4];
            assert_eq!(pixel[0], 255, "red at byte {corner}");
            assert!(pixel[1] <= 2, "green at byte {corner} was {}", pixel[1]);
            assert!(pixel[2] <= 2, "blue at byte {corner} was {}", pixel[2]);
            assert_eq!(pixel[3], 255, "alpha at byte {corner}");
        }
    }

    /// A frame that is not the Canvas's size is refused rather than copied
    /// into part of it.
    #[test]
    fn a_frame_of_another_size_is_refused() {
        let Some((device, queue)) = metal_device() else {
            eprintln!("skipped: no Metal adapter on this machine");
            return;
        };
        let Ok(video_toolbox) = VideoToolboxDevice::new() else {
            eprintln!("skipped: no VideoToolbox on this machine");
            return;
        };
        let drawn = Arc::new(AtomicBool::new(false));
        let surface = PreviewSurface::new(
            &device,
            &queue,
            Nv12Target::new(&device, 64, 64),
            Arc::clone(&drawn),
        )
        .expect("the Preview's surface");
        let (compositor, _handle) = MetalVideoCompositor::with_format(
            "test-compositor",
            &video_toolbox,
            VideoCompositorOptions {
                mode: media_pp::elements::RenderMode::Live,
                width: 32,
                height: 32,
                frame_rate: ffmpeg::Rational::new(30, 1),
                background: Color::new(255, 0, 0),
                background_alpha: 255,
            },
            VideoToolboxFrameFormat::Nv12,
        )
        .expect("compositor");
        let (refused, told) = mpsc::channel();
        let renderer = MetalRenderer::new(
            "test-out",
            Box::new(Refusals {
                inner: PreviewRenderer::new(surface),
                refused,
            }),
        );
        let (pipeline, ()) = Pipeline::new("test", compositor, |source, context| {
            let branch = context.branch().to(renderer)?;
            context.attach(source, 0, branch)?;
            Ok(())
        })
        .expect("pipeline");
        pipeline.run().expect("run");
        let answer = told.recv_timeout(Duration::from_secs(5));
        pipeline.stop();
        assert_eq!(answer, Ok(Err(SubmitError::InvalidFrame)));
        assert!(
            !drawn.load(Ordering::Relaxed),
            "a part-copied frame was drawn"
        );
    }

    /// Says what the renderer answered each frame.
    struct Refusals {
        inner: PreviewRenderer,
        refused: mpsc::Sender<Result<(), SubmitError>>,
    }

    impl MetalFrameRenderer for Refusals {
        fn device(&self) -> Retained<ProtocolObject<dyn MTLDevice>> {
            self.inner.device()
        }

        fn submit(&self, frame: MetalFrame) -> Result<(), SubmitError> {
            let answer = self.inner.submit(frame);
            let _ = self.refused.send(answer);
            answer
        }

        fn resize(&self, width: u32, height: u32) -> Result<(), SubmitError> {
            self.inner.resize(width, height)
        }
    }

    /// One resolved frame's RGBA bytes. The width here is 64, so a row is
    /// exactly the 256 bytes `copy_texture_to_buffer` insists on.
    fn read_texture(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        texture: &wgpu::Texture,
    ) -> Vec<u8> {
        let size = texture.size();
        let bytes = u64::from(size.width * size.height * 4);
        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("read-back-frame"),
            size: bytes,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &staging,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(size.width * 4),
                    rows_per_image: Some(size.height),
                },
            },
            size,
        );
        queue.submit([encoder.finish()]);
        let slice = staging.slice(..);
        let (mapped, done) = mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = mapped.send(result);
        });
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("poll");
        done.recv_timeout(Duration::from_secs(5))
            .expect("map never completed")
            .expect("map failed");
        let bytes = slice.get_mapped_range().expect("mapped range").to_vec();
        staging.unmap();
        bytes
    }

    /// A headless wgpu device on Metal, or nothing where there is none.
    fn metal_device() -> Option<(wgpu::Device, wgpu::Queue)> {
        let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
        descriptor.backends = wgpu::Backends::METAL;
        let instance = wgpu::Instance::new(descriptor);
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
                .ok()?;
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("preview-test"),
            ..Default::default()
        }))
        .ok()
    }
}
