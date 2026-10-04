//! Doctor checks only the daemon can run: model state, the llama-server version against
//! `config/runtime.lock.toml`, and model hashes against `config/models.lock.toml`.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use nebula_config::NebulaConfig;
use nebula_proto::{CheckStatus, DoctorCheck, ModelState, ModelStatus};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Pinned runtimes.
pub const RUNTIME_LOCK: &str = include_str!("../../../config/runtime.lock.toml");
/// Pinned model files.
pub const MODELS_LOCK: &str = include_str!("../../../config/models.lock.toml");

fn check(name: impl Into<String>, status: CheckStatus, detail: impl Into<String>) -> DoctorCheck {
    DoctorCheck {
        name: name.into(),
        status,
        detail: detail.into(),
    }
}

/// Verdict on a model server's status.
#[must_use]
pub fn model_check(name: &str, s: &ModelStatus) -> DoctorCheck {
    let status = match s.state {
        ModelState::Ready | ModelState::Busy => CheckStatus::Ok,
        ModelState::Failed => CheckStatus::Fail,
        ModelState::Stopped if s.last_error.is_none() => CheckStatus::Ok,
        ModelState::Starting
        | ModelState::Restarting
        | ModelState::Stopped
        | ModelState::Unloaded => CheckStatus::Warn,
    };
    let mut detail = format!(
        "{} is {:?} since {}, {} restart(s)",
        s.profile,
        s.state,
        s.since
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default(),
        s.restarts
    );
    if let Some(e) = &s.last_error {
        detail.push_str("; last error: ");
        detail.push_str(e);
    }
    check(name, status, detail)
}

/// `(build, commit prefix)` from `llama-server --version` output.
#[must_use]
pub fn parse_version(output: &str) -> Option<(u64, String)> {
    let re = regex::Regex::new(r"build (\d+), commit ([0-9a-fA-F]+)").ok()?;
    let c = re.captures(output)?;
    Some((c[1].parse().ok()?, c[2].to_ascii_lowercase()))
}

/// Compares `--version` output with a `runtime.lock.toml` section.
#[must_use]
pub fn judge_version(name: &str, output: &str, lock: Option<&toml::Table>) -> DoctorCheck {
    let check_name = format!("runtime.{name}");
    let Some((build, commit)) = parse_version(output) else {
        return check(
            check_name,
            CheckStatus::Warn,
            format!("unrecognized --version output: {}", output.trim()),
        );
    };
    let Some(lock) = lock else {
        return check(
            check_name,
            CheckStatus::Warn,
            format!("build {build} ({commit}) is not in runtime.lock.toml"),
        );
    };
    let want_build = lock.get("build").and_then(toml::Value::as_integer);
    let want_commit = lock
        .get("commit")
        .and_then(toml::Value::as_str)
        .unwrap_or_default();
    let build_ok = want_build.and_then(|b| u64::try_from(b).ok()) == Some(build);
    let commit_ok = !commit.is_empty() && want_commit.to_ascii_lowercase().starts_with(&commit);
    if build_ok && commit_ok {
        check(
            check_name,
            CheckStatus::Ok,
            format!("build {build} ({commit}) matches runtime.lock.toml"),
        )
    } else {
        check(
            check_name,
            CheckStatus::Warn,
            format!(
                "build {build} ({commit}) but runtime.lock.toml pins build {} ({})",
                want_build.unwrap_or_default(),
                want_commit.get(..9).unwrap_or(want_commit)
            ),
        )
    }
}

fn run_version(exe: &Path) -> Result<String, String> {
    use std::process::{Command, Stdio};
    let mut cmd = Command::new(exe);
    cmd.arg("--version").stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let out = cmd.output().map_err(|e| e.to_string())?;
    // llama.cpp prints the version on stderr.
    Ok(format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    ))
}

/// One check per configured runtime.
#[must_use]
pub fn runtime_checks(cfg: &NebulaConfig) -> Vec<DoctorCheck> {
    let lock: toml::Table = RUNTIME_LOCK.parse().unwrap_or_default();
    cfg.model
        .runtimes
        .iter()
        .map(|(name, exe)| {
            if !exe.exists() {
                return check(
                    format!("runtime.{name}"),
                    CheckStatus::Fail,
                    format!("{} is missing", exe.display()),
                );
            }
            match run_version(exe) {
                Ok(out) => {
                    judge_version(name, &out, lock.get(name).and_then(toml::Value::as_table))
                }
                Err(e) => check(
                    format!("runtime.{name}"),
                    CheckStatus::Fail,
                    format!("{} --version: {e}", exe.display()),
                ),
            }
        })
        .collect()
}

/// Pinned `(sha256, bytes)` by lowercase full path, from `models.lock.toml`.
#[must_use]
pub fn pinned_models(lock: &toml::Table) -> BTreeMap<String, (String, u64)> {
    let mut out = BTreeMap::new();
    for section in lock.values().filter_map(toml::Value::as_table) {
        let Some(dir) = section.get("dir").and_then(toml::Value::as_str) else {
            continue;
        };
        let Some(files) = section.get("files").and_then(toml::Value::as_table) else {
            continue;
        };
        for (name, f) in files {
            let sha = f.get("sha256").and_then(toml::Value::as_str);
            let bytes = f
                .get("bytes")
                .and_then(toml::Value::as_integer)
                .and_then(|b| u64::try_from(b).ok());
            if let (Some(sha), Some(bytes)) = (sha, bytes) {
                let path = Path::new(dir).join(name);
                out.insert(
                    path.to_string_lossy().to_lowercase(),
                    (sha.to_ascii_lowercase(), bytes),
                );
            }
        }
    }
    out
}

/// Hash cache entry, keyed by path; valid while size and mtime match.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cached {
    /// File size.
    pub bytes: u64,
    /// Modification time, nanoseconds since the Unix epoch.
    pub mtime_ns: u64,
    /// SHA-256, lowercase hex.
    pub sha256: String,
}

/// `state\model-hashes.json`.
pub type HashCache = BTreeMap<String, Cached>;

fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// SHA-256 of `path`, from `cache` if size and mtime are unchanged (otherwise hashed and
/// cached).
///
/// # Errors
/// If the file can't be read.
pub fn hash_cached(path: &Path, cache: &mut HashCache) -> std::io::Result<(String, u64, bool)> {
    let meta = std::fs::metadata(path)?;
    let mtime_ns = meta
        .modified()?
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_nanos();
    let mtime_ns = u64::try_from(mtime_ns).unwrap_or(u64::MAX);
    let key = path.to_string_lossy().to_lowercase();
    if let Some(c) = cache
        .get(&key)
        .filter(|c| c.bytes == meta.len() && c.mtime_ns == mtime_ns)
    {
        return Ok((c.sha256.clone(), c.bytes, true));
    }
    let sha256 = sha256_file(path)?;
    cache.insert(
        key,
        Cached {
            bytes: meta.len(),
            mtime_ns,
            sha256: sha256.clone(),
        },
    );
    Ok((sha256, meta.len(), false))
}

/// Hash checks for the given profiles' model files.
#[must_use]
pub fn model_hash_checks(
    cfg: &NebulaConfig,
    profiles: &[&str],
    cache_file: &Path,
) -> Vec<DoctorCheck> {
    let lock: toml::Table = MODELS_LOCK.parse().unwrap_or_default();
    let pinned = pinned_models(&lock);
    let mut cache: HashCache = std::fs::read_to_string(cache_file)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    let mut seen: Vec<PathBuf> = Vec::new();
    let mut out = Vec::new();
    for name in profiles {
        let Some(p) = cfg.model.profiles.get(*name) else {
            continue;
        };
        if seen.contains(&p.model) {
            continue;
        }
        seen.push(p.model.clone());
        let check_name = format!("model.hash.{name}");
        let file = p
            .model
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default();
        let Some((want_sha, want_bytes)) = pinned.get(&p.model.to_string_lossy().to_lowercase())
        else {
            out.push(check(
                check_name,
                CheckStatus::Warn,
                format!("{file} is not in models.lock.toml"),
            ));
            continue;
        };
        match hash_cached(&p.model, &mut cache) {
            Err(e) => out.push(check(
                check_name,
                CheckStatus::Fail,
                format!("{}: {e}", p.model.display()),
            )),
            Ok((sha, bytes, cached)) => {
                let how = if cached { "cached" } else { "hashed now" };
                if &sha == want_sha && bytes == *want_bytes {
                    out.push(check(
                        check_name,
                        CheckStatus::Ok,
                        format!("{file} matches models.lock.toml ({how})"),
                    ));
                } else {
                    out.push(check(
                        check_name,
                        CheckStatus::Fail,
                        format!(
                            "{file}: sha256 {} ({bytes} bytes), pinned {} ({want_bytes} bytes)",
                            &sha[..12.min(sha.len())],
                            &want_sha[..12.min(want_sha.len())]
                        ),
                    ));
                }
            }
        }
    }
    let write = || -> std::io::Result<()> {
        if let Some(d) = cache_file.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(
            cache_file,
            serde_json::to_vec_pretty(&cache).map_err(std::io::Error::other)?,
        )
    };
    if let Err(e) = write() {
        tracing::warn!(event = "doctor.hash_cache_unwritable", path = %cache_file.display(), error = %e);
    }
    out
}
