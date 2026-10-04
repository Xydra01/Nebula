#![allow(clippy::unwrap_used, clippy::panic, missing_docs)]

use std::path::Path;

use nebula_backup::archive::{self, Source};
use nebula_backup::{
    BackupError, Options, Outcome, Policy, Rclone, RemoteFile, archive_name, backup_now,
    load_record, parse_name, prune_local_plan, prune_remote_plan, restore, retention,
    stale_partials,
};
use nebula_config::NebulaConfig;
use time::macros::datetime;
use time::{Duration, OffsetDateTime};

const POLICY: Policy = Policy {
    recent: 8,
    daily: 14,
    weekly: 8,
    monthly: 6,
};

#[test]
fn names_round_trip() {
    let t = datetime!(2026-10-04 17:00:05 UTC);
    let n = archive_name(t);
    assert_eq!(n, "nebula-20261004T170005Z.tar.zst");
    assert_eq!(parse_name(&n), Some(t));
    assert_eq!(parse_name("nebula-2026.tar.zst"), None);
    assert_eq!(parse_name("other.txt"), None);
    let local = datetime!(2026-10-04 13:00:05 -4);
    assert_eq!(archive_name(local), n);
}

#[test]
fn retention_over_a_year_of_six_hourly_backups() {
    let start = datetime!(2025-10-01 00:00 UTC);
    let times: Vec<OffsetDateTime> = (0..365 * 4)
        .map(|i| start + Duration::hours(6 * i))
        .collect();
    let newest = *times.last().unwrap();
    let keep = retention::keep(&times, POLICY);

    let recent: Vec<_> = times.iter().rev().take(8).collect();
    assert!(recent.iter().all(|t| keep.contains(t)));
    // 8 recent span 2 days; 14 daily, 8 weekly and 6 monthly overlap with them.
    assert!(keep.len() <= 8 + 14 + 8 + 6);
    assert!(keep.len() >= 20, "{}", keep.len());
    let oldest = *keep.iter().next().unwrap();
    assert!(
        newest - oldest > Duration::days(140),
        "monthly tier reaches back ~5 months"
    );
    assert!(newest - oldest < Duration::days(200));
    for m in 0..6 {
        let month = newest.month().nth_prev(m);
        assert!(
            keep.iter().any(|t| t.month() == month),
            "month {month} kept"
        );
    }
}

#[test]
fn retention_keeps_everything_when_there_are_few() {
    let t0 = datetime!(2026-10-04 00:00 UTC);
    let times: Vec<_> = (0..5).map(|i| t0 + Duration::hours(6 * i)).collect();
    assert_eq!(retention::keep(&times, POLICY).len(), 5);
}

fn file(t: OffsetDateTime) -> RemoteFile {
    RemoteFile {
        name: archive_name(t),
        size: 10_000,
    }
}

#[test]
fn prune_plans() {
    let now = datetime!(2026-10-20 00:00 UTC);
    let files: Vec<RemoteFile> = (0..80)
        .map(|i| file(now - Duration::hours(6 * i)))
        .collect();
    let doomed = prune_remote_plan(&files, POLICY);
    assert_eq!(
        doomed.len()
            + retention::keep(
                &files
                    .iter()
                    .filter_map(|f| parse_name(&f.name))
                    .collect::<Vec<_>>(),
                POLICY
            )
            .len(),
        80
    );
    assert!(!doomed.iter().any(|f| f.name == archive_name(now)));

    let mut with_other = files.clone();
    with_other.push(RemoteFile {
        name: "notes.txt".into(),
        size: 1,
    });
    assert!(
        !prune_remote_plan(&with_other, POLICY)
            .iter()
            .any(|f| f.name == "notes.txt")
    );

    let local = prune_local_plan(&files, now, 7);
    assert!(
        local
            .iter()
            .all(|f| parse_name(&f.name).unwrap() < now - Duration::days(7))
    );
    assert_eq!(local.len(), 80 - 29);

    // The newest local copy survives even when it's old.
    let stale = vec![
        file(now - Duration::days(30)),
        file(now - Duration::days(40)),
    ];
    let p = prune_local_plan(&stale, now, 7);
    assert_eq!(p.len(), 1);
    assert_eq!(p[0].name, archive_name(now - Duration::days(40)));
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

#[test]
fn archive_round_trip_and_tamper_check() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    write(&state.join("smart").join("a.json"), "{\"x\":1}");
    write(&state.join("doctor.json"), "volatile");
    write(&state.join("model-hashes.json"), "{}");
    let sources = vec![Source {
        label: "state".into(),
        dir: state.clone(),
        exclude: vec!["doctor.json".into()],
    }];
    let entries = archive::collect(&sources).unwrap();
    let paths: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(paths, ["state/model-hashes.json", "state/smart/a.json"]);

    let d1 = archive::digest(&entries);
    write(&state.join("doctor.json"), "changed");
    assert_eq!(
        d1,
        archive::digest(&archive::collect(&sources).unwrap()),
        "excluded files don't count"
    );
    write(&state.join("smart").join("a.json"), "{\"x\":2}");
    assert_ne!(d1, archive::digest(&archive::collect(&sources).unwrap()));

    let entries = archive::collect(&sources).unwrap();
    let tar = dir.path().join("b.tar.zst");
    let t = datetime!(2026-10-04 12:00 UTC);
    archive::write(&sources, &entries, t, &tar).unwrap();
    let out = dir.path().join("out");
    let m = archive::extract(&tar, &out).unwrap();
    assert_eq!(m.created_at, t);
    assert_eq!(m.files.len(), 2);
    assert_eq!(m.sources["state"], state);
    assert_eq!(
        std::fs::read_to_string(out.join("state/smart/a.json")).unwrap(),
        "{\"x\":2}"
    );

    let err = archive::extract(&tar, &out).unwrap_err();
    assert!(err.to_string().contains("not empty"), "{err}");
}

fn rclone_exe() -> String {
    std::env::var("NEBULA_TEST_RCLONE").unwrap_or_else(|_| "rclone".into())
}

fn cfg(root: &Path) -> NebulaConfig {
    let mut cfg = NebulaConfig::from_toml(None).unwrap();
    cfg.paths.state = root.join("state");
    cfg.paths.config = root.join("config");
    cfg.paths.backups_local = root.join("cold").join("backups-local");
    cfg.backup.rclone = rclone_exe().into();
    cfg.backup.rclone_config = root.join("config").join("rclone.conf");
    write(&root.join("config").join("nebula.toml"), "# override");
    let remote = root.join("remote");
    std::fs::create_dir_all(&remote).unwrap();
    cfg.backup.remote = format!("{}/", remote.display()).replace('\\', "/");
    cfg
}

#[test]
fn only_old_backup_partials_are_stale() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = cfg(dir.path());
    let local = &cfg.paths.backups_local;
    let name = archive_name(datetime!(2026-10-04 12:00 UTC));
    write(&local.join(format!("{name}.partial")), "half");
    write(&local.join("someone-else.partial"), "x");
    write(&local.join(&name), "done");

    let now = OffsetDateTime::now_utc();
    assert!(stale_partials(&cfg, now).unwrap().is_empty());
    let later: Vec<_> = stale_partials(&cfg, now + Duration::hours(2))
        .unwrap()
        .into_iter()
        .map(|f| f.name)
        .collect();
    assert_eq!(later, [format!("{name}.partial")]);
}

#[test]
#[ignore = "needs rclone on PATH (or NEBULA_TEST_RCLONE)"]
fn backup_upload_retention_and_restore_with_a_local_remote() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = cfg(dir.path());
    write(&cfg.paths.state.join("smart").join("a.json"), "{\"x\":1}");
    let rc = Rclone::new(&cfg.backup.rclone, &cfg.backup.rclone_config, "");

    let t0 = datetime!(2026-10-04 12:00 UTC);
    let Outcome::Done(r) = backup_now(&cfg, &rc, Options::default(), t0).unwrap() else {
        panic!("skipped")
    };
    assert!(r.files >= 1);
    assert!(dir.path().join("remote").join(&r.name).exists());
    assert!(cfg.paths.backups_local.join(&r.name).exists());
    let rec = load_record(&cfg);
    assert_eq!(rec.last_uploaded.as_deref(), Some(r.name.as_str()));
    assert!(rec.last_error.is_none());

    let skip = Options {
        if_changed: true,
        ..Options::default()
    };
    let t1 = t0 + Duration::hours(6);
    assert_eq!(backup_now(&cfg, &rc, skip, t1).unwrap(), Outcome::Unchanged);
    write(&cfg.paths.state.join("smart").join("b.json"), "{}");
    assert!(matches!(
        backup_now(&cfg, &rc, skip, t1).unwrap(),
        Outcome::Done(_)
    ));

    // A tiny limit makes retention ask before deleting.
    for i in 2..12 {
        write(&cfg.paths.state.join(format!("f{i}")), "x");
        backup_now(&cfg, &rc, Options::default(), t0 + Duration::hours(6 * i)).unwrap();
    }
    cfg.backup.keep_recent = 1;
    cfg.backup.keep_daily = 1;
    cfg.backup.keep_weekly = 0;
    cfg.backup.keep_monthly = 0;
    cfg.backup.max_delete_files = 2;
    let t_last = t0 + Duration::hours(6 * 12);
    let err = backup_now(&cfg, &rc, Options::default(), t_last).unwrap_err();
    assert!(matches!(err, BackupError::TooManyDeletes { .. }), "{err}");
    assert!(
        load_record(&cfg)
            .last_error
            .unwrap()
            .contains("--allow-large-delete")
    );
    let allow = Options {
        allow_large_delete: true,
        ..Options::default()
    };
    let Outcome::Done(r) = backup_now(&cfg, &rc, allow, t_last + Duration::hours(6)).unwrap()
    else {
        panic!("skipped")
    };
    assert!(!r.pruned_remote.is_empty());
    assert!(load_record(&cfg).last_error.is_none());

    // Restore from the remote only (local copy removed), into a scratch folder.
    std::fs::remove_file(cfg.paths.backups_local.join(&r.name)).unwrap();
    let dest = dir.path().join("restore");
    let m = restore(&cfg, Some(&rc), &r.name, &dest).unwrap();
    assert!(m.files.iter().any(|f| f.path == "state/smart/a.json"));
    assert!(m.files.iter().any(|f| f.path == "config/nebula.toml"));
    assert!(
        !m.files
            .iter()
            .any(|f| f.path.ends_with(nebula_backup::RECORD_FILE))
    );
    assert_eq!(
        std::fs::read_to_string(dest.join("state/smart/a.json")).unwrap(),
        "{\"x\":1}"
    );
    assert!(matches!(
        restore(
            &cfg,
            Some(&rc),
            "nebula-20000101T000000Z",
            &dir.path().join("r2")
        ),
        Err(BackupError::NotFound(_))
    ));
}
