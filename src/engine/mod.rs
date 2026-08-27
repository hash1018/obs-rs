//! The compositing engine: everything that produces Preview pixels.
//!
//! It runs on its own thread for the same reason `ProjectManager` does — the
//! work does not belong on the UI thread — but the pressure here is different.
//! A 1920x1080 frame is 8 MB, so at 60 fps this moves roughly half a gigabyte
//! per second into GPU memory. Doing that inside `eframe::App::ui` would pay
//! for every frame twice: once to build it, once in the dropped input latency.
//!
//! What crosses the thread boundary is therefore not pixels but a
//! [`CompositeFrame`] — an already-uploaded texture the UI only has to name.
//! The upload itself happens here, through a `wgpu::Queue` clone; `Queue` and
//! `Device` are `Send + Sync` and internally reference-counted, so the engine
//! shares eframe's device rather than opening a second one.

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU32, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use arc_swap::ArcSwapOption;
use eframe::egui;
use eframe::egui_wgpu::RenderState;

use crate::domain::SceneCanvas;

/// How often the engine composites. Independent of the egui repaint rate:
/// egui redraws when something asks it to, this advances on its own clock.
const TARGET_FPS: f32 = 60.0;

/// egui samples a registered texture directly, and its renderer documents
/// `Rgba8Unorm` as the only format it accepts for one.
const FRAME_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// One composited frame, already resident on the GPU.
///
/// The `TextureId` stays valid for the life of the engine: the texture is
/// created and registered once, and each frame overwrites its contents rather
/// than producing a new texture. Registering per frame would take the egui
/// renderer's write lock every frame and stall the very thread this exists to
/// keep free.
pub struct CompositeFrame {
    pub texture_id: egui::TextureId,
}

pub struct EngineManager {
    frame: Arc<ArcSwapOption<CompositeFrame>>,
    /// `f32` bits, so the UI can read the rate without a lock.
    active_fps: Arc<AtomicU32>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl EngineManager {
    pub fn spawn(
        render_state: RenderState,
        canvas: SceneCanvas,
        wake_ui: impl Fn() + Send + 'static,
    ) -> std::io::Result<Self> {
        let size = [canvas.width as u32, canvas.height as u32];
        let frame = Arc::new(ArcSwapOption::empty());
        let active_fps = Arc::new(AtomicU32::new(0));
        let stop = Arc::new(AtomicBool::new(false));

        let worker = thread::Builder::new().name("engine".to_owned()).spawn({
            let frame = Arc::clone(&frame);
            let active_fps = Arc::clone(&active_fps);
            let stop = Arc::clone(&stop);
            move || run(render_state, size, &frame, &active_fps, &stop, &wake_ui)
        })?;

        Ok(Self {
            frame,
            active_fps,
            stop,
            worker: Some(worker),
        })
    }

    /// The most recent composited frame, or `None` before the first one.
    pub fn frame(&self) -> Option<Arc<CompositeFrame>> {
        self.frame.load_full()
    }

    /// Measured output rate, or `None` until a full window has been observed.
    pub fn active_fps(&self) -> Option<f32> {
        let bits = self.active_fps.load(Ordering::Relaxed);
        (bits != 0).then(|| f32::from_bits(bits))
    }

    pub fn target_fps(&self) -> f32 {
        TARGET_FPS
    }
}

impl Drop for EngineManager {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.thread().unpark();
            let _ = worker.join();
        }
    }
}

fn run(
    render_state: RenderState,
    size: [u32; 2],
    frame: &ArcSwapOption<CompositeFrame>,
    active_fps: &AtomicU32,
    stop: &AtomicBool,
    wake_ui: &impl Fn(),
) {
    let [width, height] = size;
    let texture = render_state
        .device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("composite-frame"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FRAME_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let texture_id = render_state.renderer.write().register_native_texture(
        &render_state.device,
        &view,
        wgpu::FilterMode::Linear,
    );

    let mut pixels = Placeholder::new(width, height);
    let interval = Duration::from_secs_f32(1.0 / TARGET_FPS);
    let mut rate = FrameRate::new();
    let mut next_tick = Instant::now();

    while !stop.load(Ordering::Acquire) {
        pixels.advance();
        render_state.queue.write_texture(
            texture.as_image_copy(),
            pixels.bytes(),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: Some(height),
            },
            texture.size(),
        );
        frame.store(Some(Arc::new(CompositeFrame { texture_id })));
        if let Some(measured) = rate.tick() {
            active_fps.store(measured.to_bits(), Ordering::Relaxed);
        }
        wake_ui();

        next_tick += interval;
        let now = Instant::now();
        if next_tick > now {
            thread::park_timeout(next_tick - now);
        } else {
            // Fell behind; give up the missed ticks rather than sprinting to
            // catch up, which would only make the next frame later still.
            next_tick = now;
        }
    }
}

/// Stand-in frame content until a real compositor produces one.
///
/// Deliberately a full-size repaint every tick rather than a static image: the
/// point of this stage is to prove the upload path carries a moving picture at
/// the real frame size and rate, which a texture written once would not.
struct Placeholder {
    bytes: Vec<u8>,
    width: u32,
    height: u32,
    tick: u32,
}

impl Placeholder {
    fn new(width: u32, height: u32) -> Self {
        Self {
            bytes: vec![0; (width as usize) * (height as usize) * 4],
            width,
            height,
            tick: 0,
        }
    }

    fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    fn advance(&mut self) {
        self.tick = self.tick.wrapping_add(1);
        let phase = self.tick as f32 / TARGET_FPS;
        let row_bytes = (self.width as usize) * 4;

        // One row is built and then copied down the frame: 1080 memcpys cost
        // far less than four million per-pixel writes, which matters in debug
        // builds where this would otherwise dominate the frame.
        let mut row = vec![0u8; row_bytes];
        for (x, pixel) in row.chunks_exact_mut(4).enumerate() {
            let across = x as f32 / self.width as f32;
            pixel.copy_from_slice(&[
                channel(across + phase * 0.15),
                channel(across + phase * 0.15 + 0.33),
                channel(across + phase * 0.15 + 0.66),
                255,
            ]);
        }
        for line in self.bytes.chunks_exact_mut(row_bytes) {
            line.copy_from_slice(&row);
        }

        // A band sweeping down the frame, so a stalled pipeline is obvious at
        // a glance rather than looking like a slowly shifting gradient.
        let band_height = self.height / 24;
        let top = ((phase * 0.25).fract() * self.height as f32) as u32;
        let bottom = (top + band_height).min(self.height);
        let band = vec![255u8; row_bytes];
        for line in top..bottom {
            let start = (line as usize) * row_bytes;
            self.bytes[start..start + row_bytes].copy_from_slice(&band);
        }
    }
}

fn channel(position: f32) -> u8 {
    let wave = (position.fract() * std::f32::consts::TAU).sin();
    (((wave + 1.0) * 0.5) * 190.0 + 40.0) as u8
}

/// Counts composited frames over a rolling one-second window.
struct FrameRate {
    window_start: Instant,
    frames: u32,
}

impl FrameRate {
    fn new() -> Self {
        Self {
            window_start: Instant::now(),
            frames: 0,
        }
    }

    fn tick(&mut self) -> Option<f32> {
        self.frames += 1;
        let elapsed = self.window_start.elapsed();
        if elapsed < Duration::from_secs(1) {
            return None;
        }
        let measured = self.frames as f32 / elapsed.as_secs_f32();
        self.window_start = Instant::now();
        self.frames = 0;
        Some(measured)
    }
}
