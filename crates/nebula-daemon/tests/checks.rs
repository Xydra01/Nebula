#![allow(clippy::unwrap_used, clippy::panic, missing_docs)]

use nebula_daemon::checks::{
    HashCache, MODELS_LOCK, RUNTIME_LOCK, hash_cached, judge_version, model_check, parse_version,
    pinned_models,
};
use nebula_proto::{CheckStatus, ModelState, ModelStatus};
use time::OffsetDateTime;

const VERSION_OUT: &str = "ggml_cuda_init: found 1 CUDA devices\nversion: 0.2.0-dev (build 10743, commit adfffbe41)\nbuilt with MSVC 19.44.35229.0 for Windows AMD64\n";

#[test]
fn version_parsing() {
    assert_eq!(
        parse_version(VERSION_OUT),
        Some((10743, "adfffbe41".into()))
    );
    assert_eq!(parse_version("nonsense"), None);
}

#[test]
fn version_against_the_lock() {
    let lock: toml::Table = RUNTIME_LOCK.parse().unwrap();
    let prism = lock["llama-prism"].as_table();
    assert_eq!(
        judge_version("llama-prism", VERSION_OUT, prism).status,
        CheckStatus::Ok
    );
    let other = VERSION_OUT.replace("10743", "10800");
    let c = judge_version("llama-prism", &other, prism);
    assert_eq!(c.status, CheckStatus::Warn);
    assert!(c.detail.contains("pins build 10743"), "{}", c.detail);
    assert_eq!(
        judge_version("x", VERSION_OUT, None).status,
        CheckStatus::Warn
    );
    assert_eq!(
        judge_version("x", "garbage", prism).status,
        CheckStatus::Warn
    );
}

#[test]
fn the_default_profiles_are_pinned() {
    let lock: toml::Table = MODELS_LOCK.parse().unwrap();
    let pinned = pinned_models(&lock);
    let cfg = nebula_config::NebulaConfig::from_toml(None).unwrap();
    for (name, p) in &cfg.model.profiles {
        let key = p.model.to_string_lossy().to_lowercase();
        assert!(
            pinned.contains_key(&key),
            "profile {name}: {} not in models.lock.toml",
            p.model.display()
        );
    }
}

#[test]
fn hashes_are_cached_by_size_and_mtime() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("m.gguf");
    std::fs::write(&f, b"abc").unwrap();
    let mut cache = HashCache::new();
    let (sha, bytes, cached) = hash_cached(&f, &mut cache).unwrap();
    assert_eq!(
        sha,
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!((bytes, cached), (3, false));
    assert!(hash_cached(&f, &mut cache).unwrap().2);
    std::fs::write(&f, b"abcd").unwrap();
    let (sha2, bytes, cached) = hash_cached(&f, &mut cache).unwrap();
    assert_ne!(sha2, sha);
    assert_eq!((bytes, cached), (4, false));
}

fn status(state: ModelState, last_error: Option<&str>) -> ModelStatus {
    ModelStatus {
        profile: "standard".into(),
        state,
        since: OffsetDateTime::now_utc(),
        restarts: 2,
        last_error: last_error.map(str::to_owned),
    }
}

#[test]
fn model_state_verdicts() {
    assert_eq!(
        model_check("m", &status(ModelState::Ready, None)).status,
        CheckStatus::Ok
    );
    assert_eq!(
        model_check("m", &status(ModelState::Stopped, None)).status,
        CheckStatus::Ok
    );
    let refused = model_check("m", &status(ModelState::Stopped, Some("not enough commit")));
    assert_eq!(refused.status, CheckStatus::Warn);
    assert!(refused.detail.contains("not enough commit"));
    assert_eq!(
        model_check("m", &status(ModelState::Restarting, None)).status,
        CheckStatus::Warn
    );
    assert_eq!(
        model_check("m", &status(ModelState::Failed, Some("x"))).status,
        CheckStatus::Fail
    );
}

#[test]
#[ignore = "runs the real llama-server binaries and hashes the real models"]
fn real_artifact_checks() {
    let cfg = nebula_config::NebulaConfig::from_toml(None).unwrap();
    for c in nebula_daemon::checks::runtime_checks(&cfg) {
        println!("{:?} {}: {}", c.status, c.name, c.detail);
        assert_eq!(c.status, CheckStatus::Ok);
    }
    let cache = cfg.paths.state.join("model-hashes.json");
    for c in nebula_daemon::checks::model_hash_checks(&cfg, &["standard", "embedding"], &cache) {
        println!("{:?} {}: {}", c.status, c.name, c.detail);
        assert_eq!(c.status, CheckStatus::Ok);
    }
}
