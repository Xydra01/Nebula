#![allow(clippy::unwrap_used, clippy::panic, missing_docs)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use nebula_config::{NebulaConfig, VolumeGuard};
use nebula_model::Preflight;
use nebula_proto::DiskUsage;
use nebula_resources::sources::{
    GpuProcessSource, GpuReading, GpuSource, SystemReading, SystemSource, pagefile_max_mib,
    pid_of_instance,
};
use nebula_resources::{
    Commit, CommitPreflight, GB, GuardLevel, ProcessSource, ResourceError, Sampler, SamplerConfig,
    Sources, check_commit, disk_guard,
};

fn guard(mount: &str, warn: u64, block: Option<u64>, pause: Option<u64>) -> VolumeGuard {
    VolumeGuard {
        mount: mount.into(),
        warn_gb: warn,
        block_gb: block,
        pause_gb: pause,
    }
}

#[test]
fn disk_guard_thresholds() {
    let f = guard(r"F:\", 30, Some(20), Some(12));
    let d = guard(r"D:\", 50, Some(25), None);
    let level = |g: &VolumeGuard, gb: u64| disk_guard(gb * GB, g);
    assert_eq!(level(&f, 31), GuardLevel::Ok);
    assert_eq!(level(&f, 30), GuardLevel::Ok);
    assert_eq!(level(&f, 29), GuardLevel::Warn);
    assert_eq!(level(&f, 19), GuardLevel::Block);
    assert_eq!(level(&f, 11), GuardLevel::Pause);
    assert_eq!(level(&d, 49), GuardLevel::Warn);
    assert_eq!(level(&d, 1), GuardLevel::Block);
}

fn commit(used: u64, limit: u64, physical: u64, pf_max: Option<u64>) -> Commit {
    Commit {
        used_mib: used,
        limit_mib: limit,
        physical_mib: physical,
        pagefile_max_mib: pf_max,
    }
}

#[test]
fn page_file_growth_counts_toward_the_limit() {
    assert_eq!(pagefile_max_mib(r"F:\pagefile.sys 4096 32768"), Some(32768));
    assert_eq!(
        pagefile_max_mib("F:\\pagefile.sys 4096 32768\nD:\\pagefile.sys 0 1024"),
        Some(33792)
    );
    assert_eq!(pagefile_max_mib(r"?:\pagefile.sys"), None);
    assert_eq!(pagefile_max_mib(r"F:\pagefile.sys 0 0"), None);
    assert_eq!(pagefile_max_mib(""), None);

    // 32 GB RAM, page file currently 4 GB of a 32 GB maximum.
    let c = commit(30_000, 36_864, 32_768, Some(32_768));
    assert_eq!(c.effective_limit_mib(), 65_536);
    assert_eq!(c.headroom_mib(), 35_536);
    assert_eq!(commit(30_000, 36_864, 32_768, None).headroom_mib(), 6_864);
}

#[test]
fn commit_check_needs_estimate_plus_margin() {
    let c = commit(50_000, 60_000, 32_768, None);
    assert!(check_commit("standard", 8_000, 1_024, &c).is_ok());
    let err = check_commit("standard", 10_700, 1_024, &c).unwrap_err();
    assert!(err.contains("11724 MiB"), "{err}");
    assert!(err.contains("10000 MiB is free"), "{err}");
}

fn profile(estimate: Option<u64>) -> nebula_model::ModelProfile {
    let cfg = NebulaConfig::from_toml(None).unwrap();
    let mut p = cfg.model.profiles["standard"].clone();
    p.commit_estimate_mib = estimate;
    p
}

#[test]
fn commit_preflight_hook() {
    let tight =
        CommitPreflight::with_reader(1_024, Arc::new(|| Ok(commit(50_000, 55_000, 32_768, None))));
    assert!(tight.check("standard", &profile(Some(10_700))).is_err());
    assert!(tight.check("standard", &profile(None)).is_ok());
    let unreadable = CommitPreflight::with_reader(
        1_024,
        Arc::new(|| Err(ResourceError::Unavailable("nope".into()))),
    );
    assert!(unreadable.check("standard", &profile(Some(10_700))).is_ok());
}

#[test]
fn pdh_instance_names() {
    assert_eq!(
        pid_of_instance("pid_1234_luid_0x00000000_0x0000D1C5_phys_0"),
        Some(1234)
    );
    assert_eq!(pid_of_instance("luid_0x0_phys_0"), None);
    assert_eq!(pid_of_instance("pid_x_luid"), None);
}

struct FakeGpu(Option<GpuReading>);
impl GpuSource for FakeGpu {
    fn read(&mut self) -> Result<GpuReading, ResourceError> {
        self.0
            .clone()
            .ok_or_else(|| ResourceError::Unavailable("no gpu".into()))
    }
}

struct FakeProcs(Vec<(u32, u64)>);
impl GpuProcessSource for FakeProcs {
    fn read(&mut self) -> Result<Vec<(u32, u64)>, ResourceError> {
        Ok(self.0.clone())
    }
}

struct FakeSystem;
impl SystemSource for FakeSystem {
    fn read(&mut self, mounts: &[String]) -> Result<SystemReading, ResourceError> {
        Ok(SystemReading {
            ram_used_mib: 20_000,
            ram_total_mib: 32_768,
            commit_used_mib: 40_000,
            commit_limit_mib: 65_536,
            cpu_pct: 12.5,
            disks: mounts
                .iter()
                .map(|m| DiskUsage {
                    mount: m.clone(),
                    free_bytes: 100 * GB,
                    total_bytes: 1000 * GB,
                })
                .collect(),
        })
    }
    fn process_names(&mut self, pids: &[u32]) -> HashMap<u32, String> {
        pids.iter().map(|p| (*p, format!("proc{p}.exe"))).collect()
    }
}

fn gpu(processes: Vec<(u32, Option<u64>)>) -> GpuReading {
    GpuReading {
        name: "RTX".into(),
        driver: "1.0".into(),
        vram_used_mib: 9_000,
        vram_total_mib: 12_282,
        util_pct: Some(40),
        processes,
    }
}

fn mounts() -> Vec<String> {
    vec![r"F:\".into(), r"D:\".into()]
}

#[test]
fn snapshot_prefers_nvml_amounts() {
    let mut s = Sources {
        gpu: Some(Box::new(FakeGpu(Some(gpu(vec![
            (10, Some(500)),
            (11, Some(7_000)),
        ]))))),
        gpu_processes: Some(Box::new(FakeProcs(vec![(99, 1)]))),
        system: Box::new(FakeSystem),
    };
    let (snap, source) = s.snapshot(&mounts()).unwrap();
    assert_eq!(source, ProcessSource::Nvml);
    assert_eq!((snap.vram_used_mib, snap.vram_total_mib), (9_000, 12_282));
    assert_eq!(snap.commit_used_mib, 40_000);
    assert_eq!(snap.disks.len(), 2);
    let procs: Vec<(u32, &str, u64)> = snap
        .gpu_processes
        .iter()
        .map(|p| (p.pid, p.name.as_str(), p.vram_mib))
        .collect();
    assert_eq!(procs, [(11, "proc11.exe", 7_000), (10, "proc10.exe", 500)]);
}

#[test]
fn snapshot_falls_back_to_pdh_and_keeps_the_top_ten() {
    let mut s = Sources {
        gpu: Some(Box::new(FakeGpu(Some(gpu(vec![(10, None)]))))),
        gpu_processes: Some(Box::new(FakeProcs(
            (1..=15).map(|p| (p, u64::from(p) * 100)).collect(),
        ))),
        system: Box::new(FakeSystem),
    };
    let (snap, source) = s.snapshot(&mounts()).unwrap();
    assert_eq!(source, ProcessSource::Pdh);
    assert_eq!(snap.gpu_processes.len(), 10);
    assert_eq!(snap.gpu_processes[0].pid, 15);
    assert_eq!(snap.gpu_processes[9].pid, 6);
}

#[test]
fn snapshot_without_a_gpu_reads_zero() {
    let mut s = Sources {
        gpu: Some(Box::new(FakeGpu(None))),
        gpu_processes: None,
        system: Box::new(FakeSystem),
    };
    let (snap, source) = s.snapshot(&mounts()).unwrap();
    assert_eq!(source, ProcessSource::None);
    assert_eq!((snap.vram_total_mib, snap.gpu_util_pct), (0, None));
    assert_eq!(snap.ram_total_mib, 32_768);
}

#[test]
fn sampler_publishes_and_stops_on_drop() {
    let sources = Sources {
        gpu: Some(Box::new(FakeGpu(Some(gpu(vec![]))))),
        gpu_processes: None,
        system: Box::new(FakeSystem),
    };
    let cfg = SamplerConfig {
        interval: Duration::from_millis(100),
        log_every: Duration::from_secs(30),
        volumes: vec![guard(r"F:\", 30, Some(20), Some(12))],
    };
    let sampler = Sampler::start(sources, cfg).unwrap();
    let mut rx = sampler.subscribe();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    rt.block_on(async {
        tokio::time::timeout(Duration::from_secs(5), rx.wait_for(Option::is_some))
            .await
            .unwrap()
            .unwrap();
    });
    let snap = sampler.latest().unwrap();
    assert_eq!(snap.disks[0].mount, r"F:\");
    let started = std::time::Instant::now();
    drop(sampler);
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
#[ignore = "needs an NVIDIA GPU"]
fn real_nvml_reads() {
    let mut g = nebula_resources::sources::NvmlGpu::new().unwrap();
    let r = g.read().unwrap();
    println!("{r:?}");
    assert!(r.vram_total_mib > 0);
}

#[cfg(windows)]
#[test]
#[ignore = "needs a GPU with WDDM counters"]
fn real_pdh_and_snapshot() {
    let mut p = nebula_resources::sources::PdhGpuProcesses::new().unwrap();
    println!("pdh: {:?}", p.read().unwrap());
    let mut s = Sources::real();
    let (snap, source) = s.snapshot(&mounts()).unwrap();
    println!("source {source}: {snap:#?}");
    println!(
        "commit: {:?}",
        nebula_resources::sources::commit_info().unwrap()
    );
}
