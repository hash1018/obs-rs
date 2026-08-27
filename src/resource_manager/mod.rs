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

#[derive(Debug, Clone, Copy, Default)]
pub struct ResourceUsage {
    pub cpu_percent: Option<f32>,
    pub gpu_percent: Option<f32>,
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
    fn worker_publishes_process_cpu_usage() {
        let manager = ResourceManager::spawn(|| {}).expect("resource manager thread should start");
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if let Some(sample) = manager.latest() {
                assert!(sample.cpu_percent.is_some(), "{sample:?}");
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        panic!("resource manager did not publish within three seconds");
    }
}
