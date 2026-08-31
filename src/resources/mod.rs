//! Low-frequency process resource monitoring kept off the UI thread.

use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "windows")]
mod windows;

const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);

/// What a GPU reading actually measures.
///
/// Not cosmetic: a per-process figure and a whole-device one answer different
/// questions, and no platform offers both from one source. Reporting a device
/// figure as if it were this process's would overstate obs-rs's cost by
/// whatever else is drawing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuScope {
    /// This process's own share, from per-process engine counters.
    Process,
    /// Every process on the adapter. Used only where no per-process counter
    /// exists — NVIDIA's Linux driver exposes neither `drm-engine-*` fdinfo
    /// nor working per-process NVML samples on GeForce parts.
    ///
    /// Only the Linux backend reports it, so on every other platform nothing
    /// constructs this variant — it is still read there, since the status bar
    /// marks a device figure differently from a process one.
    #[allow(dead_code)]
    Device,
}

#[derive(Debug, Clone, Copy)]
pub struct GpuUsage {
    pub percent: f32,
    pub scope: GpuScope,
}

/// Two answers to "how much memory", because they differ by a factor of
/// three here and people check the wrong one against the other.
///
/// Private in both cases: shared pages are the DLLs every process on the
/// machine maps, and counting them would report an application's cost as
/// whatever the system loaded into it.
///
/// The GPU is in neither, mostly. Measured on this machine: removing two
/// 1080p Display Captures freed 18 MB of dedicated video memory and moved
/// the private figures by one megabyte, so what a graphics driver holds in
/// VRAM is not charged here — only whatever system memory it uses alongside
/// it.
#[derive(Debug, Clone, Copy)]
pub struct MemoryUsage {
    /// What is in RAM for this process alone — Windows' private working set,
    /// Linux's `RssAnon`. The number a task manager shows, and the reason
    /// this is the one on the bar.
    ///
    /// It moves for reasons that are not this application's doing: an
    /// operating system trims a working set when it wants the pages back,
    /// so minimising the window can halve it without a byte being freed.
    pub resident_bytes: u64,
    /// What the process has claimed, whether or not it is in RAM — Windows'
    /// private commit, Linux's `VmData`. Steady where the resident figure is
    /// not, which is what makes it the one to watch a leak in, and it is a
    /// hover away for exactly that.
    pub committed_bytes: Option<u64>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ResourceUsage {
    pub cpu_percent: Option<f32>,
    pub gpu: Option<GpuUsage>,
    pub memory: Option<MemoryUsage>,
}

pub struct ResourceManager {
    samples: Receiver<ResourceUsage>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl ResourceManager {
    pub fn spawn(wake_ui: impl Fn() + Send + 'static) -> io::Result<Self> {
        let (sender, samples) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker = thread::Builder::new()
            .name("resource-manager".to_owned())
            .spawn(move || {
                #[cfg(target_os = "windows")]
                let mut sampler = windows::ProcessResourceSampler::new();
                #[cfg(target_os = "linux")]
                let mut sampler = linux::ProcessResourceSampler::new();

                while !worker_stop.load(Ordering::Acquire) {
                    thread::park_timeout(SAMPLE_INTERVAL);
                    if worker_stop.load(Ordering::Acquire) {
                        break;
                    }

                    #[cfg(target_os = "windows")]
                    let usage = sampler.sample();
                    #[cfg(target_os = "linux")]
                    let usage = sampler.sample();
                    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
                    let usage = ResourceUsage::default();

                    if sender.send(usage).is_err() {
                        break;
                    }
                    wake_ui();
                }
            })?;

        Ok(Self {
            samples,
            stop,
            worker: Some(worker),
        })
    }

    /// Drains pending samples so a slow UI always receives the newest value.
    pub fn latest(&self) -> Option<ResourceUsage> {
        self.samples.try_iter().last()
    }
}

impl Drop for ResourceManager {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.thread().unpark();
            let _ = worker.join();
        }
    }
}

#[cfg(all(test, any(target_os = "windows", target_os = "linux")))]
mod tests {
    use std::time::Instant;

    use super::*;

    #[test]
    fn worker_publishes_process_cpu_and_memory_usage() {
        let manager = ResourceManager::spawn(|| {}).expect("resource manager thread should start");
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if let Some(sample) = manager.latest() {
                assert!(sample.cpu_percent.is_some(), "{sample:?}");
                // A running process has memory, so unlike the GPU there is
                // no machine where this is legitimately absent.
                let memory = sample.memory.expect("a process has memory");
                assert!(
                    memory.resident_bytes > 1024 * 1024,
                    "a process running a test suite has more than a megabyte in memory: {memory:?}"
                );
                // The two are different measures of the same process, so one
                // being wildly under the other is a sign of reading the wrong
                // field rather than of a frugal process.
                if let Some(committed) = memory.committed_bytes {
                    assert!(
                        committed >= memory.resident_bytes,
                        "claimed memory cannot be less than what is resident: {memory:?}"
                    );
                }
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        panic!("resource manager did not publish within three seconds");
    }
}
