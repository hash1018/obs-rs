//! Current-process resource sampling on macOS, from the kernel's own
//! per-task accounting.

use std::time::Instant;

use objc2_core_foundation::{CFArray, CFDictionary, CFNumber, CFRetained, CFString, CFType};

use super::{GpuScope, GpuUsage, MemoryUsage, ResourceUsage};

pub(super) struct ProcessResourceSampler {
    cpu: TimeSampler,
    gpu: TimeSampler,
}

impl ProcessResourceSampler {
    pub(super) fn new() -> Self {
        Self {
            cpu: TimeSampler::new(cpu_nanoseconds),
            gpu: TimeSampler::new(gpu_nanoseconds),
        }
    }

    pub(super) fn sample(&mut self) -> ResourceUsage {
        let cores = std::thread::available_parallelism().map_or(1, |cores| cores.get());
        ResourceUsage {
            // Of the whole machine, as Linux's is: the process's CPU time over
            // what every core had between two samples.
            cpu_percent: self.cpu.percent(cores),
            // The GPU time this process's own GPU clients were charged, so
            // this is its own share, as on Windows.
            gpu: self.gpu.percent(1).map(|percent| GpuUsage {
                percent,
                scope: GpuScope::Process,
            }),
            memory: memory(),
        }
    }
}

/// A running total of nanoseconds, turned into a share of the wall-clock
/// time between two readings.
struct TimeSampler {
    read: fn() -> Option<u64>,
    previous: Option<(u64, Instant)>,
}

impl TimeSampler {
    fn new(read: fn() -> Option<u64>) -> Self {
        Self {
            read,
            previous: read().map(|total| (total, Instant::now())),
        }
    }

    /// The share of `units` — cores, or the one GPU — the total grew by
    /// since the last reading, in percent.
    fn percent(&mut self, units: usize) -> Option<f32> {
        let now = ((self.read)()?, Instant::now());
        let (total, at) = self.previous.replace(now)?;
        let busy = now.0.checked_sub(total)? as f64;
        let elapsed = now.1.duration_since(at).as_nanos() as f64 * units as f64;
        if elapsed == 0.0 {
            return None;
        }
        Some((busy / elapsed * 100.0).clamp(0.0, 100.0) as f32)
    }
}

/// This process's user and system CPU time.
fn cpu_nanoseconds() -> Option<u64> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    // SAFETY: `usage` is a live, correctly sized out-parameter for this call.
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: `getrusage` returned success, which is its promise to have
    // filled in the structure.
    let usage = unsafe { usage.assume_init() };
    let nanoseconds = |time: libc::timeval| -> Option<u64> {
        Some(
            u64::try_from(time.tv_sec).ok()? * 1_000_000_000
                + u64::try_from(time.tv_usec).ok()? * 1_000,
        )
    };
    Some(nanoseconds(usage.ru_utime)? + nanoseconds(usage.ru_stime)?)
}

/// The GPU time this process's GPU clients have been charged, in
/// nanoseconds — where Activity Monitor reads a process's GPU from.
///
/// Each process that submits GPU work holds a user client of the GPU's
/// driver, a child of its `IOAccelerator` in the I/O Registry, which says
/// whose it is (`IOUserClientCreator`, `pid 1234, obs-rs`) and keeps the GPU
/// time each of its command queues has used (`AppUsage`, each entry's
/// `accumulatedGPUTime`). The kernel's own per-task GPU figure
/// (`TASK_POWER_INFO_V2`) was tried first and reads zero on Apple silicon.
fn gpu_nanoseconds() -> Option<u64> {
    let own = format!("pid {},", std::process::id());
    let mut total = 0u64;
    // SAFETY: every object handle comes from the iterator calls below and is
    // released once, after its last use; the matching dictionary is consumed
    // by `IOServiceGetMatchingServices`, as that call documents; every
    // property is a retained CF object this function owns.
    unsafe {
        let matching = iokit::IOServiceMatching(c"IOAccelerator".as_ptr());
        if matching.is_null() {
            return None;
        }
        let mut accelerators = 0;
        if iokit::IOServiceGetMatchingServices(
            iokit::kIOMainPortDefault,
            matching,
            &mut accelerators,
        ) != libc::KERN_SUCCESS
        {
            return None;
        }
        loop {
            let accelerator = iokit::IOIteratorNext(accelerators);
            if accelerator == 0 {
                break;
            }
            let mut clients = 0;
            if iokit::IORegistryEntryGetChildIterator(
                accelerator,
                c"IOService".as_ptr(),
                &mut clients,
            ) == libc::KERN_SUCCESS
            {
                loop {
                    let client = iokit::IOIteratorNext(clients);
                    if client == 0 {
                        break;
                    }
                    let ours = iokit::property(client, "IOUserClientCreator")
                        .and_then(|creator| creator.downcast::<CFString>().ok())
                        .is_some_and(|creator| creator.to_string().starts_with(&own));
                    if ours {
                        total += client_gpu_time(client);
                    }
                    iokit::IOObjectRelease(client);
                }
                iokit::IOObjectRelease(clients);
            }
            iokit::IOObjectRelease(accelerator);
        }
        iokit::IOObjectRelease(accelerators);
    }
    Some(total)
}

/// What one GPU client's command queues have used, summed.
///
/// # Safety
///
/// `client` must be a live registry entry handle.
unsafe fn client_gpu_time(client: u32) -> u64 {
    // SAFETY: the caller's.
    let Some(usage) = (unsafe { iokit::property(client, "AppUsage") }) else {
        return 0;
    };
    let Ok(usage) = usage.downcast::<CFArray>() else {
        return 0;
    };
    // SAFETY: the driver documents `AppUsage` as an array of dictionaries
    // keyed by strings, which is what this reads it as.
    let usage: CFRetained<CFArray<CFDictionary<CFString, CFType>>> =
        unsafe { CFRetained::cast_unchecked(usage) };
    let key = CFString::from_static_str("accumulatedGPUTime");
    usage
        .iter()
        .filter_map(|queue| queue.get(&key)?.downcast::<CFNumber>().ok()?.as_i64())
        .filter_map(|time| u64::try_from(time).ok())
        .sum()
}

/// The few I/O Kit calls the GPU figure needs.
mod iokit {
    use std::ffi::{c_char, c_void};
    use std::ptr::NonNull;

    use objc2_core_foundation::{CFRetained, CFString, CFType};

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        pub(super) static kIOMainPortDefault: libc::mach_port_t;
        pub(super) fn IOServiceMatching(name: *const c_char) -> *mut c_void;
        pub(super) fn IOServiceGetMatchingServices(
            main_port: libc::mach_port_t,
            matching: *mut c_void,
            existing: *mut u32,
        ) -> libc::kern_return_t;
        pub(super) fn IOIteratorNext(iterator: u32) -> u32;
        pub(super) fn IORegistryEntryGetChildIterator(
            entry: u32,
            plane: *const c_char,
            iterator: *mut u32,
        ) -> libc::kern_return_t;
        fn IORegistryEntryCreateCFProperty(
            entry: u32,
            key: &CFString,
            allocator: *const c_void,
            options: u32,
        ) -> *mut CFType;
        pub(super) fn IOObjectRelease(object: u32) -> libc::kern_return_t;
    }

    /// `name`'s value on a registry entry, or `None` where it has none.
    ///
    /// # Safety
    ///
    /// `entry` must be a live registry entry handle.
    pub(super) unsafe fn property(entry: u32, name: &str) -> Option<CFRetained<CFType>> {
        let key = CFString::from_str(name);
        // SAFETY: the caller's handle, a live key, the default allocator;
        // what comes back is a new reference this takes ownership of.
        let value = unsafe { IORegistryEntryCreateCFProperty(entry, &key, std::ptr::null(), 0) };
        // SAFETY: as above — created, so owned.
        NonNull::new(value).map(|value| unsafe { CFRetained::from_raw(value) })
    }
}

/// This process's physical footprint — what Activity Monitor calls its
/// memory: the pages it has dirtied and not shared, compressed ones
/// included. Nothing here is the claimed-but-not-resident figure Windows and
/// Linux give beside it, so that one is left out.
fn memory() -> Option<MemoryUsage> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage_info_v2>::zeroed();
    // SAFETY: `usage` is a writable `rusage_info_v2`, the structure the
    // `RUSAGE_INFO_V2` flavour fills, for this live process.
    let status = unsafe {
        libc::proc_pid_rusage(
            libc::getpid(),
            libc::RUSAGE_INFO_V2,
            usage.as_mut_ptr().cast(),
        )
    };
    if status != 0 {
        return None;
    }
    // SAFETY: filled by the call above, which succeeded.
    let usage = unsafe { usage.assume_init() };
    Some(MemoryUsage {
        resident_bytes: usage.ri_phys_footprint,
        committed_bytes: None,
    })
}
