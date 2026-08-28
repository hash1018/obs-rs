//! Turning the compositor's NV12 output into something egui can sample.
//!
//! egui draws one texture; NV12 is two planes at different resolutions and a
//! colour space that is not RGB. Converting on the CPU would undo the reason
//! the compositor moved to the GPU in the first place, so the two planes are
//! uploaded as they are and a small render pass resolves them into the RGBA
//! texture the Preview already knows how to draw. The UI side does not change.
//!
//! The planes are separate textures rather than one, because that is what the
//! shape of NV12 makes them: different resolutions, different formats. What
//! fills them is one buffer CUDA wrote the whole frame into — see [`super::shared`] —
//! so the upload is two `copy_buffer_to_texture` calls in the same submission
//! as the pass, and nothing here crosses the bus.

use super::shared::SharedNv12;

/// `CudaConverter` documents its output as BT.709 limited-range Y'CbCr from
/// full-range RGB, so this is that conversion run backwards. Guessing the
/// matrix produces a picture that looks almost right, which is worse than one
/// that looks wrong.
const SHADER: &str = r#"
struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vertex(@builtin(vertex_index) index: u32) -> VertexOut {
    // One oversized triangle rather than two: no vertex buffer, no seam.
    let x = f32((index << 1u) & 2u);
    let y = f32(index & 2u);
    var out: VertexOut;
    out.uv = vec2<f32>(x, y);
    out.position = vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
    return out;
}

@group(0) @binding(0) var luma_plane: texture_2d<f32>;
@group(0) @binding(1) var chroma_plane: texture_2d<f32>;
@group(0) @binding(2) var plane_sampler: sampler;

@fragment
fn fragment(in: VertexOut) -> @location(0) vec4<f32> {
    // 16..235 and 16..240 expanded to 0..1.
    let luma = (textureSample(luma_plane, plane_sampler, in.uv).r - 0.0627451) * 1.1643836;
    let chroma = (textureSample(chroma_plane, plane_sampler, in.uv).rg - vec2<f32>(0.5, 0.5))
        * 1.1383929;
    return vec4<f32>(
        luma + 1.5748 * chroma.y,
        luma - 0.1873243 * chroma.x - 0.4681243 * chroma.y,
        luma + 1.8556 * chroma.x,
        1.0,
    );
}
"#;

/// egui's renderer documents `Rgba8Unorm` for a registered texture.
const OUTPUT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

pub(super) struct Nv12Target {
    luma: wgpu::Texture,
    chroma: wgpu::Texture,
    /// The resolved frame. A view keeps its texture alive on its own, so
    /// this is held for one reason: a test reads back what the pass drew.
    _output: wgpu::Texture,
    output_view: wgpu::TextureView,
    bind_group: wgpu::BindGroup,
    pipeline: wgpu::RenderPipeline,
    size: [u32; 2],
}

impl Nv12Target {
    pub(super) fn new(device: &wgpu::Device, width: u32, height: u32) -> Self {
        let luma = plane(
            device,
            "composite-luma",
            wgpu::TextureFormat::R8Unorm,
            width,
            height,
        );
        // NV12 subsamples chroma by two in both directions, and carries Cb and
        // Cr interleaved in one two-channel plane.
        let chroma = plane(
            device,
            "composite-chroma",
            wgpu::TextureFormat::Rg8Unorm,
            width / 2,
            height / 2,
        );
        let mut usage =
            wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT;
        // Only a test ever reads the resolved frame back; the Preview samples
        // it where it lies.
        if cfg!(test) {
            usage |= wgpu::TextureUsages::COPY_SRC;
        }
        let output = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("composite-frame"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: OUTPUT_FORMAT,
            usage,
            view_formats: &[],
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("composite-plane-sampler"),
            // Linear across the chroma plane, which is half resolution and
            // would otherwise show its own blocks at the Viewport's scale.
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("nv12-to-rgba"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("nv12-to-rgba"),
            layout: None,
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vertex"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fragment"),
                targets: &[Some(OUTPUT_FORMAT.into())],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("nv12-planes"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(
                        &luma.create_view(&Default::default()),
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(
                        &chroma.create_view(&Default::default()),
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });

        Self {
            luma,
            chroma,
            output_view: output.create_view(&Default::default()),
            _output: output,
            bind_group,
            pipeline,
            size: [width, height],
        }
    }

    /// The texture the Preview samples. Registered once with egui.
    pub(super) fn output_view(&self) -> &wgpu::TextureView {
        &self.output_view
    }

    /// The same texture, for a test that reads back what the pass resolved.
    #[cfg(test)]
    pub(super) fn output_texture(&self) -> &wgpu::Texture {
        &self._output
    }

    /// Resolves the frame now in the shared buffer into the output texture.
    ///
    /// Returns `false` when the buffer was built for a different size, which
    /// would otherwise paint a torn picture rather than fail.
    pub(super) fn draw(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        shared: &SharedNv12,
    ) -> bool {
        let layout = shared.layout();
        if [layout.width, layout.height] != self.size {
            return false;
        }

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("nv12-to-rgba"),
        });
        // The two plane copies and the pass go in one submission: the buffer
        // is read and resolved before anything else can be recorded against
        // it, so the next frame's copy cannot overtake this one.
        copy_plane(&mut encoder, shared.buffer(), 0, layout.pitch, &self.luma);
        copy_plane(
            &mut encoder,
            shared.buffer(),
            layout.chroma_offset,
            layout.pitch,
            &self.chroma,
        );
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("nv12-to-rgba"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.output_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        // Every pixel is written, so there is nothing to keep.
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        queue.submit([encoder.finish()]);
        true
    }
}

/// Moves one plane out of the shared buffer and into its texture.
fn copy_plane(
    encoder: &mut wgpu::CommandEncoder,
    buffer: &wgpu::Buffer,
    offset: u64,
    pitch: u32,
    texture: &wgpu::Texture,
) {
    let size = texture.size();
    encoder.copy_buffer_to_texture(
        wgpu::TexelCopyBufferInfo {
            buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset,
                bytes_per_row: Some(pitch),
                rows_per_image: Some(size.height),
            },
        },
        texture.as_image_copy(),
        size,
    );
}

fn plane(
    device: &wgpu::Device,
    label: &str,
    format: wgpu::TextureFormat,
    width: u32,
    height: u32,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}
