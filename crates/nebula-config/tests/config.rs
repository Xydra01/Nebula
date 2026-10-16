#![allow(clippy::unwrap_used, clippy::panic, missing_docs)]

use std::path::PathBuf;

use nebula_config::{ConfigError, NebulaConfig};

#[test]
fn defaults_load_and_validate() {
    let cfg = NebulaConfig::from_toml(None).unwrap();
    assert_eq!(cfg.paths.data_root, PathBuf::from(r"F:\Nebula"));
    assert_eq!(
        cfg.telemetry.log_dir,
        Some(PathBuf::from(r"F:\Nebula\logs"))
    );
    assert_eq!(cfg.daemon.pipe_path(), r"\\.\pipe\nebula");
    assert_eq!(cfg.daemon.load_on_start, "standard");
    assert_eq!(cfg.resources.volumes.len(), 2);
    assert_eq!(cfg.resources.volumes[0].pause_gb, Some(12));
    assert_eq!(
        cfg.model.profile("standard").unwrap().commit_estimate_mib,
        Some(10700)
    );
    assert_eq!(cfg.sandbox.rules_table_path, "");
    assert!(cfg.paths_on_retired_drive().is_empty());
}

#[test]
fn override_merges_tables_and_replaces_values() {
    let cfg = NebulaConfig::from_toml(Some(
        r#"
        [daemon]
        load_on_start = ""
        [resources]
        commit_margin_mib = 2048
        [model.profiles.standard]
        ctx = 16384
        "#,
    ))
    .unwrap();
    assert_eq!(cfg.daemon.load_on_start, "");
    assert_eq!(cfg.daemon.pipe_name, "nebula");
    assert_eq!(cfg.resources.commit_margin_mib, 2048);
    assert_eq!(cfg.resources.log_interval_s, 30);
    let standard = cfg.model.profile("standard").unwrap();
    assert_eq!(standard.ctx, 16384);
    assert_eq!(standard.kv_type, "q4_0");
}

#[test]
fn unknown_keys_are_rejected() {
    let err = NebulaConfig::from_toml(Some("[daemon]\npipe_nmae = 'x'\n")).unwrap_err();
    assert!(matches!(err, ConfigError::Parse { .. }), "{err}");
}

#[test]
fn unknown_sandbox_key_is_rejected() {
    // `SandboxConfig` is `#[serde(deny_unknown_fields)]`, so an unknown key under `[sandbox]`
    // trips the parse rather than being silently ignored (Req 1.1, config convention).
    let err = NebulaConfig::from_toml(Some("[sandbox]\nbogus_key = true\n")).unwrap_err();
    assert!(matches!(&err, ConfigError::Parse { .. }), "{err}");
}

#[test]
fn sandbox_rules_table_path_override_round_trips() {
    // A non-empty override replaces the embedded-table sentinel verbatim; the resolver (not
    // this config layer) decides whether the empty string means "use the embedded table".
    // A TOML single-quoted literal string does not process escapes, so the backslashes in the
    // path survive verbatim into the parsed value.
    let cfg = NebulaConfig::from_toml(Some(
        "[sandbox]\nrules_table_path = 'F:\\Nebula\\rules.toml'\n",
    ))
    .unwrap();
    assert_eq!(cfg.sandbox.rules_table_path, r"F:\Nebula\rules.toml");
}

#[test]
fn paths_on_the_retired_drive_are_rejected() {
    let err = NebulaConfig::from_toml(Some("[paths]\nstate = 'c:\\Nebula\\state'\n")).unwrap_err();
    assert!(
        matches!(&err, ConfigError::Invalid(m) if m.contains("paths.state")),
        "{err}"
    );
    let err = NebulaConfig::from_toml(Some(
        "[model.runtimes]\nllama-prism = 'C:\\llama\\llama-server.exe'\n",
    ))
    .unwrap_err();
    assert!(matches!(err, ConfigError::Invalid(_)), "{err}");
}

#[test]
fn unknown_startup_profile_is_rejected() {
    let err = NebulaConfig::from_toml(Some("[daemon]\nload_on_start = 'quality'\n")).unwrap_err();
    assert!(matches!(err, ConfigError::Invalid(_)), "{err}");
}

#[test]
fn missing_override_file_means_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = NebulaConfig::load_from(&dir.path().join("absent.toml")).unwrap();
    assert_eq!(cfg, NebulaConfig::from_toml(None).unwrap());
    let path = dir.path().join("nebula.toml");
    std::fs::write(&path, "[telemetry]\nfilter = 'debug'\n").unwrap();
    assert_eq!(
        NebulaConfig::load_from(&path).unwrap().telemetry.filter,
        "debug"
    );
}

#[test]
fn all_paths_includes_the_builtin_worktree_root() {
    let cfg = NebulaConfig::from_toml(None).unwrap();
    let entry = cfg
        .all_paths()
        .into_iter()
        .find(|(label, _)| label == "tools.builtin.worktree_root")
        .expect("all_paths should label the built-in worktree root");
    assert_eq!(entry.1, cfg.tools.builtin.worktree_root);
    assert_eq!(entry.1, PathBuf::from("."));
}

#[test]
fn all_paths_includes_the_worktrees_dir() {
    // The retired-drive doctor check walks `all_paths()`, so the per-task `worktrees_dir` must be
    // listed there (git-worktree-per-task task 1.2; Req 8.3).
    let cfg = NebulaConfig::from_toml(None).unwrap();
    let entry = cfg
        .all_paths()
        .into_iter()
        .find(|(label, _)| label == "worktree.worktrees_dir")
        .expect("all_paths should label the worktrees dir");
    assert_eq!(entry.1, cfg.worktree.worktrees_dir);
    assert_eq!(entry.1, PathBuf::from(r"F:\Nebula\worktrees"));
}

#[test]
fn executor_defaults_load_and_validate() {
    let cfg = NebulaConfig::from_toml(None).unwrap();
    assert_eq!(cfg.executor.max_tool_calls, 15);
    assert_eq!(cfg.executor.max_tokens, 40960);
    assert_eq!(cfg.executor.wall_time_secs, 600);
    assert_eq!(cfg.executor.default_profile, "");
}

#[test]
fn executor_overrides_merge_and_validate() {
    let cfg = NebulaConfig::from_toml(Some(
        r#"
        [executor]
        max_tool_calls = 25
        max_tokens = 20000
        wall_time_secs = 300
        default_profile = "standard"
        "#,
    ))
    .unwrap();
    assert_eq!(cfg.executor.max_tool_calls, 25);
    assert_eq!(cfg.executor.max_tokens, 20000);
    assert_eq!(cfg.executor.wall_time_secs, 300);
    assert_eq!(cfg.executor.default_profile, "standard");
}

#[test]
fn unknown_executor_key_is_rejected() {
    let err = NebulaConfig::from_toml(Some("[executor]\nbogus_limit = 10\n")).unwrap_err();
    assert!(matches!(err, ConfigError::Parse { .. }), "{err}");
}

#[test]
fn zero_executor_limits_are_rejected() {
    let err = NebulaConfig::from_toml(Some("[executor]\nmax_tool_calls = 0\n")).unwrap_err();
    assert!(
        matches!(&err, ConfigError::Invalid(m) if m.contains("executor.max_tool_calls")),
        "{err}"
    );

    let err = NebulaConfig::from_toml(Some("[executor]\nmax_tokens = 0\n")).unwrap_err();
    assert!(
        matches!(&err, ConfigError::Invalid(m) if m.contains("executor.max_tokens")),
        "{err}"
    );

    let err = NebulaConfig::from_toml(Some("[executor]\nwall_time_secs = 0\n")).unwrap_err();
    assert!(
        matches!(&err, ConfigError::Invalid(m) if m.contains("executor.wall_time_secs")),
        "{err}"
    );
}

#[test]
fn unknown_executor_default_profile_is_rejected() {
    let err =
        NebulaConfig::from_toml(Some("[executor]\ndefault_profile = 'nonexistent'\n")).unwrap_err();
    assert!(
        matches!(&err, ConfigError::Invalid(m) if m.contains("executor.default_profile")),
        "{err}"
    );
}

#[test]
fn github_defaults_load_and_validate() {
    let cfg = NebulaConfig::from_toml(None).unwrap();
    assert!(!cfg.github.enabled);
    assert_eq!(cfg.github.repo, "Xydra01/Nebula");
    assert_eq!(cfg.github.base_branch, "main");
    assert_eq!(cfg.github.branch_prefix, "nebula/");
    assert_eq!(cfg.github.token_target, "nebula/github_bot_token");
    assert_eq!(cfg.github.bot_username, "Nebula-dev-bot");
}

#[test]
fn github_overrides_merge_and_validate() {
    let toml = r#"
[github]
enabled = true
repo = "custom-owner/custom-repo"
base_branch = "develop"
branch_prefix = "task/"
bot_username = "CustomBot"
"#;
    let cfg = NebulaConfig::from_toml(Some(toml)).unwrap();
    assert!(cfg.github.enabled);
    assert_eq!(cfg.github.repo, "custom-owner/custom-repo");
    assert_eq!(cfg.github.base_branch, "develop");
    assert_eq!(cfg.github.branch_prefix, "task/");
    assert_eq!(cfg.github.bot_username, "CustomBot");
}

#[test]
fn unknown_github_key_is_rejected() {
    let err = NebulaConfig::from_toml(Some("[github]\ninvalid_key = true\n")).unwrap_err();
    assert!(matches!(err, ConfigError::Parse { .. }), "{err}");
}
