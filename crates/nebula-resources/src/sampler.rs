use std::collections::HashMap;
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use nebula_config::{ResourcesConfig, VolumeGuard};
use nebula_proto::{GpuProcess, ResourceSnapshot};
use time::OffsetDateTime;
use tokio::sync::watch;

use crate::sources::{GpuProcessSource, GpuSource, SysinfoSystem, SystemSource};
use crate::{GuardLevel, ResourceError, guard_disks};

/// How many GPU processes a snapshot keeps.
pub const TOP_GPU_PROCESSES: usize = 10;

/// Where per-process VRAM came from in a snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessSource {
    /// NVML reported amounts.
    Nvml,
    /// The PDH `GPU Process Memory` counters.
    Pdh,
    /// Neither worked.
    None,
}

impl std::fmt::Display for ProcessSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Nvml => "nvml",
            Self::Pdh => "pdh",
            Self::None => "none",
        })
    }
}

/// The readers a snapshot is built from.
pub struct Sources {
    /// Whole-GPU readings; `None` without a GPU driver.
    pub gpu: Option<Box<dyn GpuSource>>,
    /// Per-process VRAM fallback.
    pub gpu_processes: Option<Box<dyn GpuProcessSource>>,
    /// Memory, CPU, disks and process names.
    pub system: Box<dyn SystemSource>,
}

impl Sources {
    /// NVML, PDH and `sysinfo`, skipping (and logging) any that can't start.
    #[must_use]
    pub fn real() -> Self {
        let gpu = match crate::sources::NvmlGpu::new() {
            Ok(g) => Some(Box::new(g) as Box<dyn GpuSource>),
            Err(e) => {
                tracing::warn!(event = "resources.source_unavailable", source = "nvml", error = %e);
                None
            }
        };
        #[cfg(windows)]
        let gpu_processes = match crate::sources::PdhGpuProcesses::new() {
            Ok(p) => Some(Box::new(p) as Box<dyn GpuProcessSource>),
            Err(e) => {
                tracing::warn!(event = "resources.source_unavailable", source = "pdh", error = %e);
                None
            }
        };
        #[cfg(not(windows))]
        let gpu_processes = None;
        Self {
            gpu,
            gpu_processes,
            system: Box::new(SysinfoSystem::new()),
        }
    }

    /// Takes one snapshot of `mounts`. A failing GPU reads as zeros; a failing system source
    /// is an error.
    ///
    /// # Errors
    /// [`ResourceError::Unavailable`] from the system source.
    pub fn snapshot(
        &mut self,
        mounts: &[String],
    ) -> Result<(ResourceSnapshot, ProcessSource), ResourceError> {
        let sys = self.system.read(mounts)?;
        let gpu = self
            .gpu
            .as_mut()
            .and_then(|g| g.read().ok())
            .unwrap_or_default();

        let nvml: Vec<(u32, u64)> = gpu
            .processes
            .iter()
            .filter_map(|(pid, mib)| mib.map(|m| (*pid, m)))
            .collect();
        let (mut procs, source) = if !nvml.is_empty() {
            (nvml, ProcessSource::Nvml)
        } else if let Some(p) = self.gpu_processes.as_mut().and_then(|s| s.read().ok()) {
            (p, ProcessSource::Pdh)
        } else {
            (Vec::new(), ProcessSource::None)
        };
        procs.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        procs.truncate(TOP_GPU_PROCESSES);
        let pids: Vec<u32> = procs.iter().map(|(p, _)| *p).collect();
        let names = self.system.process_names(&pids);

        Ok((
            ResourceSnapshot {
                taken_at: OffsetDateTime::now_utc(),
                vram_used_mib: gpu.vram_used_mib,
                vram_total_mib: gpu.vram_total_mib,
                gpu_util_pct: gpu.util_pct,
                ram_used_mib: sys.ram_used_mib,
                ram_total_mib: sys.ram_total_mib,
                commit_used_mib: sys.commit_used_mib,
                commit_limit_mib: sys.commit_limit_mib,
                cpu_pct: sys.cpu_pct,
                disks: sys.disks,
                gpu_processes: procs
                    .into_iter()
                    .map(|(pid, vram_mib)| GpuProcess {
                        pid,
                        name: names.get(&pid).cloned().unwrap_or_default(),
                        vram_mib,
                    })
                    .collect(),
            },
            source,
        ))
    }
}

/// Sampler timing and what to watch.
#[derive(Clone, Debug)]
pub struct SamplerConfig {
    /// Time between snapshots.
    pub interval: Duration,
    /// Time between logged `resources.sample` events.
    pub log_every: Duration,
    /// Guarded volumes; their mounts are the disks sampled.
    pub volumes: Vec<VolumeGuard>,
}

impl SamplerConfig {
    /// From the `[resources]` section.
    #[must_use]
    pub fn from_config(cfg: &ResourcesConfig) -> Self {
        Self {
            interval: Duration::from_millis(cfg.sample_interval_ms.max(100)),
            log_every: Duration::from_secs(cfg.log_interval_s),
            volumes: cfg.volumes.clone(),
        }
    }
}

/// Background snapshots on a dedicated thread (NVML and PDH calls block). The latest is
/// published on a watch channel. Dropping the sampler stops the thread.
pub struct Sampler {
    rx: watch::Receiver<Option<ResourceSnapshot>>,
    stop: Option<mpsc::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Sampler {
    /// Starts sampling. The first snapshot is taken immediately.
    ///
    /// # Errors
    /// [`ResourceError::Thread`] if the thread can't be spawned.
    pub fn start(sources: Sources, cfg: SamplerConfig) -> Result<Self, ResourceError> {
        let (tx, rx) = watch::channel(None);
        let (stop_tx, stop_rx) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("nebula-sampler".into())
            .spawn(move || run(sources, &cfg, &tx, &stop_rx))?;
        Ok(Self {
            rx,
            stop: Some(stop_tx),
            thread: Some(thread),
        })
    }

    /// The latest snapshot, if one has been taken.
    #[must_use]
    pub fn latest(&self) -> Option<ResourceSnapshot> {
        self.rx.borrow().clone()
    }

    /// A receiver that sees each new snapshot.
    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<Option<ResourceSnapshot>> {
        self.rx.clone()
    }
}

impl Drop for Sampler {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn run(
    mut sources: Sources,
    cfg: &SamplerConfig,
    tx: &watch::Sender<Option<ResourceSnapshot>>,
    stop: &mpsc::Receiver<()>,
) {
    let mounts: Vec<String> = cfg.volumes.iter().map(|v| v.mount.clone()).collect();
    let mut last_log: Option<Instant> = None;
    let mut levels: HashMap<String, GuardLevel> = HashMap::new();
    let mut failing = false;
    loop {
        match sources.snapshot(&mounts) {
            Ok((snap, source)) => {
                if failing {
                    tracing::info!(event = "resources.sample_recovered");
                    failing = false;
                }
                report_guard_changes(&snap, &cfg.volumes, &mut levels);
                if last_log.is_none_or(|t| t.elapsed() >= cfg.log_every) {
                    log_sample(&snap, source);
                    last_log = Some(Instant::now());
                }
                tx.send_replace(Some(snap));
            }
            Err(e) => {
                if !failing {
                    tracing::warn!(event = "resources.sample_failed", error = %e);
                    failing = true;
                }
            }
        }
        match stop.recv_timeout(cfg.interval) {
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn log_sample(s: &ResourceSnapshot, source: ProcessSource) {
    let disks: Vec<String> = s
        .disks
        .iter()
        .map(|d| format!("{} {} GB free", d.mount, d.free_bytes / crate::GB))
        .collect();
    let top: Vec<String> = s
        .gpu_processes
        .iter()
        .take(3)
        .map(|p| format!("{}({}) {} MiB", p.name, p.pid, p.vram_mib))
        .collect();
    tracing::info!(
        event = "resources.sample",
        vram_used_mib = s.vram_used_mib,
        vram_total_mib = s.vram_total_mib,
        gpu_util_pct = s.gpu_util_pct.map(u64::from),
        ram_used_mib = s.ram_used_mib,
        commit_used_mib = s.commit_used_mib,
        commit_limit_mib = s.commit_limit_mib,
        cpu_pct = f64::from(s.cpu_pct),
        disks = %disks.join(", "),
        gpu_top = %top.join(", "),
        gpu_process_source = %source,
    );
}

fn report_guard_changes(
    s: &ResourceSnapshot,
    volumes: &[VolumeGuard],
    levels: &mut HashMap<String, GuardLevel>,
) {
    for (disk, _, level) in guard_disks(&s.disks, volumes) {
        let prev = levels.insert(disk.mount.clone(), level);
        if prev.is_some_and(|p| p == level) || (prev.is_none() && level == GuardLevel::Ok) {
            continue;
        }
        let free_gb = disk.free_bytes / crate::GB;
        if level == GuardLevel::Ok {
            tracing::info!(event = "resources.disk_guard", mount = %disk.mount, level = %level, free_gb);
        } else {
            tracing::warn!(event = "resources.disk_guard", mount = %disk.mount, level = %level, free_gb);
        }
    }
}
