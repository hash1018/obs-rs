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
type DeviceGetUtilizationRates = unsafe extern "C" fn(NvmlDevice, *mut Utilization) -> i32;

/// Whole-adapter utilization, the counterpart of [`ProcessUtilizationSample`].
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Utilization {
    gpu: u32,
    memory: u32,
}

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
    /// Optional because its absence only costs the device-wide fallback, while
    /// the symbols above are what make this sampler exist at all.
    get_utilization_rates: Option<DeviceGetUtilizationRates>,
    devices: Vec<Device>,
    own_pid: u32,
    /// Whether this process has ever turned up in a per-process sample.
    ///
    /// A driver that returns samples is not thereby tracking *us*. GeForce
    /// parts answer a poll now and then with other processes' entries and
    /// never ours, which reads identically to "obs-rs used no GPU" — and
    /// would make the status bar alternate between a real device figure and a
    /// false 0%. Seeing our own pid once is the evidence that the per-process
    /// counter covers this process; until then it does not.
    own_pid_seen: bool,
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
        // SAFETY: same stable ABI and retained-library reasoning as above.
        let get_utilization_rates = unsafe {
            load::<DeviceGetUtilizationRates>(&library, b"nvmlDeviceGetUtilizationRates\0")
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
            get_utilization_rates,
            devices,
            own_pid: std::process::id(),
            own_pid_seen: false,
        })
    }

    /// This process's own utilization, or `None` while nothing shows that
    /// the driver tracks this process at all.
    ///
    /// The distinction is the whole point. An empty result is not "this
    /// process used no GPU": NVIDIA's GeForce parts report `NOT_FOUND` for
    /// nearly every poll, so treating that as zero would pin the reading at
    /// 0% and hide that no per-process counter covers us. Returning `None` is
    /// what lets the caller fall through to a coarser source.
    pub(super) fn sample_process(&mut self) -> Option<f32> {
        let mut busiest = 0;
        for device in &mut self.devices {
            let Some(samples) = query_samples(self.get_process_utilization, device) else {
                continue;
            };
            for sample in samples {
                if sample.pid != self.own_pid {
                    continue;
                }
                self.own_pid_seen = true;
                busiest = busiest.max(sample_utilization(sample));
            }
        }
        // Once this process is known to be tracked, a poll without it is a
        // real zero. Before that, it is only the absence of evidence.
        self.own_pid_seen.then_some(busiest as f32)
    }

    /// Whole-adapter utilization, for drivers with no working per-process
    /// counter. Reports the busiest adapter rather than averaging them.
    pub(super) fn sample_device(&mut self) -> Option<f32> {
        let get_utilization_rates = self.get_utilization_rates?;
        self.devices
            .iter()
            .filter_map(|device| {
                let mut rates = Utilization::default();
                // SAFETY: `device.handle` came from NVML and `rates` is a live
                // output of the ABI-declared type.
                let status = unsafe { get_utilization_rates(device.handle, &mut rates) };
                (status == NVML_SUCCESS && rates.gpu <= 100).then_some(rates.gpu)
            })
            .max()
            .map(|percent| percent as f32)
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
    // The same code the size query treats as "nothing to report" has to mean
    // the same thing here. The size query hands back a buffer capacity rather
    // than a sample count, so a driver with no per-process data reaches this
    // call and answers `NOT_FOUND` — reading that as a device failure is what
    // made the whole sampler give up.
    if status == NVML_ERROR_NOT_FOUND {
        return Some(Vec::new());
    }
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
    fn installed_nvml_reports_usable_percentages() {
        let Some(mut sampler) = NvmlSampler::new() else {
            return;
        };

        // Per-process sampling is genuinely absent on some drivers, so `None`
        // is a valid answer here and asserting `Some` made this test fail or
        // pass depending on what else was using the GPU at the time.
        if let Some(percent) = sampler.sample_process() {
            assert!((0.0..=100.0).contains(&percent), "process {percent}");
        }

        // The device-wide reading is what has to work wherever NVML loaded at
        // all; it is the fallback the status bar depends on.
        let device = sampler
            .sample_device()
            .expect("an initialized NVML sampler should report adapter utilization");
        assert!((0.0..=100.0).contains(&device), "device {device}");
    }
}
