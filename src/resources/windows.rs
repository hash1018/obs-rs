use std::{mem::MaybeUninit, time::Instant};

use windows::{
    Win32::{
        Foundation::FILETIME,
        System::{
            Performance::{
                PDH_CSTATUS_NEW_DATA, PDH_CSTATUS_VALID_DATA, PDH_FMT_COUNTERVALUE_ITEM_W,
                PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY, PDH_MORE_DATA, PdhAddEnglishCounterW,
                PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW, PdhOpenQueryW,
            },
            ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX},
            Threading::{GetCurrentProcess, GetCurrentProcessId, GetProcessTimes},
        },
    },
    core::w,
};

use super::{GpuScope, GpuUsage, ResourceUsage};

pub struct ProcessResourceSampler {
    cpu: CpuSampler,
    gpu: Option<GpuSampler>,
}

impl ProcessResourceSampler {
    pub fn new() -> Self {
        Self {
            cpu: CpuSampler::new(),
            gpu: GpuSampler::new(),
        }
    }

    pub fn sample(&mut self) -> ResourceUsage {
        ResourceUsage {
            cpu_percent: self.cpu.sample(),
            // PDH's GPU engine counters are already scoped to this
            // process, so Windows never needs the device-wide fallback.
            gpu: self
                .gpu
                .as_mut()
                .and_then(GpuSampler::sample)
                .map(|percent| GpuUsage {
                    percent,
                    scope: GpuScope::Process,
                }),
            memory_bytes: private_bytes(),
        }
    }
}

/// This process's private commit, which is what Task Manager calls its
/// memory and what a leak shows up in.
///
/// `PROCESS_MEMORY_COUNTERS_EX` is the wider struct `GetProcessMemoryInfo`
/// fills when told its size; `PrivateUsage` is the field the narrow one does
/// not have, so the size is what asks for it.
fn private_bytes() -> Option<u64> {
    let mut counters = MaybeUninit::<PROCESS_MEMORY_COUNTERS_EX>::zeroed();
    let size = u32::try_from(size_of::<PROCESS_MEMORY_COUNTERS_EX>()).ok()?;
    // SAFETY: the struct is zeroed and live, its size is what is being
    // declared, and the pointer cast is the one this API documents for the
    // extended form.
    unsafe {
        GetProcessMemoryInfo(GetCurrentProcess(), counters.as_mut_ptr().cast(), size).ok()?;
        Some(counters.assume_init().PrivateUsage as u64)
    }
}

struct CpuSampler {
    previous_process_ticks: Option<u64>,
    previous_wall_time: Instant,
    logical_processors: f64,
}

impl CpuSampler {
    fn new() -> Self {
        Self {
            previous_process_ticks: process_ticks(),
            previous_wall_time: Instant::now(),
            logical_processors: std::thread::available_parallelism()
                .map_or(1.0, |count| count.get() as f64),
        }
    }

    fn sample(&mut self) -> Option<f32> {
        let now = Instant::now();
        let current_ticks = process_ticks()?;
        let previous_ticks = self.previous_process_ticks.replace(current_ticks)?;
        let elapsed = now.duration_since(self.previous_wall_time).as_secs_f64();
        self.previous_wall_time = now;
        if elapsed <= 0.0 || current_ticks < previous_ticks {
            return None;
        }

        let process_seconds = (current_ticks - previous_ticks) as f64 / 10_000_000.0;
        Some((process_seconds / elapsed / self.logical_processors * 100.0).clamp(0.0, 100.0) as f32)
    }
}

fn process_ticks() -> Option<u64> {
    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: the pseudo-handle is valid for the current process and every
    // output points to a live FILETIME local.
    unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &mut creation,
            &mut exit,
            &mut kernel,
            &mut user,
        )
        .ok()?;
    }
    Some(filetime_ticks(kernel) + filetime_ticks(user))
}

fn filetime_ticks(time: FILETIME) -> u64 {
    ((time.dwHighDateTime as u64) << 32) | time.dwLowDateTime as u64
}

struct GpuSampler {
    query: PDH_HQUERY,
    counter: PDH_HCOUNTER,
    pid_marker: String,
}

impl GpuSampler {
    fn new() -> Option<Self> {
        let mut query = PDH_HQUERY::default();
        // SAFETY: the out-parameter points to a live handle local.
        if unsafe { PdhOpenQueryW(None, 0, &mut query) } != 0 {
            return None;
        }

        let mut counter = PDH_HCOUNTER::default();
        // The English form is locale-independent even on non-English Windows.
        let status = unsafe {
            PdhAddEnglishCounterW(
                query,
                w!(r"\GPU Engine(*)\Utilization Percentage"),
                0,
                &mut counter,
            )
        };
        if status != 0 {
            // SAFETY: query was successfully opened above and is not reused.
            unsafe { PdhCloseQuery(query) };
            return None;
        }

        // Prime counters that need a previous sample before they can format.
        unsafe { PdhCollectQueryData(query) };
        Some(Self {
            query,
            counter,
            pid_marker: format!("pid_{}_", unsafe { GetCurrentProcessId() }),
        })
    }

    fn sample(&mut self) -> Option<f32> {
        // SAFETY: both handles remain owned by this sampler for its lifetime.
        if unsafe { PdhCollectQueryData(self.query) } != 0 {
            return None;
        }

        let mut buffer_size = 0;
        let mut item_count = 0;
        let status = unsafe {
            PdhGetFormattedCounterArrayW(
                self.counter,
                PDH_FMT_DOUBLE,
                &mut buffer_size,
                &mut item_count,
                None,
            )
        };
        if status != PDH_MORE_DATA || buffer_size == 0 || item_count == 0 {
            return None;
        }

        // A usize-backed buffer gives the returned struct array pointer its
        // required alignment; PDH also stores the referenced UTF-16 names in
        // the same allocation after that array.
        let word_count = (buffer_size as usize).div_ceil(size_of::<usize>());
        let mut buffer = vec![MaybeUninit::<usize>::uninit(); word_count];
        let items = buffer.as_mut_ptr().cast::<PDH_FMT_COUNTERVALUE_ITEM_W>();
        let status = unsafe {
            PdhGetFormattedCounterArrayW(
                self.counter,
                PDH_FMT_DOUBLE,
                &mut buffer_size,
                &mut item_count,
                Some(items),
            )
        };
        if status != 0 {
            return None;
        }

        let mut busiest_engine = None::<f64>;
        // SAFETY: PDH wrote item_count initialized entries at the start of the
        // aligned buffer, and all name pointers remain live until it is dropped.
        for item in unsafe { std::slice::from_raw_parts(items, item_count as usize) } {
            if item.FmtValue.CStatus != PDH_CSTATUS_VALID_DATA
                && item.FmtValue.CStatus != PDH_CSTATUS_NEW_DATA
            {
                continue;
            }
            let Ok(name) = (unsafe { item.szName.to_string() }) else {
                continue;
            };
            if !name.to_ascii_lowercase().contains(&self.pid_marker) {
                continue;
            }
            let value = unsafe { item.FmtValue.Anonymous.doubleValue };
            if value.is_finite() {
                busiest_engine = Some(busiest_engine.map_or(value, |current| current.max(value)));
            }
        }

        busiest_engine.map(|value| value.clamp(0.0, 100.0) as f32)
    }
}

impl Drop for GpuSampler {
    fn drop(&mut self) {
        // SAFETY: this sampler uniquely owns the successfully opened query.
        unsafe { PdhCloseQuery(self.query) };
    }
}
