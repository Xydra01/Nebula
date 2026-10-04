#![allow(clippy::unwrap_used, clippy::panic, missing_docs)]

use std::time::Duration;

use nebula_config::NebulaConfig;
use nebula_proto::{CheckStatus, DiskUsage, DoctorCheck};
use nebula_resources::doctor::{
    BackupAuth, BackupState, DiskEvent, EFI_GPT_TYPE, FirewallRule, LocalInputs, Partition,
    SystemFacts, backup_auth, evaluate, record_backup_auth, smart_checks,
};
use nebula_resources::sources::GpuReading;
use nebula_resources::{Commit, GB};
use time::macros::datetime;

fn cfg() -> NebulaConfig {
    NebulaConfig::from_toml(None).unwrap()
}

fn healthy_facts() -> SystemFacts {
    SystemFacts {
        page_files: vec![r"F:\pagefile.sys".into()],
        partitions: vec![
            Partition {
                disk: 2,
                letter: None,
                gpt_type: EFI_GPT_TYPE.into(),
            },
            Partition {
                disk: 2,
                letter: Some("F".into()),
                gpt_type: String::new(),
            },
            Partition {
                disk: 1,
                letter: Some("D".into()),
                gpt_type: String::new(),
            },
        ],
        disk_events: vec![],
        ssh_rules: vec![FirewallRule {
            name: "OpenSSH-Server-In-TCP".into(),
            enabled: true,
            remote: vec![
                "100.64.0.0/255.192.0.0".into(),
                "fd7a:115c:a1e0::/48".into(),
            ],
        }],
        wsl: Some(true),
    }
}

fn disk(mount: &str, free_gb: u64) -> DiskUsage {
    DiskUsage {
        mount: mount.into(),
        free_bytes: free_gb * GB,
        total_bytes: 1000 * GB,
    }
}

fn healthy() -> LocalInputs {
    LocalInputs {
        gpu: Ok(GpuReading {
            name: "RTX 4070".into(),
            driver: "581.57".into(),
            vram_used_mib: 1_200,
            vram_total_mib: 12_282,
            util_pct: Some(3),
            processes: vec![],
        }),
        disks: vec![disk(r"F:\", 400), disk(r"D:\", 800)],
        commit: Ok(Commit {
            used_mib: 30_000,
            limit_mib: 36_864,
            physical_mib: 32_768,
            pagefile_max_mib: Some(32_768),
        }),
        facts: Ok(healthy_facts()),
        events_since: datetime!(2026-10-01 00:00 UTC),
        smart_files: Some(vec![(
            "dev_sdc-2026-10-01_1932.json".into(),
            nvme_json("S1", 2, 99),
        )]),
        backups: BackupState {
            uploaded_age: Some(Duration::from_secs(3600)),
            uploaded_name: Some("nebula-20261004T110000Z.tar.zst".into()),
            last_error: None,
        },
        backup_auth: BackupAuth::SignedIn(datetime!(2026-10-03 12:00 UTC)),
        log_bytes: Ok(50 * 1024 * 1024),
        log_writable: Ok(()),
        now: datetime!(2026-10-04 12:00 UTC),
    }
}

fn nvme_json(serial: &str, media_errors: u64, spare: u64) -> String {
    format!(
        "\u{feff}{{\"serial_number\":\"{serial}\",\"model_name\":\"Samsung SSD 980 1TB\",\
         \"smart_status\":{{\"passed\":true}},\"temperature\":{{\"current\":41}},\
         \"nvme_smart_health_information_log\":{{\"media_errors\":{media_errors},\
         \"available_spare\":{spare},\"available_spare_threshold\":10,\"percentage_used\":4}}}}"
    )
}

fn ata_json(serial: &str, pending: u64, passed: bool) -> String {
    format!(
        "{{\"serial_number\":\"{serial}\",\"model_name\":\"WDC\",\"smart_status\":{{\"passed\":{passed}}},\
         \"ata_smart_attributes\":{{\"table\":[{{\"id\":5,\"name\":\"Reallocated_Sector_Ct\",\"raw\":{{\"value\":0}}}},\
         {{\"id\":197,\"name\":\"Current_Pending_Sector\",\"raw\":{{\"value\":{pending}}}}}]}}}}"
    )
}

fn find<'a>(checks: &'a [DoctorCheck], name: &str) -> &'a DoctorCheck {
    checks
        .iter()
        .find(|c| c.name == name)
        .unwrap_or_else(|| panic!("no check {name} in {checks:#?}"))
}

fn status(checks: &[DoctorCheck], name: &str) -> CheckStatus {
    find(checks, name).status
}

#[test]
fn healthy_machine_is_all_ok() {
    let checks = evaluate(&cfg(), &healthy());
    let bad: Vec<&DoctorCheck> = checks
        .iter()
        .filter(|c| c.status != CheckStatus::Ok)
        .collect();
    assert!(bad.is_empty(), "{bad:#?}");
    for name in [
        "gpu",
        r"disk.F:",
        r"disk.D:",
        "memory.commit",
        "paths.retired_drive",
        "retired_drive.attached",
        "boot.efi",
        "memory.pagefile",
        "disk.events",
        "smart.age",
        "smart.sdc",
        "backup",
        "backup.auth",
        "wsl",
        "ssh.firewall",
        "logs.budget",
        "logs.writable",
    ] {
        find(&checks, name);
    }
    assert!(find(&checks, "gpu").detail.contains("RTX 4070"));
}

#[test]
fn disk_thresholds_map_to_doctor_status() {
    let mut i = healthy();
    i.disks = vec![disk(r"F:\", 25), disk(r"D:\", 10)];
    let checks = evaluate(&cfg(), &i);
    assert_eq!(status(&checks, r"disk.F:"), CheckStatus::Warn);
    assert_eq!(
        status(&checks, r"disk.D:"),
        CheckStatus::Warn,
        "D: has no pause level, so never Fail"
    );
    i.disks = vec![disk(r"F:\", 15)];
    let checks = evaluate(&cfg(), &i);
    assert_eq!(status(&checks, r"disk.F:"), CheckStatus::Fail);
    assert_eq!(
        status(&checks, r"disk.D:"),
        CheckStatus::Fail,
        "missing volume"
    );
}

#[test]
fn gpu_missing_or_full() {
    let mut i = healthy();
    i.gpu = Err("NVML: driver not loaded".into());
    assert_eq!(status(&evaluate(&cfg(), &i), "gpu"), CheckStatus::Fail);
    i = healthy();
    if let Ok(g) = &mut i.gpu {
        g.vram_used_mib = 11_800;
    }
    assert_eq!(status(&evaluate(&cfg(), &i), "gpu"), CheckStatus::Warn);
}

#[test]
fn retired_drive_attached_with_efi_and_page_file() {
    let mut i = healthy();
    let mut f = healthy_facts();
    f.partitions.push(Partition {
        disk: 0,
        letter: Some("C".into()),
        gpt_type: String::new(),
    });
    f.partitions.push(Partition {
        disk: 0,
        letter: None,
        gpt_type: EFI_GPT_TYPE.into(),
    });
    f.page_files.push(r"C:\pagefile.sys".into());
    i.facts = Ok(f);
    let checks = evaluate(&cfg(), &i);
    assert_eq!(status(&checks, "retired_drive.attached"), CheckStatus::Warn);
    assert_eq!(status(&checks, "boot.efi"), CheckStatus::Fail);
    assert_eq!(status(&checks, "memory.pagefile"), CheckStatus::Fail);
}

#[test]
fn nebula_path_on_the_retired_drive_fails() {
    let mut c = cfg();
    c.paths.state = r"c:\Nebula\state".into();
    let checks = evaluate(&c, &healthy());
    assert_eq!(status(&checks, "paths.retired_drive"), CheckStatus::Fail);
    assert!(
        find(&checks, "paths.retired_drive")
            .detail
            .contains("paths.state")
    );
}

#[test]
fn ssh_rule_open_to_any_fails() {
    let mut i = healthy();
    let mut f = healthy_facts();
    f.ssh_rules[0].remote = vec!["*".into()];
    i.facts = Ok(f.clone());
    assert_eq!(
        status(&evaluate(&cfg(), &i), "ssh.firewall"),
        CheckStatus::Fail
    );
    f.ssh_rules[0].enabled = false;
    i.facts = Ok(f);
    assert_eq!(
        status(&evaluate(&cfg(), &i), "ssh.firewall"),
        CheckStatus::Warn
    );
}

#[test]
fn disk_events_warn() {
    let mut i = healthy();
    let mut f = healthy_facts();
    f.disk_events.push(DiskEvent {
        time: "2026-10-03T01:02:03Z".into(),
        provider: "disk".into(),
        id: 7,
        message: r"The device, \Device\Harddisk1\DR1, has a bad block.".into(),
    });
    i.facts = Ok(f);
    let checks = evaluate(&cfg(), &i);
    assert_eq!(status(&checks, "disk.events"), CheckStatus::Warn);
    assert!(find(&checks, "disk.events").detail.contains("bad block"));
}

#[test]
fn facts_unavailable_warns_but_does_not_fail() {
    let mut i = healthy();
    i.facts = Err("powershell timed out".into());
    let checks = evaluate(&cfg(), &i);
    for name in ["retired_drive", "disk.events", "wsl", "ssh.firewall"] {
        assert_eq!(status(&checks, name), CheckStatus::Warn, "{name}");
    }
}

#[test]
fn backups_and_logs() {
    let mut i = healthy();
    i.backups = BackupState::default();
    i.log_bytes = Ok(3 * GB);
    i.log_writable = Err("access denied".into());
    let checks = evaluate(&cfg(), &i);
    assert_eq!(status(&checks, "backup"), CheckStatus::Fail);
    assert!(
        find(&checks, "backup")
            .detail
            .contains("no successful off-site backup")
    );
    assert_eq!(status(&checks, "logs.budget"), CheckStatus::Warn);
    assert_eq!(status(&checks, "logs.writable"), CheckStatus::Fail);
}

#[test]
fn backup_age_and_errors() {
    let judge = |b: BackupState| {
        let mut i = healthy();
        i.backups = b;
        find(&evaluate(&cfg(), &i), "backup").clone()
    };
    let hours = |h: u64| Some(Duration::from_secs(h * 3600));

    let c = judge(BackupState {
        uploaded_age: hours(40),
        ..BackupState::default()
    });
    assert_eq!(c.status, CheckStatus::Warn);
    assert!(c.detail.contains("40 h ago"), "{c:?}");

    let c = judge(BackupState {
        uploaded_age: hours(5),
        last_error: Some("rclone copyto failed".into()),
        ..BackupState::default()
    });
    assert_eq!(c.status, CheckStatus::Warn);
    assert!(
        c.detail.contains("last run failed: rclone copyto failed"),
        "{c:?}"
    );

    let c = judge(BackupState {
        last_error: Some("token expired".into()),
        ..BackupState::default()
    });
    assert_eq!(c.status, CheckStatus::Fail);
    assert!(c.detail.contains("token expired"), "{c:?}");
}

#[test]
fn backup_sign_in_expiry() {
    let judge = |cfg: &NebulaConfig, auth: BackupAuth| {
        let mut i = healthy();
        i.backup_auth = auth;
        find(&evaluate(cfg, &i), "backup.auth").clone()
    };
    let at = |d: time::OffsetDateTime| BackupAuth::SignedIn(d);
    let cfg = cfg();

    // Now is 2026-10-04 12:00; the sign-in lasts 7 days and doctor warns 2 days ahead.
    let c = judge(&cfg, at(datetime!(2026-10-03 12:00 UTC)));
    assert_eq!(c.status, CheckStatus::Ok, "{c:?}");
    assert!(c.detail.contains("expires in 6 days (2026-10-10)"), "{c:?}");

    let c = judge(&cfg, at(datetime!(2026-09-29 12:00 UTC)));
    assert_eq!(c.status, CheckStatus::Warn, "{c:?}");
    assert!(c.detail.contains("expires in 48 h") && c.detail.contains("nebula backup reauth"));

    let c = judge(&cfg, at(datetime!(2026-09-27 12:00 UTC)));
    assert_eq!(c.status, CheckStatus::Fail, "{c:?}");
    assert!(c.detail.contains("expired 2026-10-04"), "{c:?}");

    assert_eq!(
        judge(&cfg, BackupAuth::Unrecorded).status,
        CheckStatus::Warn
    );
    assert_eq!(
        judge(&cfg, BackupAuth::NoRcloneConfig).status,
        CheckStatus::Warn
    );

    let mut forever = cfg.clone();
    forever.backup.token_lifetime_days = 0;
    let c = judge(&forever, at(datetime!(2020-01-01 00:00 UTC)));
    assert_eq!(c.status, CheckStatus::Ok);
    assert!(c.detail.contains("does not expire"));
}

#[test]
fn backup_sign_in_is_recorded_per_remote() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = cfg();
    cfg.paths.state = dir.path().join("state");
    cfg.backup.rclone_config = dir.path().join("rclone.conf");
    assert_eq!(backup_auth(&cfg), BackupAuth::NoRcloneConfig);

    std::fs::write(&cfg.backup.rclone_config, "x").unwrap();
    assert_eq!(backup_auth(&cfg), BackupAuth::Unrecorded);

    let t = datetime!(2026-10-04 12:34:56 UTC);
    record_backup_auth(&cfg, t).unwrap();
    assert_eq!(backup_auth(&cfg), BackupAuth::SignedIn(t));

    cfg.backup.auth_remote = "b2:".into();
    assert_eq!(backup_auth(&cfg), BackupAuth::Unrecorded);
}

#[test]
fn smart_not_set_up_or_stale() {
    let now = datetime!(2026-10-04 12:00 UTC);
    let checks = smart_checks(None, now);
    assert_eq!(status(&checks, "smart"), CheckStatus::Warn);
    assert_eq!(
        status(&smart_checks(Some(&[]), now), "smart"),
        CheckStatus::Warn
    );
    let files = vec![(
        "dev_sdc-2026-08-01_1200.json".to_owned(),
        nvme_json("S1", 2, 99),
    )];
    assert_eq!(
        status(&smart_checks(Some(&files), now), "smart.age"),
        CheckStatus::Warn
    );
}

#[test]
fn smart_trend_compares_with_the_previous_snapshot() {
    let now = datetime!(2026-10-04 12:00 UTC);
    let files = vec![
        (
            "dev_sdc-2026-09-01_1200.json".to_owned(),
            nvme_json("S1", 2, 99),
        ),
        (
            "dev_sda-2026-09-01_1200.json".to_owned(),
            ata_json("W1", 0, true),
        ),
        (
            "dev_sdc-2026-10-01_1932.json".to_owned(),
            nvme_json("S1", 3, 99),
        ),
        (
            "dev_sda-2026-10-01_1932.json".to_owned(),
            ata_json("W1", 8, true),
        ),
        // Only in the old batch: not judged.
        (
            "dev_sdb-2026-09-01_1200.json".to_owned(),
            ata_json("GONE", 0, false),
        ),
    ];
    let checks = smart_checks(Some(&files), now);
    assert_eq!(status(&checks, "smart.age"), CheckStatus::Ok);
    assert_eq!(status(&checks, "smart.sdc"), CheckStatus::Warn);
    assert!(
        find(&checks, "smart.sdc")
            .detail
            .contains("media errors rose 2 -> 3")
    );
    assert_eq!(status(&checks, "smart.sda"), CheckStatus::Warn);
    assert!(
        find(&checks, "smart.sda")
            .detail
            .contains("pending rose 0 -> 8")
    );
    assert!(checks.iter().all(|c| c.name != "smart.sdb"));
}

#[test]
fn smart_failures() {
    let now = datetime!(2026-10-04 12:00 UTC);
    let files = vec![
        (
            "dev_sdc-2026-10-01_1932.json".to_owned(),
            nvme_json("S1", 2, 5),
        ),
        (
            "dev_sda-2026-10-01_1932.json".to_owned(),
            ata_json("W1", 0, false),
        ),
    ];
    let checks = smart_checks(Some(&files), now);
    assert_eq!(status(&checks, "smart.sdc"), CheckStatus::Fail);
    assert_eq!(status(&checks, "smart.sda"), CheckStatus::Fail);
}

#[cfg(windows)]
#[test]
#[ignore = "reads the real machine (PowerShell, NVML, SMART files)"]
fn real_local_checks() {
    let cfg = cfg();
    let inputs = nebula_resources::doctor::gather(&cfg);
    println!("{:#?}", inputs.facts);
    for c in evaluate(&cfg, &inputs) {
        println!("{:?} {}: {}", c.status, c.name, c.detail);
    }
}
