//! Turning the compositor's BGRA output into something egui can sample.
//!
//! Simpler than the CUDA side's NV12 resolve: the D3D11 compositor already
//! works in BGRA, and `Bgra8Unorm` is a filterable wgpu format egui samples
//! like any other, so the downloaded frame is written straight into the one
//! registered texture — no render pass, no colour conversion.

use media_pp::ffmpeg;

pub(super) struct BgraTarget {
    texture: wgpu::Texture,
    output_view: wgpu::TextureView,
    size: [u32; 2],
}

impl BgraTarget {
    pub(super) fn new(device: &wgpu::Device, width: u32, height: u32) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("composite-frame"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Bgra8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let output_view = texture.create_view(&Default::default());
        Self {
            texture,
            output_view,
            size: [width, height],
        }
    }

    /// The texture the Preview samples. Registered once with egui.
    pub(super) fn output_view(&self) -> &wgpu::TextureView {
        &self.output_view
    }

    /// Writes one downloaded BGRA frame into the output texture.
    ///
    /// Returns `false` when the frame does not match the size this was built
    /// for, which would otherwise paint a torn picture rather than fail.
    pub(super) fn draw(&self, queue: &wgpu::Queue, frame: &ffmpeg::frame::Video) -> bool {
        let [width, height] = self.size;
        if frame.width() != width || frame.height() != height {
            return false;
        }
        // FFmpeg pads rows to its own alignment, so the frame's stride is
        // passed through: `write_texture` re-strides into its staging buffer.
        let data = frame.data(0);
        let stride = frame.stride(0);
        let needed = stride * height as usize;
        if data.len() < needed || stride < width as usize * 4 {
            return false;
        }
        queue.write_texture(
            self.texture.as_image_copy(),
            &data[..needed],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(stride as u32),
                rows_per_image: Some(height),
            },
            self.texture.size(),
        );
        true
    }
}
