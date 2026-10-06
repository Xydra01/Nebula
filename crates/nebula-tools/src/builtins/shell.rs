//! The `shell.run` built-in tool (Requirement 4).
//!
//! `shell.run` runs a single command inside the task's confinement root, but only after the
//! command has been classified and (where required) approved. The classification gate runs
//! **before any child process is started** (Requirement 4.2, 4.11): the command and its arguments
//! are submitted to the injected [`CommandClassifier`], which returns a [`Tier`] and an optional
//! [`Approval`]. The decision table is:
//!
//! - `tier <= `[`NO_APPROVAL_THRESHOLD`]` ([`Tier::Sandbox`]) → execute, no approval needed
//!   (Requirement 4.3).
//! - `tier > ` threshold and **no** approval → refuse with [`ToolError::InvalidArguments`],
//!   starting **zero** processes (Requirement 4.4).
//! - `tier > ` threshold **with** an approval → execute (Requirement 4.5).
//!
//! A model second opinion is advisory only and can never lower the enforced tier below the
//! classifier's: the enforced tier is [`effective_tier`]`(classifier_tier, advisory_tier)`
//! (Requirement 4.12). This feature has no second-opinion source wired in, so the advisory tier is
//! the classifier's own tier and `effective_tier` is a no-op; the call site is kept so a future
//! advisory input slots in without changing the gate.
//!
//! Execution (Requirement 4.6–4.10) runs the child inside the shared kill-on-close Job Object via
//! [`spawn_in_job`](crate::launcher::child::spawn_in_job): the child is launched with a
//! [`scrub_env`]-produced [`Scrubbed_Environment`](scrub_env) (secret-bearing variables removed and
//! `PATH` set to a controlled value) and its working directory pinned to the worktree root. The
//! per-call timeout is measured from child start; on elapse the [`JobChild`] guard is dropped,
//! which closes the job handle and terminates the child and all its descendants, and the call
//! returns [`ToolError::Timeout`]. Cumulative output is capped at the host's output cap.

use crate::ToolError;
use crate::builtins::{BuiltinTool, ToolContext, ToolOutput};
use crate::permit::{Approval, NO_APPROVAL_THRESHOLD, Tier, effective_tier};

/// The controlled `PATH` value handed to every `shell.run` child.
///
/// A fixed, minimal system `PATH` (Requirement 4.7): it replaces whatever `PATH` the parent
/// process carried so a child cannot resolve executables from a location the agent could have
/// poisoned, while still finding the standard Windows system utilities.
const CONTROLLED_PATH: &str = r"C:\Windows\System32;C:\Windows";

/// Substrings (compared case-insensitively) that mark an environment variable name as
/// secret-bearing. Any variable whose name contains one of these is removed from the child's
/// environment (Requirement 4.7).
const SECRET_NAME_MARKERS: &[&str] = &["TOKEN", "SECRET", "KEY", "PASSWORD", "API"];

/// Produce a [`Scrubbed_Environment`](scrub_env) from a parent environment.
///
/// This is the pure, testable core of the child-environment rules (Requirement 4.7):
///
/// 1. Every variable whose **name** contains a secret marker (`TOKEN`, `SECRET`, `KEY`,
///    `PASSWORD`, `API`, matched case-insensitively) is dropped — these are the secret-bearing
///    variables that must never reach a child.
/// 2. Any inherited `PATH` (matched case-insensitively, since Windows env names are
///    case-insensitive) is dropped and replaced with the single controlled [`CONTROLLED_PATH`].
///
/// The function is deterministic and performs no I/O: it takes an iterator of `(name, value)`
/// pairs and returns the filtered, PATH-controlled environment. The caller clears the child's
/// inherited environment and sets exactly the returned pairs.
///
/// Note that the `KEY` marker intentionally also removes variables whose names merely contain
/// `KEY` (for example `MONKEY`): over-removal is safe here (the child simply does not see that
/// variable), whereas under-removal could leak a secret, so the rule errs toward dropping.
#[must_use]
pub fn scrub_env<I>(parent_env: I) -> Vec<(String, String)>
where
    I: IntoIterator<Item = (String, String)>,
{
    let is_secret = |name: &str| {
        let upper = name.to_ascii_uppercase();
        SECRET_NAME_MARKERS
            .iter()
            .any(|marker| upper.contains(marker))
    };

    let mut scrubbed: Vec<(String, String)> = parent_env
        .into_iter()
        .filter(|(name, _)| !is_secret(name))
        .filter(|(name, _)| !name.eq_ignore_ascii_case("PATH"))
        .collect();

    // Set the single controlled PATH, replacing any inherited one (dropped above).
    scrubbed.push(("PATH".to_owned(), CONTROLLED_PATH.to_owned()));
    scrubbed
}

/// The `shell.run` built-in tool.
///
/// Classifies and confines every command, refusing above-threshold commands that lack an
/// approval before any process starts (Requirement 4). Executed commands run inside the shared
/// kill-on-close Job Object with a scrubbed environment and the worktree root as the working
/// directory.
#[derive(Clone, Copy, Debug, Default)]
pub struct ShellRun;

impl ShellRun {
    /// The stable tool name used in `tools.list` and `tools.call`.
    pub const NAME: &'static str = "shell.run";
}

/// Parse and validate the `{ command, args? }` arguments.
///
/// Schema validation at the boundary has already guaranteed the shape; this extracts the typed
/// values and defends against an empty command. Returns `(command, args)`.
fn parse_args(arguments: &serde_json::Value) -> Result<(String, Vec<String>), ToolError> {
    let invalid = |detail: String| ToolError::InvalidArguments {
        tool: ShellRun::NAME.to_owned(),
        detail,
    };

    let command = arguments
        .get("command")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| invalid("`command` must be a string".to_owned()))?
        .to_owned();
    if command.trim().is_empty() {
        return Err(invalid("`command` must not be empty".to_owned()));
    }

    let args = match arguments.get("args") {
        None | Some(serde_json::Value::Null) => Vec::new(),
        Some(serde_json::Value::Array(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                let s = item
                    .as_str()
                    .ok_or_else(|| invalid("each entry in `args` must be a string".to_owned()))?;
                out.push(s.to_owned());
            }
            out
        }
        Some(_) => return Err(invalid("`args` must be an array of strings".to_owned())),
    };

    Ok((command, args))
}

/// The outcome of the classification gate: either refuse before any spawn, or proceed to execute.
///
/// Kept as a small enum so the gate is a pure decision over `(tier, approval)` that the execution
/// path and the unit tests can both exercise without touching the operating system.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Decision {
    /// The command may execute (its enforced tier is at or below the threshold, or an approval is
    /// present).
    Execute,
    /// The command is refused; no process may start. Carries the error to return.
    Refuse(ToolError),
}

/// Apply the permission gate (Requirement 4.2–4.5, 4.11, 4.12) to a classifier result.
///
/// `enforced_tier` is [`effective_tier`] of the classifier tier and the advisory tier; it is never
/// below the classifier tier. The gate returns [`Decision::Execute`] when the enforced tier is at
/// or below [`NO_APPROVAL_THRESHOLD`], or when an [`Approval`] is present; otherwise it returns
/// [`Decision::Refuse`] with an [`ToolError::InvalidArguments`] that indicates approval is
/// required.
fn gate(enforced_tier: Tier, approval: Option<&Approval>) -> Decision {
    if enforced_tier <= NO_APPROVAL_THRESHOLD || approval.is_some() {
        Decision::Execute
    } else {
        Decision::Refuse(ToolError::InvalidArguments {
            tool: ShellRun::NAME.to_owned(),
            detail: format!(
                "command classified at tier {enforced_tier:?} (above the no-approval threshold \
                 {NO_APPROVAL_THRESHOLD:?}) requires an approval; none was granted"
            ),
        })
    }
}

#[async_trait::async_trait]
impl BuiltinTool for ShellRun {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn description(&self) -> Option<&str> {
        Some(
            "Run a single command inside the worktree, after command classification and \
             approval, isolated in a Job Object with a scrubbed environment.",
        )
    }

    /// `{ command: string, args?: [string] }`. `additionalProperties: false` rejects any other
    /// field at the boundary before [`call`](Self::call) runs.
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "minLength": 1,
                    "description": "The program to run (resolved against the controlled PATH)."
                },
                "args": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Arguments passed to the command, unquoted."
                }
            },
            "required": ["command"],
            "additionalProperties": false
        })
    }

    /// `shell.run` has no fixed registration tier: the effective tier is decided per call by the
    /// classifier. [`Tier::Workspace`] is reported here as the conservative registration-time
    /// label (above the no-approval threshold), but the enforced tier always comes from the
    /// classifier at call time (Requirement 4.2, 4.11).
    fn tier(&self) -> Tier {
        Tier::Workspace
    }

    /// Classify, gate, and (if permitted) execute the command.
    ///
    /// `arguments` have already passed schema validation. The classification gate runs before any
    /// child process is started (Requirement 4.2, 4.4, 4.11).
    ///
    /// # Errors
    /// [`ToolError::InvalidArguments`] when the command is refused for lacking an approval
    /// (Requirement 4.4) or when the arguments are malformed; [`ToolError::Timeout`] when the
    /// child does not exit within the per-call timeout (Requirement 4.9); [`ToolError::Unavailable`]
    /// when the child cannot be started or waited on.
    async fn call(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let (command, args) = parse_args(&arguments)?;

        // Classify BEFORE starting any child process (Requirement 4.2, 4.11).
        let (classifier_tier, approval) = ctx.classifier.classify(&command, &args);
        // A model second opinion is advisory only and can never lower the tier; with no
        // second-opinion source wired in, the advisory tier is the classifier's own tier so this
        // is a no-op that keeps the Requirement 4.12 contract explicit at the gate.
        let enforced_tier = effective_tier(classifier_tier, classifier_tier);

        match gate(enforced_tier, approval.as_ref()) {
            Decision::Refuse(err) => Err(err),
            Decision::Execute => {
                let worktree_root = ctx.worktree.worktree_root();
                execute(
                    &command,
                    &args,
                    &worktree_root,
                    ctx.limits.call_timeout,
                    ctx.limits.output_cap,
                )
                .await
            }
        }
    }
}

/// Run a classified-and-approved command inside the Job Object (Requirement 4.6–4.10).
///
/// On Windows the child is spawned through [`spawn_in_job`](crate::launcher::child::spawn_in_job),
/// launched with the [`scrub_env`]-produced environment and `current_dir = worktree_root`, waited
/// on with the per-call timeout, and its combined output capped. Dropping the returned guard on
/// timeout (or on any early return) closes the job handle and kills the child and its descendants.
#[cfg(windows)]
async fn execute(
    command: &str,
    args: &[String],
    worktree_root: &std::path::Path,
    timeout: std::time::Duration,
    output_cap: usize,
) -> Result<ToolOutput, ToolError> {
    use std::process::Stdio;

    use tokio::process::Command;

    use crate::launcher::child::spawn_in_job;

    let unavailable = |detail: String| ToolError::Unavailable {
        server: "builtin".to_owned(),
        detail,
    };

    let mut cmd = Command::new(command);
    cmd.args(args)
        .current_dir(worktree_root)
        .env_clear()
        .envs(scrub_env(std::env::vars()))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // Spawning assigns the child to a fresh kill-on-close Job Object (Requirement 4.6). The guard
    // is held across the wait below so a cancelled/timed-out call drops it and kills the child.
    let mut job_child = spawn_in_job(&mut cmd)
        .map_err(|e| unavailable(format!("failed to start {command:?}: {e}")))?;

    // Enforce the per-call timeout from child start (Requirement 4.8). `wait_with_timeout` returns
    // `Ok(None)` when the deadline elapses; we then drop `job_child`, which kills the child and all
    // descendants via the job (Requirement 4.9).
    match job_child.wait_with_timeout(timeout).await {
        Ok(Some(output)) => {
            drop(job_child);
            Ok(combine_output(&output, output_cap))
        }
        Ok(None) => {
            drop(job_child);
            Err(ToolError::Timeout {
                tool: ShellRun::NAME.to_owned(),
                timeout_ms: u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
            })
        }
        Err(e) => {
            drop(job_child);
            Err(unavailable(format!("failed waiting on {command:?}: {e}")))
        }
    }
}

/// Non-Windows fallback so the crate compiles off Windows.
///
/// The Job Object isolation is Windows-only (design 7.3); `shell.run` is a Windows-target tool, so
/// on other platforms it reports [`ToolError::Unavailable`] rather than running an unisolated
/// child.
#[cfg(not(windows))]
async fn execute(
    command: &str,
    _args: &[String],
    _worktree_root: &std::path::Path,
    _timeout: std::time::Duration,
    _output_cap: usize,
) -> Result<ToolOutput, ToolError> {
    Err(ToolError::Unavailable {
        server: "builtin".to_owned(),
        detail: format!("shell.run is only available on Windows; cannot run {command:?}"),
    })
}

/// Combine a finished child's stdout and stderr into a single capped text output.
///
/// stdout is emitted first, then stderr (prefixed when non-empty). The cumulative output is capped
/// to `output_cap` bytes here as a first line of defense (Requirement 4.10); the host's
/// `enforce_cap` applies the authoritative truncation-and-blob handling (Requirement 5) on the
/// returned [`ToolOutput`]. `is_error` reflects a non-success exit status.
#[cfg(windows)]
fn combine_output(output: &std::process::Output, output_cap: usize) -> ToolOutput {
    let mut bytes = output.stdout.clone();
    if !output.stderr.is_empty() {
        if !bytes.is_empty() {
            bytes.push(b'\n');
        }
        bytes.extend_from_slice(&output.stderr);
    }

    // Cap the cumulative bytes on a UTF-8 boundary so the first-pass truncation never splits a
    // code point; the host boundary re-measures and applies the marker + blob.
    if bytes.len() > output_cap {
        let mut end = output_cap;
        while end > 0 && std::str::from_utf8(&bytes[..end]).is_err() {
            end -= 1;
        }
        bytes.truncate(end);
    }

    ToolOutput {
        bytes,
        is_error: !output.status.success(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use super::*;
    use crate::builtins::{BuiltinLimits, ResourceProvider, WorktreeRootProvider};
    use crate::permit::{Approval, CommandClassifier, DefaultClassifier, Tier};

    /// A worktree provider pinned to a fixed root.
    struct FixedWorktree(std::path::PathBuf);

    impl WorktreeRootProvider for FixedWorktree {
        fn worktree_root(&self) -> std::path::PathBuf {
            self.0.clone()
        }
    }

    /// A resource provider that is never consulted by `shell.run`.
    struct UnusedResources;

    impl ResourceProvider for UnusedResources {
        fn latest(&self) -> Option<nebula_proto::ResourceSnapshot> {
            None
        }
    }

    /// A classifier returning a fixed `(tier, approval)` for every command, recording how many
    /// times it was asked. Used to assert the gate consults the classifier exactly once per call.
    struct FixedClassifier {
        tier: Tier,
        approval: Option<Approval>,
        calls: AtomicUsize,
    }

    impl FixedClassifier {
        fn new(tier: Tier, approval: Option<Approval>) -> Self {
            Self {
                tier,
                approval,
                calls: AtomicUsize::new(0),
            }
        }
    }

    impl CommandClassifier for FixedClassifier {
        fn classify(&self, _command: &str, _args: &[String]) -> (Tier, Option<Approval>) {
            self.calls.fetch_add(1, Ordering::SeqCst);
            (self.tier, self.approval.clone())
        }
    }

    fn ctx_with(classifier: Arc<dyn CommandClassifier>) -> ToolContext {
        ToolContext {
            worktree: Arc::new(FixedWorktree(std::env::temp_dir())),
            classifier,
            resources: Arc::new(UnusedResources),
            retired_drive: "C:".to_owned(),
            limits: BuiltinLimits {
                call_timeout: Duration::from_secs(30),
                output_cap: 65_536,
            },
        }
    }

    #[test]
    fn metadata_names_shell_run_with_command_schema() {
        let tool = ShellRun;
        assert_eq!(tool.name(), "shell.run");
        let schema = tool.input_schema();
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["required"][0], "command");
        assert_eq!(schema["additionalProperties"], false);
        assert!(tool.description().is_some());
    }

    #[test]
    fn parse_args_extracts_command_and_args() {
        let (cmd, args) =
            parse_args(&serde_json::json!({ "command": "echo", "args": ["a", "b"] })).unwrap();
        assert_eq!(cmd, "echo");
        assert_eq!(args, vec!["a".to_owned(), "b".to_owned()]);
    }

    #[test]
    fn parse_args_defaults_missing_args_to_empty() {
        let (cmd, args) = parse_args(&serde_json::json!({ "command": "ls" })).unwrap();
        assert_eq!(cmd, "ls");
        assert!(args.is_empty());
    }

    #[test]
    fn parse_args_rejects_empty_command() {
        let err = parse_args(&serde_json::json!({ "command": "   " })).unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments { .. }));
    }

    #[test]
    fn parse_args_rejects_non_string_arg() {
        let err = parse_args(&serde_json::json!({ "command": "echo", "args": [1] })).unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments { .. }));
    }

    #[test]
    fn gate_executes_at_or_below_threshold_without_approval() {
        assert_eq!(gate(Tier::Read, None), Decision::Execute);
        assert_eq!(gate(Tier::Sandbox, None), Decision::Execute);
    }

    #[test]
    fn gate_refuses_above_threshold_without_approval() {
        for tier in [Tier::Workspace, Tier::System] {
            match gate(tier, None) {
                Decision::Refuse(ToolError::InvalidArguments { tool, detail }) => {
                    assert_eq!(tool, "shell.run");
                    assert!(
                        detail.contains("approval"),
                        "detail should mention approval"
                    );
                }
                other => panic!("expected refusal for {tier:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn gate_executes_above_threshold_with_approval() {
        let approval = Approval::new();
        assert_eq!(gate(Tier::Workspace, Some(&approval)), Decision::Execute);
        assert_eq!(gate(Tier::System, Some(&approval)), Decision::Execute);
    }

    #[tokio::test]
    async fn above_threshold_without_approval_is_refused() {
        // A Tier::System classifier with no approval must refuse before any spawn.
        let classifier = Arc::new(FixedClassifier::new(Tier::System, None));
        let ctx = ctx_with(classifier.clone());
        let err = ShellRun
            .call(serde_json::json!({ "command": "whatever" }), &ctx)
            .await
            .expect_err("tier-3 without approval must be refused");
        match err {
            ToolError::InvalidArguments { tool, detail } => {
                assert_eq!(tool, "shell.run");
                assert!(detail.contains("approval"));
            }
            other => panic!("expected InvalidArguments, got {other:?}"),
        }
        // The classifier was consulted exactly once (before any spawn).
        assert_eq!(classifier.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn scrub_env_removes_secret_bearing_names() {
        let parent = vec![
            ("GITHUB_TOKEN".to_owned(), "ghp_x".to_owned()),
            ("MY_SECRET".to_owned(), "s".to_owned()),
            ("AWS_ACCESS_KEY_ID".to_owned(), "k".to_owned()),
            ("DB_PASSWORD".to_owned(), "p".to_owned()),
            ("OPENAI_API_BASE".to_owned(), "u".to_owned()),
            ("HOME".to_owned(), "/home/user".to_owned()),
            ("LANG".to_owned(), "en_US.UTF-8".to_owned()),
        ];
        let scrubbed = scrub_env(parent);
        let names: Vec<&str> = scrubbed.iter().map(|(k, _)| k.as_str()).collect();

        for secret in [
            "GITHUB_TOKEN",
            "MY_SECRET",
            "AWS_ACCESS_KEY_ID",
            "DB_PASSWORD",
            "OPENAI_API_BASE",
        ] {
            assert!(!names.contains(&secret), "{secret} must be removed");
        }
        assert!(names.contains(&"HOME"));
        assert!(names.contains(&"LANG"));
    }

    #[test]
    fn scrub_env_sets_controlled_path_and_drops_inherited() {
        let parent = vec![
            ("PATH".to_owned(), r"C:\evil;C:\more".to_owned()),
            ("USERPROFILE".to_owned(), r"C:\Users\x".to_owned()),
        ];
        let scrubbed = scrub_env(parent);
        let path_entries: Vec<&(String, String)> = scrubbed
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case("PATH"))
            .collect();
        assert_eq!(path_entries.len(), 1, "exactly one PATH entry");
        assert_eq!(path_entries[0].1, CONTROLLED_PATH);
    }

    #[test]
    fn scrub_env_path_match_is_case_insensitive() {
        let parent = vec![("Path".to_owned(), r"C:\evil".to_owned())];
        let scrubbed = scrub_env(parent);
        // Only the single controlled PATH remains; the inherited lower/mixed-case one is gone.
        assert_eq!(scrubbed.len(), 1);
        assert_eq!(scrubbed[0].1, CONTROLLED_PATH);
    }

    #[test]
    fn scrub_env_is_pure_and_deterministic() {
        let parent = vec![
            ("FOO".to_owned(), "1".to_owned()),
            ("TOKEN_X".to_owned(), "2".to_owned()),
        ];
        let a = scrub_env(parent.clone());
        let b = scrub_env(parent);
        assert_eq!(a, b);
    }

    // Feature: builtin-tools, Property 7: the scrubbed environment removes secrets and pins PATH
    // and cwd.
    //
    // Validates: Requirements 4.7. For an arbitrary parent environment, `scrub_env` must (1) drop
    // every secret-bearing variable (any name containing TOKEN/SECRET/KEY/PASSWORD/API,
    // case-insensitively), (2) carry exactly one PATH entry whose value is the controlled
    // `CONTROLLED_PATH` (replacing any inherited PATH in any letter case), and (3) preserve every
    // benign variable (neither secret-bearing nor a PATH alias) unchanged.
    //
    // The cwd-is-the-worktree-root half of Requirement 4.7 is enforced by `execute` via
    // `current_dir(worktree_root)` and is asserted in the shell execution unit tests (task 9.5);
    // this property covers the pure environment scrubbing and PATH pinning.
    mod property_scrubbed_env_removes_secrets_and_pins_path {
        use proptest::prelude::*;

        use super::*;

        /// True when `name` contains a secret marker (case-insensitive), mirroring `scrub_env`'s
        /// own rule so the oracle is independent of ordering/implementation details.
        fn name_is_secret(name: &str) -> bool {
            let upper = name.to_ascii_uppercase();
            SECRET_NAME_MARKERS
                .iter()
                .any(|marker| upper.contains(marker))
        }

        /// A benign variable name: non-empty, no `=`, not a PATH alias, and not secret-bearing.
        /// The `[A-Za-z]` lead plus `[A-Za-z0-9_]*` tail keeps names realistic while
        /// `prop_filter` removes the (rare) PATH/secret collisions so this strategy only yields
        /// variables that must survive scrubbing.
        fn benign_name() -> impl Strategy<Value = String> {
            "[A-Za-z][A-Za-z0-9_]{0,15}"
                .prop_filter("benign names must not be PATH or secret-bearing", |n| {
                    !n.eq_ignore_ascii_case("PATH") && !name_is_secret(n)
                })
        }

        /// A name that is guaranteed secret-bearing: a marker (in one of several letter cases)
        /// embedded between arbitrary benign-ish affixes, exercising the case-insensitive
        /// substring match on known names and secret-like patterns alike.
        fn secret_name() -> impl Strategy<Value = String> {
            let markers = prop_oneof![
                Just("TOKEN"),
                Just("SECRET"),
                Just("KEY"),
                Just("PASSWORD"),
                Just("API"),
            ];
            let case = prop_oneof![
                Just(0u8), // as-is (upper)
                Just(1u8), // lower
                Just(2u8), // mixed
            ];
            ("[A-Za-z_]{0,8}", markers, case, "[A-Za-z0-9_]{0,8}").prop_map(
                |(prefix, marker, case, suffix)| {
                    let marker = match case {
                        1 => marker.to_ascii_lowercase(),
                        2 => {
                            // Flip alternate characters to force a mixed case.
                            marker
                                .chars()
                                .enumerate()
                                .map(|(i, c)| {
                                    if i % 2 == 0 {
                                        c.to_ascii_lowercase()
                                    } else {
                                        c.to_ascii_uppercase()
                                    }
                                })
                                .collect()
                        }
                        _ => marker.to_owned(),
                    };
                    format!("{prefix}{marker}{suffix}")
                },
            )
        }

        /// Any PATH alias name in varied letter case (Windows env names are case-insensitive).
        fn path_name() -> impl Strategy<Value = String> {
            prop_oneof![
                Just("PATH".to_owned()),
                Just("Path".to_owned()),
                Just("path".to_owned()),
                Just("pAtH".to_owned()),
            ]
        }

        /// Arbitrary variable values; `[^=]` avoids nothing meaningful here but keeps values from
        /// containing embedded NULs that proptest shrinking dislikes.
        fn any_value() -> impl Strategy<Value = String> {
            ".{0,24}".prop_map(|s| s.replace('\0', ""))
        }

        /// A single parent entry: benign, secret-bearing, or a PATH alias, each with a value.
        fn entry() -> impl Strategy<Value = (String, String)> {
            prop_oneof![
                (benign_name(), any_value()),
                (secret_name(), any_value()),
                (path_name(), any_value()),
            ]
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(256))]

            #[test]
            fn scrubbed_env_drops_secrets_pins_path_and_keeps_benign(
                parent in proptest::collection::vec(entry(), 0..24),
            ) {
                let scrubbed = scrub_env(parent.clone());

                // (1) No surviving variable name is secret-bearing.
                for (name, _) in &scrubbed {
                    prop_assert!(
                        !name_is_secret(name),
                        "secret-bearing variable {name:?} survived scrubbing",
                    );
                }

                // (2) Exactly one PATH entry (case-insensitive) and its value is the controlled
                // PATH, regardless of any inherited PATH value or letter case.
                let path_entries: Vec<&(String, String)> = scrubbed
                    .iter()
                    .filter(|(name, _)| name.eq_ignore_ascii_case("PATH"))
                    .collect();
                prop_assert_eq!(
                    path_entries.len(),
                    1,
                    "expected exactly one PATH entry, got {}",
                    path_entries.len(),
                );
                prop_assert_eq!(&path_entries[0].1, CONTROLLED_PATH);

                // (3) Every benign (non-secret, non-PATH) parent variable is preserved
                // name-and-value. Build the surviving set once for membership checks.
                let survived: std::collections::HashSet<(&str, &str)> = scrubbed
                    .iter()
                    .map(|(n, v)| (n.as_str(), v.as_str()))
                    .collect();
                for (name, value) in &parent {
                    if !name_is_secret(name) && !name.eq_ignore_ascii_case("PATH") {
                        prop_assert!(
                            survived.contains(&(name.as_str(), value.as_str())),
                            "benign variable {name:?}={value:?} was not preserved",
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn default_classifier_tier_system_command_refuses_through_call_path() {
        // Sanity: with the real DefaultClassifier, `git push` is Tier::System and no approval, so
        // the gate refuses it.
        let (tier, approval) = DefaultClassifier.classify("git", &["push".to_owned()]);
        assert_eq!(tier, Tier::System);
        assert!(approval.is_none());
        assert!(matches!(
            gate(effective_tier(tier, tier), approval.as_ref()),
            Decision::Refuse(_)
        ));
    }

    // Feature: builtin-tools, Property 4: above-threshold command without approval never starts a process
    //
    // Property 4: An above-threshold command without approval never starts a process. For any
    // command and arguments, when the classifier reports a tier strictly above
    // `NO_APPROVAL_THRESHOLD` (`Tier::Workspace` or `Tier::System`) and grants no approval,
    // `ShellRun::call` returns `ToolError::InvalidArguments` and no child process is ever started.
    //
    // Proving "zero spawns" without refactoring the launcher: `execute()` runs the child with
    // `current_dir = worktree_root`, so a spawned process is the only thing that could create a
    // file there. Each case runs against a fresh, empty tempdir worktree and uses a generated
    // command whose argument list *would* write a uniquely named sentinel into that worktree if it
    // ever ran. After the refused call we assert (a) the error is `InvalidArguments`, (b) the
    // classifier was consulted exactly once (the gate ran), and (c) the worktree is still empty —
    // the sentinel does not exist — which can only hold if `execute()` was never reached and no
    // process started. See design.md, Property 4 (Validates: Requirements 4.2, 4.4, 4.11).
    mod property_above_threshold_never_spawns {
        use super::*;
        use proptest::prelude::*;
        use tempfile::TempDir;

        /// An above-threshold tier (strictly greater than [`NO_APPROVAL_THRESHOLD`]): the only two
        /// tiers that require an approval are [`Tier::Workspace`] and [`Tier::System`].
        fn above_threshold_tier() -> impl Strategy<Value = Tier> {
            prop_oneof![Just(Tier::Workspace), Just(Tier::System)]
        }

        /// Build a `ToolContext` whose worktree is the given (empty) tempdir, with a
        /// [`FixedClassifier`] reporting `tier` and no approval. Returns the context and the
        /// classifier so the test can assert the call count.
        fn ctx_in(root: &TempDir, tier: Tier) -> (ToolContext, Arc<FixedClassifier>) {
            let classifier = Arc::new(FixedClassifier::new(tier, None));
            let ctx = ToolContext {
                worktree: Arc::new(FixedWorktree(root.path().to_path_buf())),
                classifier: classifier.clone(),
                resources: Arc::new(UnusedResources),
                retired_drive: "C:".to_owned(),
                limits: BuiltinLimits {
                    call_timeout: Duration::from_secs(30),
                    output_cap: 65_536,
                },
            };
            (ctx, classifier)
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(256))]

            #[test]
            fn above_threshold_without_approval_refuses_and_spawns_nothing(
                tier in above_threshold_tier(),
                // A non-empty command name (schema guarantees a non-empty string at the boundary),
                // excluding control/path/quote characters that could trip arg handling.
                command in "[A-Za-z0-9_.-]{1,32}",
                // Arbitrary extra args; the sentinel-writing args are appended on top of these.
                extra_args in proptest::collection::vec("[A-Za-z0-9_.-]{0,16}", 0..4),
            ) {
                // A fresh, empty worktree for this case; the only way it gains an entry is a child
                // process started with it as the working directory.
                let worktree = TempDir::new().expect("create temp worktree");
                prop_assert!(
                    std::fs::read_dir(worktree.path())
                        .expect("read empty worktree")
                        .next()
                        .is_none(),
                    "worktree must start empty",
                );

                let sentinel = "nebula_prop4_sentinel.txt";
                // Build args that, if the command ever executed under a shell, would create the
                // sentinel in the working directory (the worktree). It must never run.
                let mut args: Vec<serde_json::Value> =
                    extra_args.iter().map(|a| serde_json::json!(a)).collect();
                args.push(serde_json::json!("/c"));
                args.push(serde_json::json!(format!("echo spawned > {sentinel}")));

                let (ctx, classifier) = ctx_in(&worktree, tier);

                let result = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("build current-thread runtime")
                    .block_on(ShellRun.call(
                        serde_json::json!({ "command": command, "args": args }),
                        &ctx,
                    ));

                // (a) The call is refused with InvalidArguments.
                match result {
                    Err(ToolError::InvalidArguments { tool, detail }) => {
                        prop_assert_eq!(tool, ShellRun::NAME);
                        prop_assert!(
                            detail.contains("approval"),
                            "refusal detail should mention approval, got {:?}",
                            detail,
                        );
                    }
                    other => prop_assert!(
                        false,
                        "expected InvalidArguments refusal, got {:?}",
                        other,
                    ),
                }

                // (b) The gate consulted the classifier exactly once, before any spawn.
                prop_assert_eq!(classifier.calls.load(Ordering::SeqCst), 1);

                // (c) No process started: the worktree is still empty and the sentinel is absent.
                prop_assert!(
                    !worktree.path().join(sentinel).exists(),
                    "sentinel exists: a process was spawned despite refusal",
                );
                prop_assert!(
                    std::fs::read_dir(worktree.path())
                        .expect("read worktree after call")
                        .next()
                        .is_none(),
                    "worktree gained an entry: a process was spawned despite refusal",
                );
            }
        }
    }

    // Issue #27 acceptance tests (task 9.5): shell timeout/job kill and the tier-3 refuse/execute
    // gate exercised through the real `ShellRun::call` execution path.
    //
    // Validates: Requirements 4.4 (above-threshold refusal starts no process), 4.8 (per-call
    // timeout measured from child start), 4.9 (timeout kills the child via the job and returns
    // `Timeout`). The spawning tests are `#[cfg(windows)]` because `execute` only spawns on
    // Windows; each uses a real `TempDir` worktree so output and cwd are isolated.
    mod acceptance_timeout_and_tier3 {
        use tempfile::TempDir;

        use super::*;

        /// Build a `ToolContext` whose worktree is `root`, with the given classifier and per-call
        /// timeout. The output cap is the host default. Isolating the worktree in a `TempDir`
        /// keeps the executed child's cwd and any output off real user paths.
        fn ctx_with_worktree(
            root: &TempDir,
            classifier: Arc<dyn CommandClassifier>,
            call_timeout: Duration,
        ) -> ToolContext {
            ToolContext {
                worktree: Arc::new(FixedWorktree(root.path().to_path_buf())),
                classifier,
                resources: Arc::new(UnusedResources),
                retired_drive: "C:".to_owned(),
                limits: BuiltinLimits {
                    call_timeout,
                    output_cap: 65_536,
                },
            }
        }

        /// A sleeping command exceeds the per-call timeout, is killed via the Job Object (the
        /// `JobChild` guard drop on elapse), and the call returns `ToolError::Timeout` naming
        /// `shell.run` (Requirements 4.8, 4.9).
        #[cfg(windows)]
        #[tokio::test]
        async fn sleeping_command_times_out_and_is_killed() {
            // A permissive classifier (Tier::Read, no approval) so the gate executes.
            let classifier = Arc::new(FixedClassifier::new(Tier::Read, None));
            let worktree = TempDir::new().expect("create temp worktree");
            // Short timeout so the test is quick; the child would otherwise run ~20s.
            let ctx = ctx_with_worktree(&worktree, classifier.clone(), Duration::from_millis(500));

            // `ping.exe -n 20 127.0.0.1` blocks for ~19s, well past the 500ms timeout. The command
            // name carries its `.exe` extension so Windows resolves it against the scrubbed
            // `CONTROLLED_PATH` (which includes System32) without relying on an inherited `PATHEXT`
            // (the scrubbed environment drops it). The child is assigned to the Job Object, so the
            // guard drop on timeout kills it.
            let err = ShellRun
                .call(
                    serde_json::json!({
                        "command": "ping.exe",
                        "args": ["-n", "20", "127.0.0.1"],
                    }),
                    &ctx,
                )
                .await
                .expect_err("a sleeping command must hit the timeout");

            match err {
                ToolError::Timeout { tool, timeout_ms } => {
                    assert_eq!(tool, "shell.run");
                    assert_eq!(timeout_ms, 500);
                }
                other => panic!("expected Timeout, got {other:?}"),
            }
            // The gate ran once before the (killed) spawn.
            assert_eq!(classifier.calls.load(Ordering::SeqCst), 1);
        }

        /// A tier-3 (`Tier::System`) command with no approval is refused with `InvalidArguments`
        /// and no process is started — asserted by an empty `TempDir` worktree after the call
        /// (Requirement 4.4). Cross-platform: the refusal happens before `execute`, so no spawn.
        #[tokio::test]
        async fn tier3_without_approval_is_refused_and_spawns_nothing() {
            let classifier = Arc::new(FixedClassifier::new(Tier::System, None));
            let worktree = TempDir::new().expect("create temp worktree");
            let ctx = ctx_with_worktree(&worktree, classifier.clone(), Duration::from_secs(30));

            let err = ShellRun
                .call(
                    serde_json::json!({
                        "command": "cmd",
                        "args": ["/C", "echo", "hi"],
                    }),
                    &ctx,
                )
                .await
                .expect_err("tier-3 without approval must be refused");

            match err {
                ToolError::InvalidArguments { tool, detail } => {
                    assert_eq!(tool, "shell.run");
                    assert!(
                        detail.contains("approval"),
                        "detail should mention approval"
                    );
                }
                other => panic!("expected InvalidArguments, got {other:?}"),
            }
            // The gate consulted the classifier exactly once, before any spawn.
            assert_eq!(classifier.calls.load(Ordering::SeqCst), 1);
            // No process ran: the worktree is still empty.
            assert!(
                std::fs::read_dir(worktree.path())
                    .expect("read worktree after refusal")
                    .next()
                    .is_none(),
                "worktree gained an entry: a process was spawned despite refusal",
            );
        }

        /// A tier-3 (`Tier::System`) command WITH an approval executes: the approval path reaches
        /// `execute` and the command runs to completion, producing its output (Requirement 4.5,
        /// complementing the 4.4 refusal above). `#[cfg(windows)]` because execution only spawns
        /// on Windows.
        #[cfg(windows)]
        #[tokio::test]
        async fn tier3_with_approval_executes() {
            let classifier = Arc::new(FixedClassifier::new(Tier::System, Some(Approval::new())));
            let worktree = TempDir::new().expect("create temp worktree");
            let ctx = ctx_with_worktree(&worktree, classifier.clone(), Duration::from_secs(30));

            let output = ShellRun
                .call(
                    serde_json::json!({
                        "command": "cmd",
                        "args": ["/C", "echo", "hi"],
                    }),
                    &ctx,
                )
                .await
                .expect("tier-3 with approval must execute");

            assert!(!output.is_error, "echo should exit successfully");
            let text = String::from_utf8_lossy(&output.bytes);
            assert!(
                text.contains("hi"),
                "command output should contain the echoed text, got {text:?}",
            );
            // The gate consulted the classifier exactly once.
            assert_eq!(classifier.calls.load(Ordering::SeqCst), 1);
        }
    }
}
