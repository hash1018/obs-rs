//! The one region of memory the composited frame reaches wgpu through, held
//! by both CUDA and Vulkan.
//!
//! This is what replaces reading the composited frame back to system memory.
//! `CudaDownload` copied every NV12 frame to the CPU and `write_texture`
//! pushed it back across PCIe, so each Preview refresh crossed the bus twice
//! for pixels that never left the GPU in the first place.
//!
//! # Which API allocates
//!
//! Vulkan does, and CUDA imports. Not a preference: a CUDA allocation cannot
//! be named by Vulkan at all, while Vulkan can allocate memory it exports as
//! a file descriptor and `cuImportExternalMemory` takes exactly that. It is
//! also the direction `media-pp`'s own [`CudaFrameRenderer`] documents, since
//! it is the only one that works.
//!
//! [`CudaFrameRenderer`]: media_pp::elements::CudaFrameRenderer
//!
//! # Why a buffer rather than two images
//!
//! CUDA can map imported memory as a mipmapped array and write into an image
//! directly, but then Vulkan's image layout and tiling have to be right for
//! an API that knows nothing about either, and wgpu transitions layouts
//! whenever it pleases. A buffer has neither: it is linear, it has no layout,
//! and `copy_buffer_to_texture` moves it into the two plane textures the
//! resolve pass already samples — on the GPU, in the same submission.
//!
//! # Why a copy at all
//!
//! The compositor hands out whichever frame from its own pool the composite
//! landed in, so there is no one surface for Vulkan to import once. This
//! keeps its own and copies each frame in — device to device, never touching
//! the CPU.
//!
//! # Synchronization
//!
//! The CUDA copy is waited for on the thread that issued it, before the wgpu
//! commands that read the buffer are even recorded. That is coarser than the
//! external semaphore this could use and costs the wait of a copy that runs
//! in tens of microseconds, in exchange for not having to make wgpu's
//! submission order and a second cross-API primitive agree.

use std::ffi::{CStr, c_char, c_int, c_uint, c_void};
use std::os::fd::RawFd;
use std::sync::Mutex;

use ash::vk;

use super::super::BackendError;

/// What `copy_buffer_to_texture` requires of a row, so the CUDA copy writes
/// rows at this pitch rather than the plane's natural width.
const ROW_ALIGNMENT: u32 = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;

/// Where each plane sits inside the shared allocation.
///
/// Both planes carry one row per `pitch` bytes: luma is a byte per pixel and
/// chroma a `(U, V)` pair per 2x2 block, which is also one byte per pixel of
/// width — so the two pitches are the same number, over half as many rows.
#[derive(Clone, Copy)]
pub(super) struct Nv12Layout {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) pitch: u32,
    pub(super) chroma_offset: u64,
    pub(super) size: u64,
}

impl Nv12Layout {
    fn new(width: u32, height: u32) -> Self {
        let pitch = width.div_ceil(ROW_ALIGNMENT) * ROW_ALIGNMENT;
        let chroma_offset = u64::from(pitch) * u64::from(height);
        Self {
            width,
            height,
            pitch,
            chroma_offset,
            // Chroma is half the rows; NV12 heights are even by construction,
            // the compositor being built for the Canvas.
            size: chroma_offset + u64::from(pitch) * u64::from(height / 2),
        }
    }
}

/// The shared NV12 staging region, from both sides.
pub(super) struct SharedNv12 {
    layout: Nv12Layout,
    /// The frame's last two luma rows and the chroma row under them, copied
    /// back per frame — see [`SharedNv12::tail_is_unwritten`] for what reads
    /// them and why it is worth six kilobytes.
    tail: Mutex<Vec<u8>>,
    /// The CUDA side. Declared before the wgpu buffer so it is dropped first:
    /// the import has to go before the memory it addresses, and freeing that
    /// memory is what dropping the buffer eventually leads to.
    cuda: CudaImport,
    /// The wgpu side. Wgpu was handed the buffer as externally owned, so
    /// destroying the handle and freeing the memory happen in the callback it
    /// runs when it is finished with it — never here, and never while it
    /// still holds submitted work that reads it.
    buffer: wgpu::Buffer,
    /// Held to wait for wgpu's own work at teardown.
    device: wgpu::Device,
}

// SAFETY: every field is either owned data or a handle whose API is
// thread-safe to call from any thread — CUDA contexts are pushed per call
// below, and Vulkan buffers and memory are not thread-affine.
unsafe impl Send for SharedNv12 {}
// SAFETY: nothing is mutated through `&self` except the tail bytes, which are
// behind their own mutex. The copy itself goes GPU-side through handles that
// are set once at construction.
unsafe impl Sync for SharedNv12 {}

impl SharedNv12 {
    /// Allocates the region on wgpu's Vulkan device and imports it into the
    /// CUDA primary context the compositor's frames live on.
    pub(super) fn new(
        device: &wgpu::Device,
        width: u32,
        height: u32,
    ) -> Result<Self, BackendError> {
        let layout = Nv12Layout::new(width, height);
        let mut allocated = VulkanMemory::allocate(device, layout.size)?;

        // SAFETY: `allocated.buffer` was created on this very device, respects
        // the descriptor below — it is `layout.size` bytes and was created
        // with `TRANSFER_SRC`, which is what `COPY_SRC` means — and neither
        // it nor its memory is touched again except by the callback wgpu runs
        // when it has finished with the buffer, which is what
        // `from_raw_externally_owned` promises.
        let buffer = unsafe {
            let hal = wgpu::hal::vulkan::Buffer::from_raw_externally_owned(
                allocated.buffer,
                allocated.release_callback(),
            );
            device.create_buffer_from_hal::<wgpu::hal::api::Vulkan>(
                hal,
                &wgpu::BufferDescriptor {
                    label: Some("composite-shared"),
                    size: layout.size,
                    usage: wgpu::BufferUsages::COPY_SRC,
                    mapped_at_creation: false,
                },
            )
        };

        // The descriptor is the import's from here, whichever way it goes,
        // and a failed import leaves the memory to be released by dropping
        // the buffer that now owns it.
        let cuda = CudaImport::open(allocated.take_fd(), allocated.size)?;

        Ok(Self {
            layout,
            tail: Mutex::new(vec![0; 3 * width as usize]),
            cuda,
            buffer,
            device: device.clone(),
        })
    }

    pub(super) fn layout(&self) -> Nv12Layout {
        self.layout
    }

    pub(super) fn buffer(&self) -> &wgpu::Buffer {
        &self.buffer
    }

    /// Copies one composited frame into the shared region and waits for it.
    ///
    /// # Safety
    /// The two pointers must be the plane pointers of a live NV12 CUDA frame
    /// on the primary context — which is what `CudaRenderer` has already
    /// established before it calls its renderer.
    pub(super) unsafe fn write(
        &self,
        y: *const u8,
        y_pitch: usize,
        uv: *const u8,
        uv_pitch: usize,
        width: u32,
        height: u32,
    ) -> bool {
        if width != self.layout.width || height != self.layout.height {
            return false;
        }
        let (width, height) = (width as usize, height as usize);
        let luma = CudaMemcpy2D::device_to_device(
            y as u64,
            y_pitch,
            self.cuda.pointer,
            self.layout.pitch as usize,
            width,
            height,
        );
        let chroma = CudaMemcpy2D::device_to_device(
            uv as u64,
            uv_pitch,
            self.cuda.pointer + self.layout.chroma_offset,
            self.layout.pitch as usize,
            width,
            height / 2,
        );

        let mut tail = self
            .tail
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let tail_luma = CudaMemcpy2D::device_to_host(
            y as u64 + ((height - 2) * y_pitch) as u64,
            y_pitch,
            tail.as_mut_ptr(),
            width,
            width,
            2,
        );
        let tail_chroma = CudaMemcpy2D::device_to_host(
            uv as u64 + ((height / 2 - 1) * uv_pitch) as u64,
            uv_pitch,
            // SAFETY: the buffer is `3 * width` bytes, so this is its last
            // row and stays inside it.
            unsafe { tail.as_mut_ptr().add(2 * width) },
            width,
            width,
            1,
        );

        // SAFETY: the plane copies read the caller's planes, whose validity is
        // this function's own contract, and write inside the region imported
        // at construction: `pitch * height` bytes from zero and
        // `pitch * height / 2` from `chroma_offset`, which is how `size` was
        // computed. The two tail copies read the last rows of those same
        // planes into a host buffer sized for exactly them. The context is
        // this process's CUDA primary context, the one the planes live on.
        self.cuda
            .with_context(|| unsafe {
                check("cuMemcpy2D", cuMemcpy2D_v2(&luma))?;
                check("cuMemcpy2D", cuMemcpy2D_v2(&chroma))?;
                check("cuMemcpy2D", cuMemcpy2D_v2(&tail_luma))?;
                check("cuMemcpy2D", cuMemcpy2D_v2(&tail_chroma))?;
                // The wgpu commands that read this are recorded after this
                // returns, so waiting here is what orders the two APIs.
                check("cuCtxSynchronize", cuCtxSynchronize())
            })
            .inspect_err(|error| {
                report_once(
                    &COPY_FAILED,
                    format_args!("copying a composited frame into shared memory failed: {error}"),
                );
            })
            .is_ok()
    }

    /// Whether the frame just copied has a tail that was never written.
    ///
    /// Rapidly resizing a layer makes the compositor emit, roughly once in
    /// six hundred frames, a frame whose last rows are untouched — the shape
    /// of a linear write that stopped part way. Those bytes are zero, and
    /// zero is not a colour this pipeline produces: BT.709 limited-range
    /// black is Y=16 with both chroma at 128, and every Source arrives
    /// converted. So all three at zero means nothing wrote there.
    ///
    /// Dropping the frame costs one Preview refresh out of hundreds and is
    /// invisible; showing it is a flash of green, since that is what zeroed
    /// NV12 resolves to. The defect itself is upstream in `media-pp` and
    /// unfound — this only keeps it off the screen.
    ///
    /// The last rows are the one part of the frame that still comes back to
    /// the CPU, because they are the only part anything here has to read: six
    /// kilobytes of a three-megabyte frame, on the copy that was already
    /// being waited for. Reading them out of the shared allocation instead
    /// would be free, but memory a Vulkan buffer can export is never also
    /// host-visible on this driver.
    pub(super) fn tail_is_unwritten(&self) -> bool {
        let Nv12Layout { width, height, .. } = self.layout;
        if height < 2 || width < 2 {
            return false;
        }
        let tail = self
            .tail
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let width = width as usize;
        // The last two rows, spread across the width: the region always
        // reaches the bottom edge, being the tail of the frame.
        let unwritten = (0..8).any(|step| {
            let column = (width - 2) * step / 8;
            let chroma = 2 * width + (column / 2) * 2;
            (0..2).any(|row| {
                tail[row * width + column] == 0 && tail[chroma] == 0 && tail[chroma + 1] == 0
            })
        });
        if unwritten {
            report_once(
                &TAIL_UNWRITTEN,
                format_args!(
                    "a composited frame arrived partly unwritten and was dropped; this \
                     happens while a layer is resized quickly and is not yet understood. \
                     The Preview skips such frames."
                ),
            );
        }
        unwritten
    }
}

impl Drop for SharedNv12 {
    fn drop(&mut self) {
        // wgpu may hold submitted work that reads the buffer. Waiting for it
        // here is what lets the release callback run promptly rather than
        // leaving the allocation until the device itself goes.
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
    }
}

/// One exportable Vulkan buffer, before wgpu takes charge of its lifetime.
struct VulkanMemory {
    device: ash::Device,
    buffer: vk::Buffer,
    memory: vk::DeviceMemory,
    /// The allocation's size, which the driver may round up from what was
    /// asked for. The CUDA import names this, not the layout's own: an import
    /// that disagrees with the allocation is rejected.
    size: u64,
    /// The exported handle, until [`VulkanMemory::take_fd`] hands it on.
    fd: RawFd,
    /// Whether wgpu has been given the job of releasing these handles. Until
    /// it has, they are this value's own to destroy.
    released: bool,
}

impl VulkanMemory {
    /// Creates an exportable buffer of at least `size` bytes and exports it.
    fn allocate(wgpu_device: &wgpu::Device, size: u64) -> Result<Self, BackendError> {
        // SAFETY: the guard borrows the live Vulkan device behind this wgpu
        // device and everything below is done while it is held. Each call is
        // an `ash` wrapper over the entry point it names, with fully
        // initialized descriptors and out-parameters ash owns. Both failure
        // paths give back what they had taken.
        unsafe {
            if !wgpu_device
                .features()
                .contains(wgpu::Features::VULKAN_EXTERNAL_MEMORY_FD)
            {
                return Err("this Vulkan device has no VK_KHR_external_memory_fd, \
                            which is how the compositor's frames reach the Preview"
                    .into());
            }
            let hal = wgpu_device
                .as_hal::<wgpu::hal::api::Vulkan>()
                .ok_or("the Preview's wgpu device is not Vulkan")?;
            let device = hal.raw_device().clone();
            let instance = hal.shared_instance().raw_instance();
            let physical = hal.raw_physical_device();

            let mut exportable = vk::ExternalMemoryBufferCreateInfo::default()
                .handle_types(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD);
            let buffer = device.create_buffer(
                &vk::BufferCreateInfo::default()
                    .size(size)
                    .usage(vk::BufferUsageFlags::TRANSFER_SRC)
                    .sharing_mode(vk::SharingMode::EXCLUSIVE)
                    .push_next(&mut exportable),
                None,
            )?;

            let requirements = device.get_buffer_memory_requirements(buffer);
            let properties = instance.get_physical_device_memory_properties(physical);
            // Device-local, which is the whole point: memory the CPU could
            // also see would be system memory, which is what this replaced.
            // The driver narrows the choice further for memory that has to be
            // exportable, and that is where the host-visible types go.
            let index = memory_type(
                &properties,
                requirements.memory_type_bits,
                vk::MemoryPropertyFlags::DEVICE_LOCAL,
            );
            let Some(index) = index else {
                device.destroy_buffer(buffer, None);
                return Err("no device-local Vulkan memory can back the shared frame".into());
            };

            let mut exported = vk::ExportMemoryAllocateInfo::default()
                .handle_types(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD);
            // CUDA imports a dedicated allocation as such, and an import that
            // disagrees with how the memory was allocated is rejected.
            let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().buffer(buffer);
            let memory = device
                .allocate_memory(
                    &vk::MemoryAllocateInfo::default()
                        .allocation_size(requirements.size)
                        .memory_type_index(index)
                        .push_next(&mut exported)
                        .push_next(&mut dedicated),
                    None,
                )
                .inspect_err(|_| device.destroy_buffer(buffer, None))?;

            let mut allocated = Self {
                device: device.clone(),
                buffer,
                memory,
                size: requirements.size,
                fd: -1,
                released: false,
            };
            device.bind_buffer_memory(buffer, memory, 0)?;
            allocated.fd = ash::khr::external_memory_fd::Device::new(instance, &device)
                .get_memory_fd(
                    &vk::MemoryGetFdInfoKHR::default()
                        .memory(memory)
                        .handle_type(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD),
                )?;
            Ok(allocated)
        }
    }

    /// The exported descriptor, which is the caller's from here on.
    fn take_fd(&mut self) -> RawFd {
        std::mem::replace(&mut self.fd, -1)
    }

    /// The callback that gives the buffer and its memory back, for wgpu to
    /// run once it has finished with the handle. Taking it hands over the
    /// job, so this value stops doing it itself.
    fn release_callback(&mut self) -> wgpu::hal::DropCallback {
        self.released = true;
        let device = self.device.clone();
        let (buffer, memory) = (self.buffer, self.memory);
        Box::new(move || {
            // SAFETY: both handles were created on this device, wgpu has said
            // it is done with the buffer by running this, and nothing else
            // holds either — the CUDA import that addressed the memory is
            // dropped before the buffer that leads here.
            unsafe {
                device.destroy_buffer(buffer, None);
                device.free_memory(memory, None);
            }
        })
    }
}

impl Drop for VulkanMemory {
    /// Gives back whatever has not been handed on: the descriptor until
    /// [`VulkanMemory::take_fd`], and the buffer and its memory until
    /// [`VulkanMemory::release_callback`].
    fn drop(&mut self) {
        if self.fd >= 0 {
            // SAFETY: the descriptor came from `get_memory_fd` and reaches
            // this only while it is still this value's own.
            unsafe { close_fd(self.fd) };
        }
        if !self.released {
            // SAFETY: both handles were created on this device and, the
            // callback never having been taken, nothing else holds them.
            unsafe {
                self.device.destroy_buffer(self.buffer, None);
                self.device.free_memory(self.memory, None);
            }
        }
    }
}

fn memory_type(
    properties: &vk::PhysicalDeviceMemoryProperties,
    allowed: u32,
    wanted: vk::MemoryPropertyFlags,
) -> Option<u32> {
    properties.memory_types[..properties.memory_type_count as usize]
        .iter()
        .enumerate()
        .find(|(index, memory)| {
            allowed & (1 << index) != 0 && memory.property_flags.contains(wanted)
        })
        .map(|(index, _)| index as u32)
}

/// The CUDA half: the imported memory and the pointer it maps to.
///
/// The context is the device's **primary** context, retained here — which is
/// the same one `media-pp` makes FFmpeg use, and that identity is the whole
/// reason this import can address the compositor's frames at all.
struct CudaImport {
    device: CUdevice,
    context: CUcontext,
    memory: CUexternalMemory,
    pointer: CUdeviceptr,
}

impl CudaImport {
    /// Takes the exported descriptor, which is closed here or owned by the
    /// driver from here — never left to the caller.
    fn open(fd: RawFd, size: u64) -> Result<Self, BackendError> {
        // SAFETY: the driver calls run in the order it requires — `cuInit`
        // before anything else, a context current before memory is imported
        // into it — and every result is checked before the next call depends
        // on it. The descriptors are fully initialized locals, and `fd` is a
        // live file descriptor for exactly `size` bytes of exported Vulkan
        // memory, whose ownership passes to the driver once the import
        // succeeds. The failure path releases the context reference it took.
        unsafe {
            let mut device: CUdevice = 0;
            let mut context: CUcontext = std::ptr::null_mut();
            let retained = check("cuInit", cuInit(0))
                .and_then(|()| check("cuDeviceGet", cuDeviceGet(&mut device, 0)))
                .and_then(|()| {
                    check(
                        "cuDevicePrimaryCtxRetain",
                        cuDevicePrimaryCtxRetain(&mut context, device),
                    )
                });
            if let Err(error) = retained {
                // Nothing took the descriptor, so it is closed here.
                close_fd(fd);
                return Err(error);
            }

            let opened = (|| {
                if let Err(error) = check("cuCtxPushCurrent", cuCtxPushCurrent_v2(context)) {
                    close_fd(fd);
                    return Err(error);
                }
                let imported = import(fd, size);
                let mut popped: CUcontext = std::ptr::null_mut();
                check("cuCtxPopCurrent", cuCtxPopCurrent_v2(&mut popped))?;
                imported
            })();
            match opened {
                Ok((memory, pointer)) => Ok(Self {
                    device,
                    context,
                    memory,
                    pointer,
                }),
                Err(error) => {
                    cuDevicePrimaryCtxRelease_v2(device);
                    Err(error)
                }
            }
        }
    }

    fn with_context(
        &self,
        f: impl FnOnce() -> Result<(), BackendError>,
    ) -> Result<(), BackendError> {
        // SAFETY: `self.context` is the primary context this still holds a
        // reference to, so it can be made current for as long as `self`
        // lives, and the pop below balances the push on this same thread.
        unsafe { check("cuCtxPushCurrent", cuCtxPushCurrent_v2(self.context))? };
        let value = f();
        let mut popped: CUcontext = std::ptr::null_mut();
        // SAFETY: as above — `popped` is a live local for the context this
        // removes.
        unsafe { check("cuCtxPopCurrent", cuCtxPopCurrent_v2(&mut popped))? };
        value
    }
}

/// Imports the exported memory and maps the whole of it as one buffer.
///
/// Takes `fd` whatever happens: the driver owns it once the import succeeds
/// and closes it with the memory, and this closes it itself if the import is
/// the thing that failed.
///
/// # Safety
/// A CUDA context must be current, and `fd` must name `size` bytes of
/// dedicated exported memory that nothing else owns.
unsafe fn import(fd: RawFd, size: u64) -> Result<(CUexternalMemory, CUdeviceptr), BackendError> {
    let handle = CudaExternalMemoryHandleDesc {
        kind: CU_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD,
        handle: CudaExternalMemoryHandle { fd },
        size,
        flags: CUDA_EXTERNAL_MEMORY_DEDICATED,
        reserved: [0; 16],
    };
    let mut memory: CUexternalMemory = std::ptr::null_mut();
    // SAFETY: `handle` is a fully initialized descriptor whose active union
    // member matches the handle type it names, and `memory` is a live
    // out-parameter.
    let imported = unsafe {
        check(
            "cuImportExternalMemory",
            cuImportExternalMemory(&mut memory, &handle),
        )
    };
    if let Err(error) = imported {
        // SAFETY: the driver took nothing, so the descriptor is still this
        // function's to close.
        unsafe { close_fd(fd) };
        return Err(error);
    }

    let buffer = CudaExternalMemoryBufferDesc {
        offset: 0,
        size,
        flags: 0,
        reserved: [0; 16],
    };
    let mut pointer: CUdeviceptr = 0;
    // SAFETY: `memory` was just imported, the descriptor covers exactly what
    // it holds, and `pointer` is a live out-parameter.
    let mapped = unsafe {
        check(
            "cuExternalMemoryGetMappedBuffer",
            cuExternalMemoryGetMappedBuffer(&mut pointer, memory, &buffer),
        )
    };
    if let Err(error) = mapped {
        // SAFETY: `memory` is the handle imported above and nothing maps it.
        // Destroying it also releases the descriptor the driver took.
        unsafe { cuDestroyExternalMemory(memory) };
        return Err(error);
    }
    Ok((memory, pointer))
}

impl Drop for CudaImport {
    fn drop(&mut self) {
        // SAFETY: the mapped buffer is freed before the memory it came from
        // is destroyed, which is the order the driver documents, and both are
        // this type's own handles. The context has to be current for the free
        // and is popped again afterwards; the reference this retained is
        // released last.
        unsafe {
            if cuCtxPushCurrent_v2(self.context) == CUDA_SUCCESS {
                cuMemFree_v2(self.pointer);
                cuDestroyExternalMemory(self.memory);
                let mut popped: CUcontext = std::ptr::null_mut();
                cuCtxPopCurrent_v2(&mut popped);
            }
            cuDevicePrimaryCtxRelease_v2(self.device);
        }
    }
}

/// Closes a file descriptor nothing else owns.
///
/// # Safety
/// `fd` must be live and unowned.
unsafe fn close_fd(fd: RawFd) {
    unsafe extern "C" {
        fn close(fd: c_int) -> c_int;
    }
    // SAFETY: the caller's own contract.
    unsafe { close(fd) };
}

static COPY_FAILED: std::sync::Once = std::sync::Once::new();
static TAIL_UNWRITTEN: std::sync::Once = std::sync::Once::new();

/// Says something once, however often it happens. A per-frame failure would
/// otherwise write sixty lines a second about one broken thing.
fn report_once(once: &std::sync::Once, message: std::fmt::Arguments<'_>) {
    once.call_once(|| eprintln!("{message}"));
}

// The handful of CUDA driver entry points this interop calls, declared here
// in the same shape `media-pp` declares its own: the driver API is what the
// NVIDIA driver itself ships, so no CUDA toolkit is needed to build or run.
// `media-pp` keeps its bindings crate-private, and these are the other half
// of the arrangement its `CudaDevice` documents — an interop implementation
// retains the primary context and imports into it.

type CUresult = c_int;
type CUdevice = c_int;
type CUcontext = *mut c_void;
type CUexternalMemory = *mut c_void;
type CUdeviceptr = u64;

const CUDA_SUCCESS: CUresult = 0;
/// `CU_MEMORYTYPE_HOST`.
const CU_MEMORYTYPE_HOST: c_uint = 1;
/// `CU_MEMORYTYPE_DEVICE`.
const CU_MEMORYTYPE_DEVICE: c_uint = 2;
/// `CU_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD`.
const CU_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD: c_uint = 1;
/// `CUDA_EXTERNAL_MEMORY_DEDICATED`.
const CUDA_EXTERNAL_MEMORY_DEDICATED: c_uint = 1;

/// `CUDA_MEMCPY2D`, versioned by name (`cuMemcpy2D_v2`) and unchanged since
/// CUDA 4.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CudaMemcpy2D {
    src_x_in_bytes: usize,
    src_y: usize,
    src_memory_type: c_uint,
    src_host: *const c_void,
    src_device: CUdeviceptr,
    src_array: *mut c_void,
    src_pitch: usize,

    dst_x_in_bytes: usize,
    dst_y: usize,
    dst_memory_type: c_uint,
    dst_host: *mut c_void,
    dst_device: CUdeviceptr,
    dst_array: *mut c_void,
    dst_pitch: usize,

    width_in_bytes: usize,
    height: usize,
}

impl CudaMemcpy2D {
    fn device_to_device(
        source: CUdeviceptr,
        source_pitch: usize,
        destination: CUdeviceptr,
        destination_pitch: usize,
        width_in_bytes: usize,
        height: usize,
    ) -> Self {
        Self {
            src_memory_type: CU_MEMORYTYPE_DEVICE,
            src_device: source,
            src_pitch: source_pitch,
            dst_memory_type: CU_MEMORYTYPE_DEVICE,
            dst_device: destination,
            dst_pitch: destination_pitch,
            width_in_bytes,
            height,
            ..Self::default()
        }
    }

    fn device_to_host(
        source: CUdeviceptr,
        source_pitch: usize,
        destination: *mut u8,
        destination_pitch: usize,
        width_in_bytes: usize,
        height: usize,
    ) -> Self {
        Self {
            src_memory_type: CU_MEMORYTYPE_DEVICE,
            src_device: source,
            src_pitch: source_pitch,
            dst_memory_type: CU_MEMORYTYPE_HOST,
            dst_host: destination.cast(),
            dst_pitch: destination_pitch,
            width_in_bytes,
            height,
            ..Self::default()
        }
    }
}

/// The `handle` union of `CUDA_EXTERNAL_MEMORY_HANDLE_DESC`. Only the file
/// descriptor is ever set here; the other members are declared so the union
/// has the size and alignment the driver expects.
#[repr(C)]
union CudaExternalMemoryHandle {
    fd: c_int,
    win32: CudaExternalMemoryWin32Handle,
    nv_sci_buf_object: *const c_void,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CudaExternalMemoryWin32Handle {
    handle: *mut c_void,
    name: *const c_void,
}

#[repr(C)]
struct CudaExternalMemoryHandleDesc {
    kind: c_uint,
    handle: CudaExternalMemoryHandle,
    size: u64,
    flags: c_uint,
    reserved: [c_uint; 16],
}

#[repr(C)]
struct CudaExternalMemoryBufferDesc {
    offset: u64,
    size: u64,
    flags: c_uint,
    reserved: [c_uint; 16],
}

// SAFETY of the block: these are the driver's own C ABI declarations, and
// every call site above checks the returned `CUresult`.
#[link(name = "cuda")]
unsafe extern "C" {
    fn cuInit(flags: c_uint) -> CUresult;
    fn cuDeviceGet(device: *mut CUdevice, ordinal: c_int) -> CUresult;
    fn cuDevicePrimaryCtxRetain(ctx: *mut CUcontext, device: CUdevice) -> CUresult;
    fn cuDevicePrimaryCtxRelease_v2(device: CUdevice) -> CUresult;
    fn cuCtxPushCurrent_v2(ctx: CUcontext) -> CUresult;
    fn cuCtxPopCurrent_v2(ctx: *mut CUcontext) -> CUresult;
    fn cuCtxSynchronize() -> CUresult;
    fn cuMemcpy2D_v2(copy: *const CudaMemcpy2D) -> CUresult;
    fn cuMemFree_v2(pointer: CUdeviceptr) -> CUresult;
    fn cuImportExternalMemory(
        memory: *mut CUexternalMemory,
        handle: *const CudaExternalMemoryHandleDesc,
    ) -> CUresult;
    fn cuExternalMemoryGetMappedBuffer(
        pointer: *mut CUdeviceptr,
        memory: CUexternalMemory,
        buffer: *const CudaExternalMemoryBufferDesc,
    ) -> CUresult;
    fn cuDestroyExternalMemory(memory: CUexternalMemory) -> CUresult;
    fn cuGetErrorString(error: CUresult, string: *mut *const c_char) -> CUresult;
}

/// Turns a failed `CUresult` into an error that names both the call and what
/// the driver said about it.
fn check(call: &'static str, result: CUresult) -> Result<(), BackendError> {
    if result == CUDA_SUCCESS {
        return Ok(());
    }
    let mut text: *const c_char = std::ptr::null();
    // SAFETY: `text` is a live out-parameter, and the string the driver
    // writes into it is a static one belonging to the driver — read only
    // while it is known non-null, and copied before this returns.
    let message = unsafe {
        if cuGetErrorString(result, &mut text) == CUDA_SUCCESS && !text.is_null() {
            CStr::from_ptr(text).to_string_lossy().into_owned()
        } else {
            format!("CUDA error {result}")
        }
    };
    Err(format!("{call} failed: {message}").into())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::mpsc;
    use std::time::Duration;

    use media_pp::{
        color::Color,
        elements::{
            CudaDevice, CudaFrameRenderer, CudaRenderer, CudaVideoCompositor, SubmitError,
            VideoCompositorOptions,
        },
        ffmpeg,
        pipeline::Pipeline,
    };

    use super::*;

    /// A composited frame, read back out of the memory both APIs hold.
    ///
    /// The compositor fills its background on the GPU, `write` copies the
    /// result into the shared allocation with CUDA, and the assertions read
    /// it back through wgpu — so what this establishes is the import itself:
    /// the bytes CUDA wrote are the bytes the Vulkan buffer holds, at the
    /// pitch `copy_buffer_to_texture` will later read them at.
    ///
    /// A vivid background rather than the black the Preview uses, because
    /// black is `Y=16, U=V=128` and this has to fail if nothing wrote at all.
    ///
    /// Needs a CUDA device and a Vulkan device that can export memory. Where
    /// the machine has neither, this says what it could not get and returns
    /// rather than failing a build that never had a chance.
    #[test]
    fn a_composited_frame_reaches_the_shared_memory() {
        let Some((_instance, device, queue)) = exporting_device() else {
            eprintln!(
                "skipped: no Vulkan adapter offering VK_KHR_external_memory_fd on this machine"
            );
            return;
        };
        if media_pp::init().is_err() {
            eprintln!("skipped: ffmpeg would not initialize");
            return;
        }
        let Ok(cuda) = CudaDevice::new() else {
            eprintln!("skipped: no CUDA device on this machine");
            return;
        };

        let (width, height) = (64u32, 64u32);
        let shared = Arc::new(SharedNv12::new(&device, width, height).expect("shared memory"));

        let (compositor, _handle) = CudaVideoCompositor::new(
            "test-compositor",
            &cuda,
            VideoCompositorOptions {
                width,
                height,
                frame_rate: ffmpeg::Rational::new(30, 1),
                background: Color::new(255, 0, 0),
            },
        )
        .expect("compositor");

        let (written, arrived) = mpsc::channel();
        let renderer = CudaRenderer::new(
            "test-out",
            &cuda,
            Box::new(Probe {
                shared: Arc::clone(&shared),
                written,
            }),
        );
        let pipeline = Pipeline::new("test", compositor, |source, context| {
            let branch = context.branch().to(Box::new(renderer))?;
            context.attach(source, 0, branch)?;
            Ok(())
        })
        .expect("pipeline");
        pipeline.run().expect("run");
        let copied = arrived.recv_timeout(Duration::from_secs(5));
        assert_eq!(copied, Ok(true), "no composited frame reached the renderer");

        let frame = read_back(&device, &queue, &shared);

        // BT.709 limited range of full-range red, which is what the
        // compositor fills its background with: Y = 16 + 219/255 * 0.2126 *
        // 255 = 63, and the two chroma bytes from the same definition.
        // Sampled rather than scanned whole: a copy that lands at all lands
        // in full, and a pitch that disagreed would show at the far corner.
        let layout = shared.layout();
        let luma = |row: u32, column: u32| frame[(row * layout.pitch + column) as usize];
        let chroma = |row: u32, column: u32| {
            let at = (layout.chroma_offset + u64::from(row * layout.pitch + column * 2)) as usize;
            (frame[at], frame[at + 1])
        };
        for (row, column) in [(0, 0), (0, width - 1), (height - 1, width - 1)] {
            assert_eq!(luma(row, column), 63, "luma at {row},{column}");
            assert_eq!(
                chroma(row / 2, column / 2),
                (102, 240),
                "chroma at {row},{column}"
            );
        }

        // And the rest of the way: the two plane copies out of that buffer
        // and the pass that resolves them, which is what the Preview draws.
        // Red again, now back in RGB — 1.0 for red saturates, and the two
        // other channels come back to zero within the rounding of a byte.
        let target = super::super::nv12::Nv12Target::new(&device, width, height);
        assert!(
            target.draw(&device, &queue, &shared),
            "the resolve pass refused the frame"
        );
        let resolved = read_texture(&device, &queue, target.output_texture());
        for corner in [0, (width - 1) * 4, ((height - 1) * width + width - 1) * 4] {
            let pixel = &resolved[corner as usize..corner as usize + 4];
            assert_eq!(pixel[0], 255, "red at byte {corner}");
            assert!(pixel[1] <= 2, "green at byte {corner} was {}", pixel[1]);
            assert!(pixel[2] <= 2, "blue at byte {corner} was {}", pixel[2]);
            assert_eq!(pixel[3], 255, "alpha at byte {corner}");
        }
        pipeline.stop();
    }

    /// Copies whatever the compositor produced into the shared memory once
    /// and reports whether it went in.
    struct Probe {
        shared: Arc<SharedNv12>,
        written: mpsc::Sender<bool>,
    }

    impl CudaFrameRenderer for Probe {
        unsafe fn submit_nv12(
            &self,
            y: *const u8,
            y_pitch: usize,
            uv: *const u8,
            uv_pitch: usize,
            width: u32,
            height: u32,
        ) -> Result<(), SubmitError> {
            // SAFETY: `CudaRenderer` validated the frame these came from
            // before calling, which is this method's own contract.
            let written = unsafe { self.shared.write(y, y_pitch, uv, uv_pitch, width, height) };
            let _ = self.written.send(written);
            written.then_some(()).ok_or(SubmitError::InvalidFrame)
        }

        fn resize(&self, _width: u32, _height: u32) -> Result<(), SubmitError> {
            Ok(())
        }
    }

    /// The shared allocation's bytes, through wgpu — the same route the
    /// Preview's own `copy_buffer_to_texture` takes.
    fn read_back(device: &wgpu::Device, queue: &wgpu::Queue, shared: &SharedNv12) -> Vec<u8> {
        let size = shared.layout().size;
        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("read-back"),
            size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        encoder.copy_buffer_to_buffer(shared.buffer(), 0, &staging, 0, size);
        queue.submit([encoder.finish()]);
        map(device, &staging)
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
        map(device, &staging)
    }

    /// Waits for a buffer to be readable and copies what is in it.
    fn map(device: &wgpu::Device, staging: &wgpu::Buffer) -> Vec<u8> {
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

    /// A headless wgpu device that can export its memory, or nothing where
    /// no adapter offers that.
    fn exporting_device() -> Option<(wgpu::Instance, wgpu::Device, wgpu::Queue)> {
        let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
        descriptor.backends = wgpu::Backends::VULKAN;
        let instance = wgpu::Instance::new(descriptor);
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
                .ok()?;
        if !adapter
            .features()
            .contains(wgpu::Features::VULKAN_EXTERNAL_MEMORY_FD)
        {
            return None;
        }
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("interop-test"),
            required_features: wgpu::Features::VULKAN_EXTERNAL_MEMORY_FD,
            ..Default::default()
        }))
        .ok()?;
        Some((instance, device, queue))
    }
}
