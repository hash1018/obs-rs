//! Current-process resource sampling from Linux procfs and DRM fdinfo.

mod nvml;

use std::{
    collections::HashMap,
    fs,
    time::{Duration, Instant},
};

use super::{GpuScope, GpuUsage, MemoryUsage, ResourceUsage};

pub(super) struct ProcessResourceSampler {
    cpu: CpuSampler,
    gpu: GpuSampler,
}

impl ProcessResourceSampler {
    pub(super) fn new() -> Self {
        Self {
            cpu: CpuSampler::new(),
            gpu: GpuSampler::new(),
        }
    }

    pub(super) fn sample(&mut self) -> ResourceUsage {
        ResourceUsage {
            cpu_percent: self.cpu.sample(),
            gpu: self.gpu.sample(),
            memory: memory(),
        }
    }
}

/// The two figures, from the two lines of `/proc/self/status` that mean
/// what Windows' private working set and private commit mean.
///
/// `RssAnon` is this process's own resident pages, where `statm`'s resident
/// field would also count the file-backed ones every process maps; `VmData`
/// is its private writable address space, claimed whether or not it is in
/// RAM. Neither is the same measure as its Windows counterpart, but each is
/// the nearest this system keeps, and the pair says the same thing: what is
/// in memory now, and what has been asked for.
fn memory() -> Option<MemoryUsage> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    let field = |name: &str| {
        status.lines().find_map(|line| {
            let rest = line.strip_prefix(name)?.strip_suffix(" kB")?;
            // Every size here is in kibibytes, whatever the label's spacing.
            rest.trim().parse::<u64>().ok().map(|value| value * 1024)
        })
    };
    Some(MemoryUsage {
        resident_bytes: field("RssAnon:")?,
        committed_bytes: field("VmData:"),
    })
}

#[derive(Clone, Copy)]
struct CpuTimes {
    process: u64,
    system: u64,
}

struct CpuSampler {
    previous: Option<CpuTimes>,
}

impl CpuSampler {
    fn new() -> Self {
        Self {
            previous: read_cpu_times(),
        }
    }

    fn sample(&mut self) -> Option<f32> {
        let current = read_cpu_times()?;
        let previous = self.previous.replace(current)?;
        cpu_percent(previous, current)
    }
}

fn read_cpu_times() -> Option<CpuTimes> {
    let process_stat = fs::read_to_string("/proc/self/stat").ok()?;
    let system_stat = fs::read_to_string("/proc/stat").ok()?;
    Some(CpuTimes {
        process: parse_process_ticks(&process_stat)?,
        system: parse_system_ticks(&system_stat)?,
    })
}

fn parse_process_ticks(stat: &str) -> Option<u64> {
    // `comm` is enclosed in parentheses and may itself contain spaces or `)`.
    // Searching from the right is the only safe way to reach fields 14/15.
    let fields = stat.get(stat.rfind(") ")? + 2..)?;
    let mut fields = fields.split_whitespace();
    let user = fields.nth(11)?.parse::<u64>().ok()?;
    let system = fields.next()?.parse::<u64>().ok()?;
    user.checked_add(system)
}

fn parse_system_ticks(stat: &str) -> Option<u64> {
    let line = stat.lines().find(|line| line.starts_with("cpu "))?;
    // The first eight fields are user through steal. `guest` and `guest_nice`
    // are already included in user/nice and must not be counted twice.
    line.split_whitespace()
        .skip(1)
        .take(8)
        .try_fold(0u64, |total, value| {
            total.checked_add(value.parse::<u64>().ok()?)
        })
}

fn cpu_percent(previous: CpuTimes, current: CpuTimes) -> Option<f32> {
    let process_delta = current.process.checked_sub(previous.process)?;
    let system_delta = current.system.checked_sub(previous.system)?;
    if system_delta == 0 {
        return None;
    }
    Some((process_delta as f64 / system_delta as f64 * 100.0).clamp(0.0, 100.0) as f32)
}

type GpuCounters = HashMap<(String, String), u64>;

struct GpuSampler {
    nvml: Option<nvml::NvmlSampler>,
    drm: DrmSampler,
}

impl GpuSampler {
    fn new() -> Self {
        Self {
            nvml: nvml::NvmlSampler::new(),
            drm: DrmSampler::new(),
        }
    }

    /// Prefers a per-process reading and only widens to the whole adapter
    /// when neither per-process source answers.
    ///
    /// Both per-process sources are unavailable together on NVIDIA's Linux
    /// driver: it exposes no `drm-engine-*` fdinfo, and per-process NVML
    /// samples are absent on GeForce parts. Without the last tier the status
    /// bar simply reads `--` on those machines.
    fn sample(&mut self) -> Option<GpuUsage> {
        let process = self
            .nvml
            .as_mut()
            .and_then(nvml::NvmlSampler::sample_process)
            .or_else(|| self.drm.sample());
        if let Some(percent) = process {
            return Some(GpuUsage {
                percent,
                scope: GpuScope::Process,
            });
        }
        self.nvml
            .as_mut()
            .and_then(nvml::NvmlSampler::sample_device)
            .map(|percent| GpuUsage {
                percent,
                scope: GpuScope::Device,
            })
    }
}

struct DrmSampler {
    previous: Option<GpuCounters>,
    previous_time: Instant,
}

impl DrmSampler {
    fn new() -> Self {
        Self {
            previous: read_gpu_counters(),
            previous_time: Instant::now(),
        }
    }

    fn sample(&mut self) -> Option<f32> {
        let now = Instant::now();
        let current = read_gpu_counters()?;
        let elapsed = now.duration_since(self.previous_time);
        self.previous_time = now;
        let previous = self.previous.replace(current.clone())?;
        gpu_percent(&previous, &current, elapsed)
    }
}

fn read_gpu_counters() -> Option<GpuCounters> {
    // A process can have duplicate file descriptors for the same DRM client.
    // Keep the greatest counter for each client/engine first, then combine
    // all of this process's clients per physical device and engine.
    let mut clients = HashMap::<(String, String, String), u64>::new();
    let entries = fs::read_dir("/proc/self/fdinfo").ok()?;
    for entry in entries.flatten() {
        let Ok(contents) = fs::read_to_string(entry.path()) else {
            continue;
        };
        let Some(record) = parse_drm_fdinfo(&contents) else {
            continue;
        };
        for (engine, nanoseconds) in record.engines {
            clients
                .entry((record.device.clone(), record.client.clone(), engine))
                .and_modify(|current| *current = (*current).max(nanoseconds))
                .or_insert(nanoseconds);
        }
    }
    if clients.is_empty() {
        return None;
    }

    let mut engines = GpuCounters::new();
    for ((device, _client, engine), nanoseconds) in clients {
        *engines.entry((device, engine)).or_default() += nanoseconds;
    }
    Some(engines)
}

struct DrmFdinfo {
    device: String,
    client: String,
    engines: Vec<(String, u64)>,
}

fn parse_drm_fdinfo(contents: &str) -> Option<DrmFdinfo> {
    let mut device = None;
    let mut client = None;
    let mut engines = Vec::new();

    for line in contents.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key {
            "drm-pdev" => device = Some(value.to_owned()),
            "drm-client-id" => client = Some(value.to_owned()),
            key if key.starts_with("drm-engine-") => {
                let nanoseconds = value
                    .strip_suffix(" ns")
                    .unwrap_or(value)
                    .trim()
                    .parse::<u64>()
                    .ok()?;
                engines.push((key["drm-engine-".len()..].to_owned(), nanoseconds));
            }
            _ => {}
        }
    }

    (!engines.is_empty()).then(|| DrmFdinfo {
        device: device.unwrap_or_else(|| "unknown-device".to_owned()),
        client: client.unwrap_or_else(|| "unknown-client".to_owned()),
        engines,
    })
}

fn gpu_percent(previous: &GpuCounters, current: &GpuCounters, elapsed: Duration) -> Option<f32> {
    let elapsed_nanoseconds = elapsed.as_nanos();
    if elapsed_nanoseconds == 0 {
        return None;
    }

    // Engines run independently. As on the Windows sampler, report the
    // busiest one instead of adding render/copy/video percentages together.
    current
        .iter()
        .filter_map(|(engine, current)| {
            let previous = previous.get(engine)?;
            let delta = current.checked_sub(*previous)?;
            Some(delta as f64 / elapsed_nanoseconds as f64 * 100.0)
        })
        .reduce(f64::max)
        .map(|percent| percent.clamp(0.0, 100.0) as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_stat_parser_handles_parentheses_in_the_command() {
        let stat = "42 (obs ) capture) R 1 2 3 4 5 6 7 8 9 10 120 30 0";
        assert_eq!(parse_process_ticks(stat), Some(150));
    }

    #[test]
    fn system_stat_does_not_double_count_guest_time() {
        let stat = "cpu  1 2 3 4 5 6 7 8 90 100\ncpu0 1 2 3 4 5 6 7 8 90 100\n";
        assert_eq!(parse_system_ticks(stat), Some(36));
    }

    #[test]
    fn cpu_usage_is_normalized_to_total_machine_capacity() {
        let previous = CpuTimes {
            process: 100,
            system: 1_000,
        };
        let current = CpuTimes {
            process: 125,
            system: 1_100,
        };
        assert_eq!(cpu_percent(previous, current), Some(25.0));
    }

    #[test]
    fn drm_fdinfo_parser_reads_engine_nanoseconds() {
        let fdinfo = "pos:\t0\ndrm-driver:\ti915\ndrm-client-id:\t7\ndrm-pdev:\t0000:00:02.0\ndrm-engine-render:\t1234 ns\ndrm-engine-copy:\t50 ns\n";
        let parsed = parse_drm_fdinfo(fdinfo).expect("DRM engines should be found");
        assert_eq!(parsed.device, "0000:00:02.0");
        assert_eq!(parsed.client, "7");
        assert_eq!(
            parsed.engines,
            vec![("render".to_owned(), 1234), ("copy".to_owned(), 50)]
        );
    }

    #[test]
    fn gpu_usage_reports_the_busiest_engine() {
        let previous = GpuCounters::from([
            (("gpu".to_owned(), "render".to_owned()), 100),
            (("gpu".to_owned(), "copy".to_owned()), 100),
        ]);
        let current = GpuCounters::from([
            (("gpu".to_owned(), "render".to_owned()), 600_000_100),
            (("gpu".to_owned(), "copy".to_owned()), 200_000_100),
        ]);
        assert_eq!(
            gpu_percent(&previous, &current, Duration::from_secs(1)),
            Some(60.0)
        );
    }
}
