//! The one texture the compositor's frames reach the Preview through, shared
//! between `media-pp`'s D3D11 device and wgpu's D3D12 one.
//!
//! This is what replaces reading the composited frame back to system memory.
//! That download measured as essentially the whole of this application's GPU
//! cost — around 17.7 of 18.1 percentage points, since `Map`ping a staging
//! texture stalls on the GPU and then copies 8 MiB per frame across PCIe —
//! while capture and compositing together came to 0.45.
//!
//! # Why a copy at all
//!
//! The compositor owns its output pool and hands out whichever texture the
//! next frame landed in, so there is no one texture for wgpu to import once.
//! This module keeps its own, imports that once, and copies each frame into
//! it — a GPU-to-GPU `CopyResource`, not a readback.
//!
//! # Why the flags are what they are
//!
//! `D3D11_RESOURCE_MISC_SHARED_NTHANDLE` alone is rejected at creation, and
//! the legacy `SHARED` flag produces a handle `ID3D12Device::OpenSharedHandle`
//! will not take. Only `NTHANDLE | KEYEDMUTEX` opens, so the keyed mutex is a
//! creation requirement rather than a choice — and it cannot double as the
//! synchronization, because the resource D3D12 opens does not expose
//! `IDXGIKeyedMutex` at all.

use std::sync::{Arc, Mutex};

use eframe::egui;
use eframe::egui_wgpu::RenderState;
use windows::Win32::Graphics::{
    Direct3D11::{
        D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE,
        D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX, D3D11_RESOURCE_MISC_SHARED_NTHANDLE,
        D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, ID3D11Device, ID3D11DeviceContext,
        ID3D11Resource, ID3D11Texture2D,
    },
    Direct3D12::ID3D12Resource,
    Dxgi::{
        Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC},
        IDXGIKeyedMutex, IDXGIResource1,
    },
};
use windows::core::Interface;

use super::BackendError;

/// `DXGI_SHARED_RESOURCE_READ | DXGI_SHARED_RESOURCE_WRITE`, which windows-rs
/// generates no constants for.
const SHARED_RESOURCE_READ_WRITE: u32 = 0x8000_0000 | 0x4000_0000;

/// The one texture both APIs see, and the egui id naming its wgpu side.
pub(super) struct SharedTarget {
    /// The D3D11 view of it — what `CopyResource` writes into.
    texture: ID3D11Texture2D,
    /// The keyed mutex the texture had to be created with, cached rather than
    /// cast for out of the texture on every frame.
    ///
    /// Taken and released around each copy even though nothing on the D3D12
    /// side ever takes it — it cannot, the resource opened there does not
    /// expose one. That is not pointless: acquiring is also what makes the
    /// writes of one device visible to another, and without it the imported
    /// texture reads as though nothing had ever been copied into it.
    keyed_mutex: IDXGIKeyedMutex,
    /// Held only so the imported texture outlives the view egui samples.
    _imported: wgpu::Texture,
    texture_id: egui::TextureId,
    size: [u32; 2],
}

impl SharedTarget {
    /// Creates the texture on `device` and imports it into wgpu's own device.
    ///
    /// Both must be on the same adapter. They are: wgpu is pinned to D3D12 on
    /// the default adapter and this backend creates its D3D11 device there
    /// too.
    pub(super) fn new(
        device: &ID3D11Device,
        render_state: &RenderState,
        width: u32,
        height: u32,
    ) -> Result<Self, BackendError> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0 as u32
                | D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX.0 as u32,
        };
        let mut texture: Option<ID3D11Texture2D> = None;
        // SAFETY: `desc` is fully initialized, no initial data is supplied,
        // and `texture` is a live out-parameter.
        unsafe { device.CreateTexture2D(&desc, None, Some(&mut texture))? };
        let texture = texture.expect("CreateTexture2D succeeded without producing a texture");

        // SAFETY: the texture carries `SHARED_NTHANDLE`, which is what makes
        // it castable to `IDXGIResource1` and shareable at all. The handle is
        // owned here and closed once the import has taken its own reference.
        let handle = unsafe {
            texture.cast::<IDXGIResource1>()?.CreateSharedHandle(
                None,
                SHARED_RESOURCE_READ_WRITE,
                None,
            )?
        };
        let imported = import_into_wgpu(render_state, handle, width, height);
        // SAFETY: `handle` came from `CreateSharedHandle` just above and is
        // not used again.
        let _ = unsafe { windows::Win32::Foundation::CloseHandle(handle) };
        let imported = imported?;

        let view = imported.create_view(&Default::default());
        let texture_id = render_state.renderer.write().register_native_texture(
            &render_state.device,
            &view,
            wgpu::FilterMode::Linear,
        );

        let keyed_mutex = texture.cast::<IDXGIKeyedMutex>()?;

        Ok(Self {
            texture,
            keyed_mutex,
            _imported: imported,
            texture_id,
            size: [width, height],
        })
    }

    /// The egui id naming this texture. Registered once and never again: its
    /// contents change, its identity does not.
    pub(super) fn texture_id(&self) -> egui::TextureId {
        self.texture_id
    }

    /// Copies one composited frame in.
    ///
    /// Returns `false` for a frame that is not the size this was built for,
    /// which `CopyResource` would reject anyway.
    pub(super) fn copy_from(
        &self,
        context: &Arc<Mutex<ID3D11DeviceContext>>,
        source: &ID3D11Texture2D,
        width: u32,
        height: u32,
    ) -> bool {
        if [width, height] != self.size {
            return false;
        }
        let (Ok(destination), Ok(source)) = (
            self.texture.cast::<ID3D11Resource>(),
            source.cast::<ID3D11Resource>(),
        ) else {
            return false;
        };
        let context = context
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // SAFETY: key 0 is always free — nothing else ever takes this mutex,
        // see the field's own docs — so the wait cannot deadlock, and the
        // release below is unconditional on the acquire having succeeded.
        unsafe {
            if self.keyed_mutex.AcquireSync(0, u32::MAX).is_err() {
                return false;
            }
            // SAFETY: both are BGRA textures of identical dimensions on this
            // backend's one device, and the shared context is held across the
            // copy exactly as every other consumer of it does.
            context.CopyResource(&destination, &source);
            // Without this the copy can sit in D3D11's own command buffer
            // while the other device is already reading the texture.
            context.Flush();
            let _ = self.keyed_mutex.ReleaseSync(0);
        }
        true
    }
}

/// Opens `handle` on the device wgpu is running and wraps it as a texture.
fn import_into_wgpu(
    render_state: &RenderState,
    handle: windows::Win32::Foundation::HANDLE,
    width: u32,
    height: u32,
) -> Result<wgpu::Texture, BackendError> {
    // SAFETY: `as_hal` hands back the live D3D12 device behind this wgpu
    // device for as long as the guard is held, and `handle` is the
    // shared-resource handle created by the caller just above.
    let resource = unsafe {
        let hal_device = render_state
            .device
            .as_hal::<wgpu::hal::api::Dx12>()
            .ok_or("wgpu is not running on Direct3D 12")?;
        let mut opened: Option<ID3D12Resource> = None;
        hal_device
            .raw_device()
            .OpenSharedHandle(handle, &mut opened)?;
        opened.ok_or("OpenSharedHandle succeeded without producing a resource")?
    };

    let size = wgpu::Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    };
    // SAFETY: the resource was opened from a texture created with exactly
    // these dimensions and format. `UNINITIALIZED` claims nothing about a
    // driver-side state wgpu would then have to match.
    let texture = unsafe {
        let hal_texture = wgpu::hal::dx12::Device::texture_from_raw(
            resource,
            wgpu::TextureFormat::Bgra8Unorm,
            wgpu::TextureDimension::D2,
            size,
            1,
            1,
        );
        render_state
            .device
            .create_texture_from_hal::<wgpu::hal::api::Dx12>(
                hal_texture,
                &wgpu::TextureDescriptor {
                    label: Some("composite-shared"),
                    size,
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Bgra8Unorm,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                },
                wgpu::wgt::TextureUses::UNINITIALIZED,
            )
    };
    Ok(texture)
}
