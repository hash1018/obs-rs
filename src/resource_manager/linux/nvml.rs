//! NVIDIA per-process GPU sampling through a runtime-loaded NVML.
//!
//! Loading dynamically keeps NVIDIA an optional runtime capability: systems
//! using Mesa do not need NVML installed and continue through DRM fdinfo.

use std::{ffi::c_void, ptr};

use libloading::Library;

const NVML_SUCCESS: i32 = 0;
const NVML_ERROR_NOT_FOUND: i32 = 6;
const NVML_ERROR_INSUFFICIENT_SIZE: i32 = 7;

type NvmlDevice = *mut c_void;
type Init = unsafe extern "C" fn() -> i32;
type Shutdown = unsafe extern "C" fn() -> i32;
type DeviceGetCount = unsafe extern "C" fn(*mut u32) -> i32;
type DeviceGetHandleByIndex = unsafe extern "C" fn(u32, *mut NvmlDevice) -> i32;
type DeviceGetProcessUtilization =
    unsafe extern "C" fn(NvmlDevice, *mut ProcessUtilizationSample, *mut u32, u64) -> i32;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct ProcessUtilizationSample {
    pid: u32,
    timestamp: u64,
    sm_util: u32,
    mem_util: u32,
    enc_util: u32,
    dec_util: u32,
}

struct Device {
    handle: NvmlDevice,
    last_seen_timestamp: u64,
}

pub(super) struct NvmlSampler {
    _library: Library,
    shutdown: Shutdown,
    get_process_utilization: DeviceGetProcessUtilization,
    devices: Vec<Device>,
    own_pid: u32,
}

impl NvmlSampler {
    pub(super) fn new() -> Option<Self> {
        // SAFETY: loading a library can run its initializers. This is the
        // NVIDIA driver's documented management library, selected by its
        // versioned SONAME rather than by an application-controlled path.
        let library = unsafe { Library::new("libnvidia-ml.so.1") }.ok()?;
        // SAFETY: each name and signature below is defined by the stable NVML
        // C ABI. Function pointers remain valid because `library` is retained.
        let init = unsafe { load::<Init>(&library, b"nvmlInit_v2\0")? };
        // SAFETY: same stable ABI and retained-library reasoning as above.
        let shutdown = unsafe { load::<Shutdown>(&library, b"nvmlShutdown\0")? };
        // SAFETY: same stable ABI and retained-library reasoning as above.
        let get_device_count =
            unsafe { load::<DeviceGetCount>(&library, b"nvmlDeviceGetCount_v2\0")? };
        // SAFETY: same stable ABI and retained-library reasoning as above.
        let get_device = unsafe {
            load::<DeviceGetHandleByIndex>(&library, b"nvmlDeviceGetHandleByIndex_v2\0")?
        };
        // SAFETY: same stable ABI and retained-library reasoning as above.
        let get_process_utilization = unsafe {
            load::<DeviceGetProcessUtilization>(&library, b"nvmlDeviceGetProcessUtilization\0")?
        };

        // SAFETY: NVML was loaded successfully and takes no arguments here.
        if unsafe { init() } != NVML_SUCCESS {
            return None;
        }

        let mut device_count = 0;
        // SAFETY: NVML is initialized and the output points to a live `u32`.
        if unsafe { get_device_count(&mut device_count) } != NVML_SUCCESS {
            // SAFETY: initialization succeeded, so it must be balanced.
            let _ = unsafe { shutdown() };
            return None;
        }

        let mut devices = Vec::with_capacity(device_count as usize);
        for index in 0..device_count {
            let mut handle = ptr::null_mut();
            // SAFETY: NVML is initialized and writes one opaque handle.
            if unsafe { get_device(index, &mut handle) } == NVML_SUCCESS && !handle.is_null() {
                devices.push(Device {
                    handle,
                    last_seen_timestamp: 0,
                });
            }
        }
        if devices.is_empty() {
            // SAFETY: initialization succeeded, so it must be balanced.
            let _ = unsafe { shutdown() };
            return None;
        }

        Some(Self {
            _library: library,
            shutdown,
            get_process_utilization,
            devices,
            own_pid: std::process::id(),
        })
    }

    pub(super) fn sample(&mut self) -> Option<f32> {
        let mut sampled_device = false;
        let mut busiest = 0;
        for device in &mut self.devices {
            let Some(samples) = query_samples(self.get_process_utilization, device) else {
                continue;
            };
            sampled_device = true;
            for sample in samples {
                if sample.pid != self.own_pid {
                    continue;
                }
                busiest = busiest.max(sample_utilization(sample));
            }
        }
        sampled_device.then_some(busiest as f32)
    }
}

impl Drop for NvmlSampler {
    fn drop(&mut self) {
        // SAFETY: this sampler owns one successful NVML initialization and
        // the library remains loaded until after `drop` returns.
        let _ = unsafe { (self.shutdown)() };
    }
}

fn query_samples(
    get_process_utilization: DeviceGetProcessUtilization,
    device: &mut Device,
) -> Option<Vec<ProcessUtilizationSample>> {
    let mut count = 0;
    // SAFETY: `device.handle` came from NVML; a null sample pointer is the
    // documented size query and `count` is a live output.
    let status = unsafe {
        get_process_utilization(
            device.handle,
            ptr::null_mut(),
            &mut count,
            device.last_seen_timestamp,
        )
    };
    if status == NVML_ERROR_NOT_FOUND {
        return Some(Vec::new());
    }
    if status != NVML_SUCCESS && status != NVML_ERROR_INSUFFICIENT_SIZE {
        return None;
    }
    if count == 0 {
        return Some(Vec::new());
    }

    let mut samples = vec![ProcessUtilizationSample::default(); count as usize];
    // SAFETY: the buffer has capacity for `count` ABI-compatible entries and
    // both the device handle and count pointer are valid for this call.
    let status = unsafe {
        get_process_utilization(
            device.handle,
            samples.as_mut_ptr(),
            &mut count,
            device.last_seen_timestamp,
        )
    };
    if status != NVML_SUCCESS {
        return None;
    }
    samples.truncate(count as usize);
    if let Some(timestamp) = samples.iter().map(|sample| sample.timestamp).max() {
        device.last_seen_timestamp = timestamp;
    }
    Some(samples)
}

fn sample_utilization(sample: ProcessUtilizationSample) -> u32 {
    [
        sample.sm_util,
        sample.mem_util,
        sample.enc_util,
        sample.dec_util,
    ]
    .into_iter()
    .filter(|value| *value <= 100)
    .max()
    .unwrap_or(0)
}

unsafe fn load<T: Copy>(library: &Library, name: &[u8]) -> Option<T> {
    // SAFETY: callers provide the exact NVML symbol signature and keep the
    // library alive for at least as long as the copied function pointer.
    unsafe { library.get::<T>(name) }.ok().map(|symbol| *symbol)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utilization_uses_the_busiest_available_engine() {
        assert_eq!(
            sample_utilization(ProcessUtilizationSample {
                sm_util: 32,
                mem_util: 14,
                enc_util: 61,
                dec_util: u32::MAX,
                ..Default::default()
            }),
            61
        );
    }

    #[test]
    fn installed_nvml_returns_a_process_percentage() {
        let Some(mut sampler) = NvmlSampler::new() else {
            return;
        };
        let percent = sampler
            .sample()
            .expect("an initialized NVML sampler should query a device");
        assert!((0.0..=100.0).contains(&percent), "{percent}");
    }
}
