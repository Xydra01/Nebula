//! Where readings come from. Each source is a trait so the sampler and checks can be tested
//! with fakes; the real implementations are NVML, PDH and `sysinfo`.

use std::collections::HashMap;
use std::sync::Mutex;

use nebula_proto::DiskUsage;

use crate::ResourceError;

const MIB: u64 = 1024 * 1024;

/// One GPU reading.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GpuReading {
    /// Device name.
    pub name: String,
    /// Driver version.
    pub driver: String,
    /// VRAM in use by all processes, MiB.
    pub vram_used_mib: u64,
    /// Total VRAM, MiB.
    pub vram_total_mib: u64,
    /// Utilization, percent.
    pub util_pct: Option<u8>,
    /// Processes on the GPU with their memory, when the driver reports it. Under WDDM the
    /// amount is usually `None`.
    pub processes: Vec<(u32, Option<u64>)>,
}

/// Whole-GPU readings.
pub trait GpuSource: Send {
    /// Reads the first GPU.
    ///
    /// # Errors
    /// [`ResourceError::Unavailable`] if there is no usable GPU or driver.
    fn read(&mut self) -> Result<GpuReading, ResourceError>;
}

/// Per-process dedicated VRAM, MiB, by process id.
pub trait GpuProcessSource: Send {
    /// Reads per-process dedicated GPU memory.
    ///
    /// # Errors
    /// [`ResourceError::Unavailable`] if the counters can't be read.
    fn read(&mut self) -> Result<Vec<(u32, u64)>, ResourceError>;
}

/// Memory, CPU and disk readings.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SystemReading {
    /// Physical RAM in use, MiB.
    pub ram_used_mib: u64,
    /// Physical RAM, MiB.
    pub ram_total_mib: u64,
    /// Commit charge, MiB.
    pub commit_used_mib: u64,
    /// Commit limit including page-file growth ([`Commit::effective_limit_mib`]), MiB.
    pub commit_limit_mib: u64,
    /// CPU utilization, percent.
    pub cpu_pct: f32,
    /// Requested volumes that exist.
    pub disks: Vec<DiskUsage>,
}

/// System-wide readings and process names.
pub trait SystemSource: Send {
    /// Reads memory, CPU, commit charge and the given volumes (e.g. `F:\`).
    ///
    /// # Errors
    /// [`ResourceError::Unavailable`] if the OS refuses.
    fn read(&mut self, mounts: &[String]) -> Result<SystemReading, ResourceError>;
    /// Executable names for process ids (missing ids are left out).
    fn process_names(&mut self, pids: &[u32]) -> HashMap<u32, String>;
}

/// NVML (the NVIDIA driver's management library).
pub struct NvmlGpu {
    nvml: nvml_wrapper::Nvml,
}

impl NvmlGpu {
    /// Loads NVML.
    ///
    /// # Errors
    /// [`ResourceError::Unavailable`] without an NVIDIA driver.
    pub fn new() -> Result<Self, ResourceError> {
        nvml_wrapper::Nvml::init()
            .map(|nvml| Self { nvml })
            .map_err(|e| ResourceError::Unavailable(format!("NVML: {e}")))
    }
}

impl GpuSource for NvmlGpu {
    fn read(&mut self) -> Result<GpuReading, ResourceError> {
        use nvml_wrapper::enums::device::UsedGpuMemory;
        let err =
            |e: nvml_wrapper::error::NvmlError| ResourceError::Unavailable(format!("NVML: {e}"));
        let device = self.nvml.device_by_index(0).map_err(err)?;
        let mem = device.memory_info().map_err(err)?;
        let mut processes: HashMap<u32, Option<u64>> = HashMap::new();
        for p in device
            .running_compute_processes()
            .unwrap_or_default()
            .into_iter()
            .chain(device.running_graphics_processes().unwrap_or_default())
        {
            let used = match p.used_gpu_memory {
                UsedGpuMemory::Used(b) => Some(b / MIB),
                UsedGpuMemory::Unavailable => None,
            };
            let slot = processes.entry(p.pid).or_insert(None);
            *slot = match (*slot, used) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (a, b) => a.or(b),
            };
        }
        Ok(GpuReading {
            name: device.name().unwrap_or_default(),
            driver: self.nvml.sys_driver_version().unwrap_or_default(),
            vram_used_mib: mem.used / MIB,
            vram_total_mib: mem.total / MIB,
            util_pct: device
                .utilization_rates()
                .ok()
                .and_then(|u| u8::try_from(u.gpu).ok()),
            processes: processes.into_iter().collect(),
        })
    }
}

/// `sysinfo` for RAM, CPU, disks and names; `GetPerformanceInfo` for commit charge.
pub struct SysinfoSystem {
    sys: Mutex<sysinfo::System>,
    created: std::time::Instant,
}

impl SysinfoSystem {
    /// A fresh reader. CPU usage is a delta, so the first [`SystemSource::read`] waits until
    /// `sysinfo`'s minimum interval has passed since creation.
    #[must_use]
    pub fn new() -> Self {
        let mut sys = sysinfo::System::new();
        sys.refresh_cpu_usage();
        Self {
            sys: Mutex::new(sys),
            created: std::time::Instant::now(),
        }
    }
}

impl Default for SysinfoSystem {
    fn default() -> Self {
        Self::new()
    }
}

fn same_mount(a: &str, b: &str) -> bool {
    a.trim_end_matches('\\')
        .eq_ignore_ascii_case(b.trim_end_matches('\\'))
}

impl SystemSource for SysinfoSystem {
    fn read(&mut self, mounts: &[String]) -> Result<SystemReading, ResourceError> {
        let commit = commit_info()?;
        if let Some(wait) = sysinfo::MINIMUM_CPU_UPDATE_INTERVAL.checked_sub(self.created.elapsed())
        {
            std::thread::sleep(wait);
        }
        let sys = self
            .sys
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        sys.refresh_memory();
        sys.refresh_cpu_usage();
        let disks = sysinfo::Disks::new_with_refreshed_list();
        let disks = mounts
            .iter()
            .filter_map(|m| {
                disks
                    .iter()
                    .find(|d| same_mount(&d.mount_point().to_string_lossy(), m))
                    .map(|d| DiskUsage {
                        mount: m.clone(),
                        free_bytes: d.available_space(),
                        total_bytes: d.total_space(),
                    })
            })
            .collect();
        Ok(SystemReading {
            ram_used_mib: sys.used_memory() / MIB,
            ram_total_mib: sys.total_memory() / MIB,
            commit_used_mib: commit.used_mib,
            commit_limit_mib: commit.effective_limit_mib(),
            cpu_pct: sys.global_cpu_usage(),
            disks,
        })
    }

    fn process_names(&mut self, pids: &[u32]) -> HashMap<u32, String> {
        let sys = self
            .sys
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let ids: Vec<sysinfo::Pid> = pids.iter().map(|p| sysinfo::Pid::from_u32(*p)).collect();
        sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&ids), true);
        pids.iter()
            .filter_map(|p| {
                sys.process(sysinfo::Pid::from_u32(*p))
                    .map(|proc| (*p, proc.name().to_string_lossy().into_owned()))
            })
            .collect()
    }
}

/// Commit charge, MiB.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Commit {
    /// Committed now.
    pub used_mib: u64,
    /// Current limit (RAM + current page-file size).
    pub limit_mib: u64,
    /// Physical RAM.
    pub physical_mib: u64,
    /// Sum of configured page-file maximums; `None` if any page file is system-managed.
    pub pagefile_max_mib: Option<u64>,
}

impl Commit {
    /// The limit once page files have grown to their maximum. Windows grows them on demand,
    /// so this, not the current limit, is what a new allocation can reach.
    #[must_use]
    pub fn effective_limit_mib(&self) -> u64 {
        self.pagefile_max_mib.map_or(self.limit_mib, |m| {
            self.limit_mib.max(self.physical_mib + m)
        })
    }

    /// Effective headroom.
    #[must_use]
    pub fn headroom_mib(&self) -> u64 {
        self.effective_limit_mib().saturating_sub(self.used_mib)
    }
}

/// Reads commit charge now.
///
/// # Errors
/// [`ResourceError::Unavailable`] if the OS call fails (or off Windows).
pub fn commit_info() -> Result<Commit, ResourceError> {
    #[cfg(windows)]
    {
        let (used_mib, limit_mib, physical_mib) = win::performance_info_mib()?;
        Ok(Commit {
            used_mib,
            limit_mib,
            physical_mib,
            pagefile_max_mib: win::paging_files().as_deref().and_then(pagefile_max_mib),
        })
    }
    #[cfg(not(windows))]
    {
        Err(ResourceError::Unavailable(
            "commit charge is Windows-only".into(),
        ))
    }
}

/// Sums the maximum sizes in the registry's `PagingFiles` entries (`F:\pagefile.sys 4096
/// 32768`, one per line). `None` if any entry is system-managed (no sizes, `0 0`, or `?:`).
#[must_use]
pub fn pagefile_max_mib(paging_files: &str) -> Option<u64> {
    let mut total = 0;
    let mut any = false;
    for line in paging_files
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
    {
        let mut parts = line.split_whitespace();
        let path = parts.next()?;
        let max: u64 = parts.nth(1)?.parse().ok()?;
        if path.starts_with('?') || max == 0 {
            return None;
        }
        total += max;
        any = true;
    }
    any.then_some(total)
}

/// Per-process dedicated GPU memory from the `GPU Process Memory` performance counters.
/// This is the WDDM fallback for NVML, which usually can't attribute memory to processes.
#[cfg(windows)]
pub struct PdhGpuProcesses {
    query: win::PdhQuery,
}

#[cfg(windows)]
impl PdhGpuProcesses {
    /// Opens the counter query.
    ///
    /// # Errors
    /// [`ResourceError::Unavailable`] if the counter set is missing.
    pub fn new() -> Result<Self, ResourceError> {
        win::PdhQuery::open(r"\GPU Process Memory(*)\Dedicated Usage")
            .map(|query| Self { query })
            .map_err(ResourceError::Unavailable)
    }
}

#[cfg(windows)]
impl GpuProcessSource for PdhGpuProcesses {
    fn read(&mut self) -> Result<Vec<(u32, u64)>, ResourceError> {
        let items = self.query.read().map_err(ResourceError::Unavailable)?;
        let mut by_pid: HashMap<u32, u64> = HashMap::new();
        for (instance, bytes) in items {
            if let Some(pid) = pid_of_instance(&instance) {
                *by_pid.entry(pid).or_default() += u64::try_from(bytes).unwrap_or(0);
            }
        }
        Ok(by_pid
            .into_iter()
            .map(|(pid, bytes)| (pid, bytes / MIB))
            .filter(|(_, mib)| *mib > 0)
            .collect())
    }
}

/// Parses the pid out of a PDH instance name like `pid_1234_luid_0x0_0xD1C5_phys_0`.
#[must_use]
pub fn pid_of_instance(instance: &str) -> Option<u32> {
    instance
        .strip_prefix("pid_")?
        .split('_')
        .next()?
        .parse()
        .ok()
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod win {
    //! The two Win32 calls: `GetPerformanceInfo` and a PDH wildcard counter query.

    use windows::Win32::System::Performance::{
        PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_LARGE, PDH_HCOUNTER, PDH_HQUERY, PDH_MORE_DATA,
        PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW,
        PdhOpenQueryW,
    };
    use windows::Win32::System::ProcessStatus::{GetPerformanceInfo, PERFORMANCE_INFORMATION};
    use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_MULTI_SZ, RegGetValueW};
    use windows::core::{HSTRING, PCWSTR};

    use crate::ResourceError;

    /// `(commit total, commit limit, physical total)` in MiB.
    pub(super) fn performance_info_mib() -> Result<(u64, u64, u64), ResourceError> {
        let mut info = PERFORMANCE_INFORMATION::default();
        let size = u32::try_from(size_of::<PERFORMANCE_INFORMATION>())
            .map_err(|e| ResourceError::Unavailable(e.to_string()))?;
        info.cb = size;
        // SAFETY: `info` is a valid, writable PERFORMANCE_INFORMATION of `size` bytes.
        unsafe { GetPerformanceInfo(&raw mut info, size) }
            .map_err(|e| ResourceError::Unavailable(format!("GetPerformanceInfo: {e}")))?;
        let page = info.PageSize as u64;
        let mib = |pages: usize| pages as u64 * page / (1024 * 1024);
        Ok((
            mib(info.CommitTotal),
            mib(info.CommitLimit),
            mib(info.PhysicalTotal),
        ))
    }

    /// The `PagingFiles` registry value, one entry per line.
    pub(super) fn paging_files() -> Option<String> {
        let key =
            HSTRING::from(r"SYSTEM\CurrentControlSet\Control\Session Manager\Memory Management");
        let value = HSTRING::from("PagingFiles");
        let mut size = 0u32;
        // SAFETY: size query; strings outlive the call and no data buffer is passed.
        let status = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                &key,
                &value,
                RRF_RT_REG_MULTI_SZ,
                None,
                None,
                Some(&raw mut size),
            )
        };
        if status.is_err() || size == 0 {
            return None;
        }
        let mut buf = vec![0u16; (size as usize).div_ceil(2)];
        // SAFETY: `buf` holds at least `size` bytes.
        let status = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                &key,
                &value,
                RRF_RT_REG_MULTI_SZ,
                None,
                Some(buf.as_mut_ptr().cast()),
                Some(&raw mut size),
            )
        };
        if status.is_err() {
            return None;
        }
        buf.truncate((size as usize) / 2);
        let text = String::from_utf16_lossy(&buf);
        Some(
            text.split('\0')
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("\n"),
        )
    }

    pub(super) struct PdhQuery {
        query: PDH_HQUERY,
        counter: PDH_HCOUNTER,
    }

    // SAFETY: PDH query handles may be used from any thread, one at a time; `&mut self`
    // on `read` guarantees exclusive use.
    unsafe impl Send for PdhQuery {}

    fn check(status: u32, what: &str) -> Result<(), String> {
        if status == 0 {
            Ok(())
        } else {
            Err(format!("{what} failed: 0x{status:08X}"))
        }
    }

    impl PdhQuery {
        pub(super) fn open(path: &str) -> Result<Self, String> {
            let mut query = PDH_HQUERY::default();
            // SAFETY: out-pointer to a local handle; no data source (live counters).
            check(
                unsafe { PdhOpenQueryW(PCWSTR::null(), 0, &raw mut query) },
                "PdhOpenQuery",
            )?;
            let mut counter = PDH_HCOUNTER::default();
            let path = HSTRING::from(path);
            // SAFETY: `query` is open, `path` outlives the call, out-pointer to a local.
            let status = unsafe { PdhAddEnglishCounterW(query, &path, 0, &raw mut counter) };
            if let Err(e) = check(status, "PdhAddEnglishCounter") {
                // SAFETY: closing the query opened above.
                let _ = unsafe { PdhCloseQuery(query) };
                return Err(e);
            }
            Ok(Self { query, counter })
        }

        /// Collects once and returns `(instance, value)` for every instance.
        pub(super) fn read(&mut self) -> Result<Vec<(String, i64)>, String> {
            // SAFETY: the query handle is open for the lifetime of `self`.
            check(
                unsafe { PdhCollectQueryData(self.query) },
                "PdhCollectQueryData",
            )?;
            let mut bytes = 0u32;
            let mut count = 0u32;
            // SAFETY: size query with no buffer; PDH writes only the two counts.
            let status = unsafe {
                PdhGetFormattedCounterArrayW(
                    self.counter,
                    PDH_FMT_LARGE,
                    &raw mut bytes,
                    &raw mut count,
                    None,
                )
            };
            if status != PDH_MORE_DATA {
                check(status, "PdhGetFormattedCounterArray (size)")?;
                return Ok(Vec::new());
            }
            let item = size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>();
            let mut buf: Vec<PDH_FMT_COUNTERVALUE_ITEM_W> =
                Vec::with_capacity((bytes as usize).div_ceil(item));
            // SAFETY: `buf` has room for at least `bytes` bytes, suitably aligned for the
            // items; the instance-name strings PDH stores after the items live in the same
            // buffer and are read before it is dropped.
            check(
                unsafe {
                    PdhGetFormattedCounterArrayW(
                        self.counter,
                        PDH_FMT_LARGE,
                        &raw mut bytes,
                        &raw mut count,
                        Some(buf.as_mut_ptr()),
                    )
                },
                "PdhGetFormattedCounterArray",
            )?;
            // SAFETY: PDH initialized `count` items at the start of the buffer.
            unsafe { buf.set_len(count as usize) };
            let mut out = Vec::with_capacity(buf.len());
            for it in &buf {
                // SAFETY: `szName` points to a NUL-terminated string inside `buf`.
                let name = unsafe { it.szName.to_string() }.unwrap_or_default();
                // SAFETY: PDH_FMT_LARGE fills the `largeValue` member of the union.
                let value = unsafe { it.FmtValue.Anonymous.largeValue };
                out.push((name, value));
            }
            Ok(out)
        }
    }

    impl Drop for PdhQuery {
        fn drop(&mut self) {
            // SAFETY: closes the query opened in `open`, exactly once.
            let _ = unsafe { PdhCloseQuery(self.query) };
        }
    }
}
