//! Permission tiers, approvals, and command classification for built-in tools.
//!
//! This module defines the injectable boundary between `nebula-tools` and the full permission
//! engine planned for the `nebula-sandbox` crate (GitHub issue #28). It ships:
//!
//! - [`Tier`] — the four operation privilege levels (design 7.1).
//! - [`NO_APPROVAL_THRESHOLD`] — the highest tier that runs without an approval decision.
//! - [`Approval`] — a minimal grant that authorizes an above-threshold command.
//! - [`CommandClassifier`] — the object-safe trait `shell.run` consults before running any
//!   child process.
//! - [`DefaultClassifier`] — a deterministic rules subset sufficient for this feature's
//!   acceptance tests. It never grants an [`Approval`] on its own, so Tier 2/3 commands are
//!   refused unless a caller injects an approving classifier.
//!
//! **Relocation note:** `permit` is a candidate for relocation to the `nebula-sandbox` crate
//! when the full command classifier, permission-tier engine, and approval UX land in issue #28.
//! It is defined here as an injectable boundary so built-in tools (issue #27) can ship first.

/// Operation privilege level (design 7.1).
///
/// Tiers are ordered: a higher tier is strictly more privileged. The ordering is relied on by
/// the no-approval threshold comparison and by the effective-tier helper, which never lowers an
/// enforced tier below the classifier's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    /// Tier 0 — read-only operations (e.g. `fs.read`, `git.status`, `system.resources`).
    Read = 0,
    /// Tier 1 — sandbox write confined to the worktree (e.g. `fs.write`, `git.commit`). This is
    /// the [`NO_APPROVAL_THRESHOLD`]: the highest tier that runs without an approval decision.
    Sandbox = 1,
    /// Tier 2 — workspace-level operations; require an approval decision.
    Workspace = 2,
    /// Tier 3 — system-level operations; always require an approval decision.
    System = 3,
}

/// The highest [`Tier`] that runs without an [`Approval`] decision (Tier 1, [`Tier::Sandbox`]).
///
/// Any command classified strictly above this threshold ([`Tier::Workspace`] or
/// [`Tier::System`]) must be refused unless an [`Approval`] is present.
pub const NO_APPROVAL_THRESHOLD: Tier = Tier::Sandbox;

/// An approval authorizing a command classified above the [`NO_APPROVAL_THRESHOLD`].
///
/// This is intentionally minimal for issue #27: the mere presence of an `Approval` authorizes
/// execution. Issue #28 will define the full grant identity and scope. The type is kept opaque
/// (no public fields) so the richer definition can be added without a breaking change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Approval {
    /// Private marker. Prevents external construction and leaves room for grant id / scope
    /// fields that issue #28 will add.
    _private: (),
}

impl Approval {
    /// Construct a minimal approval grant.
    ///
    /// For issue #27, presence alone authorizes an above-threshold command; there is no scope to
    /// configure yet. Callers (tests, or an approving classifier) use this to inject a grant.
    #[must_use]
    pub const fn new() -> Self {
        Self { _private: () }
    }
}

impl Default for Approval {
    fn default() -> Self {
        Self::new()
    }
}

/// Classifies a shell command and reports the approval decision for it.
///
/// This is the injectable boundary `shell.run` consults before starting any child process
/// (Requirement 4.2, 4.11). The full engine is issue #28; this feature ships
/// [`DefaultClassifier`] as a deterministic subset. The trait is object-safe so it can be stored
/// as `Arc<dyn CommandClassifier>` on the tool context.
pub trait CommandClassifier: Send + Sync {
    /// Return the [`Tier`] and any [`Approval`] decision for `command` with `args`.
    ///
    /// A returned `Some(Approval)` authorizes execution of a command classified strictly above
    /// the [`NO_APPROVAL_THRESHOLD`]; `None` means no approval was granted and such a command
    /// must be refused.
    fn classify(&self, command: &str, args: &[String]) -> (Tier, Option<Approval>);
}

/// Combine the [`Command_Classifier`](CommandClassifier) tier with a model second opinion into
/// the tier `shell.run` enforces (Requirement 4.12, design 7.2).
///
/// A model second opinion is **advisory only**: it may raise the enforced tier (ask for more
/// caution) but may never lower it below what the classifier returned. The enforced tier is
/// therefore `max(classifier_tier, advisory_tier)`, relying on [`Tier`]'s ordering where a
/// higher tier is strictly more privileged / more restricted.
///
/// This keeps the classifier authoritative for the floor: no advisory input can downgrade a
/// `git push` out of [`Tier::System`], while an advisory input is still free to escalate an
/// otherwise innocuous command.
#[must_use]
pub fn effective_tier(classifier_tier: Tier, advisory_tier: Tier) -> Tier {
    classifier_tier.max(advisory_tier)
}

/// A deterministic rules subset of the command classifier, sufficient for this feature's
/// acceptance tests (design 7.2).
///
/// The rules, applied in order:
/// - Destructive file operations → [`Tier::System`]: `rm -rf`, PowerShell `Remove-Item
///   -Recurse`, `del`, `format`, `diskpart`.
/// - `git push`, or `git reset --hard` targeting a protected ref → [`Tier::System`].
/// - System configuration commands → [`Tier::System`]: `reg`, `sc`, `schtasks`, `setx`,
///   `netsh`, `Set-ExecutionPolicy`.
/// - Global package installs → [`Tier::System`].
/// - Known read/sandbox commands → [`Tier::Read`] / [`Tier::Sandbox`].
/// - Any unknown command → [`Tier::Workspace`].
///
/// `DefaultClassifier` **never** returns an [`Approval`] on its own, so every [`Tier::Workspace`]
/// and [`Tier::System`] command is refused unless a caller injects an approving classifier. This
/// is exactly what "refuses tier-3 without approval" requires.
///
/// **Relocation note:** like the rest of `permit`, this is a candidate for relocation to the
/// `nebula-sandbox` crate when the full command classifier and permission engine land in issue
/// #28; it ships here as a deterministic placeholder so built-in tools (issue #27) can run first.
#[derive(Clone, Copy, Debug, Default)]
pub struct DefaultClassifier;

impl DefaultClassifier {
    /// Protected refs for which `git reset --hard` is escalated to [`Tier::System`].
    const PROTECTED_REFS: &'static [&'static str] =
        &["main", "master", "origin/main", "origin/master", "HEAD"];

    /// System-configuration command names (compared case-insensitively) that map to
    /// [`Tier::System`].
    const SYSTEM_CONFIG_COMMANDS: &'static [&'static str] = &[
        "reg",
        "sc",
        "schtasks",
        "setx",
        "netsh",
        "set-executionpolicy",
    ];

    /// Destructive command names (compared case-insensitively) that map to [`Tier::System`]
    /// on their own, regardless of arguments.
    const DESTRUCTIVE_COMMANDS: &'static [&'static str] = &["del", "format", "diskpart"];

    /// Known read-only command names (compared case-insensitively) → [`Tier::Read`].
    const READ_COMMANDS: &'static [&'static str] = &[
        "echo", "cat", "type", "ls", "dir", "pwd", "cd", "find", "grep", "where", "whoami",
    ];

    /// Known sandbox-write command names (compared case-insensitively) → [`Tier::Sandbox`].
    const SANDBOX_COMMANDS: &'static [&'static str] =
        &["mkdir", "touch", "cp", "copy", "mv", "move"];

    /// Lower-case the final path component of a command so `C:\\Windows\\reg.exe` and `reg`
    /// classify identically.
    fn normalize(command: &str) -> String {
        let trimmed = command.trim();
        let last = trimmed.rsplit(['/', '\\']).next().unwrap_or(trimmed);
        let base = last.strip_suffix(".exe").unwrap_or(last);
        base.to_ascii_lowercase()
    }

    /// Case-insensitive equality against a lower-cased candidate.
    fn eq_ci(candidate_lower: &str, probe: &str) -> bool {
        candidate_lower == probe.to_ascii_lowercase()
    }

    /// Classify a PowerShell `Remove-Item` invocation: recursive removal is destructive.
    fn is_recursive_remove_item(name: &str, args: &[String]) -> bool {
        if name != "remove-item" && name != "ri" && name != "rm" {
            return false;
        }
        args.iter().any(|a| {
            let a = a.to_ascii_lowercase();
            a == "-recurse" || a == "-r"
        })
    }

    /// Classify an `rm` invocation: `-rf` / `-r -f` style recursive-force removal is destructive.
    fn is_recursive_force_rm(name: &str, args: &[String]) -> bool {
        if name != "rm" {
            return false;
        }
        let mut recursive = false;
        let mut force = false;
        for arg in args {
            if let Some(flags) = arg.strip_prefix("--") {
                match flags {
                    "recursive" => recursive = true,
                    "force" => force = true,
                    _ => {}
                }
            } else if let Some(flags) = arg.strip_prefix('-') {
                for ch in flags.chars() {
                    match ch {
                        'r' | 'R' => recursive = true,
                        'f' => force = true,
                        _ => {}
                    }
                }
            }
        }
        recursive && force
    }

    /// Classify a `git` subcommand into a [`Tier`].
    ///
    /// `git push` and `git reset --hard <protected-ref>` escalate to [`Tier::System`]; other git
    /// subcommands are sandbox-level writes (the dedicated `git.*` built-ins enforce their own,
    /// stricter confinement — this classifier only governs `shell.run`).
    fn classify_git(args: &[String]) -> Tier {
        let Some(subcommand) = args.first() else {
            return Tier::Sandbox;
        };
        let subcommand = subcommand.to_ascii_lowercase();
        match subcommand.as_str() {
            "push" | "fetch" | "pull" | "remote" => Tier::System,
            "reset" => {
                let hard = args.iter().any(|a| a.eq_ignore_ascii_case("--hard"));
                let targets_protected = args.iter().any(|a| {
                    Self::PROTECTED_REFS
                        .iter()
                        .any(|r| a.eq_ignore_ascii_case(r))
                });
                if hard && targets_protected {
                    Tier::System
                } else {
                    Tier::Sandbox
                }
            }
            _ => Tier::Sandbox,
        }
    }

    /// Detect a global package install (e.g. `npm install -g`, `pip install --user`-less global
    /// installs, `cargo install`, `go install`), which escalates to [`Tier::System`].
    fn is_global_install(name: &str, args: &[String]) -> bool {
        let has = |needle: &str| args.iter().any(|a| a.eq_ignore_ascii_case(needle));
        match name {
            "npm" | "pnpm" | "yarn" => {
                let installs = has("install") || has("i") || has("add");
                installs && (has("-g") || has("--global"))
            }
            "pip" | "pip3" => has("install") && !has("--user"),
            "cargo" | "go" | "gem" => has("install"),
            _ => false,
        }
    }
}

impl CommandClassifier for DefaultClassifier {
    fn classify(&self, command: &str, args: &[String]) -> (Tier, Option<Approval>) {
        let name = Self::normalize(command);

        // 1. Destructive file operations → Tier 3.
        if Self::DESTRUCTIVE_COMMANDS
            .iter()
            .any(|c| Self::eq_ci(&name, c))
            || Self::is_recursive_force_rm(&name, args)
            || Self::is_recursive_remove_item(&name, args)
        {
            return (Tier::System, None);
        }

        // 2. git push / git reset --hard on protected refs → Tier 3 (other git → Sandbox).
        if name == "git" {
            return (Self::classify_git(args), None);
        }

        // 3. System configuration commands → Tier 3.
        if Self::SYSTEM_CONFIG_COMMANDS
            .iter()
            .any(|c| Self::eq_ci(&name, c))
        {
            return (Tier::System, None);
        }

        // 4. Global installs → Tier 3.
        if Self::is_global_install(&name, args) {
            return (Tier::System, None);
        }

        // 5. Known read commands → Tier 0.
        if Self::READ_COMMANDS.iter().any(|c| Self::eq_ci(&name, c)) {
            return (Tier::Read, None);
        }

        // 6. Known sandbox-write commands → Tier 1.
        if Self::SANDBOX_COMMANDS.iter().any(|c| Self::eq_ci(&name, c)) {
            return (Tier::Sandbox, None);
        }

        // 7. Unknown command → Tier 2 (requires approval, never granted here).
        (Tier::Workspace, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn tier_ordering_is_strict() {
        assert!(Tier::Read < Tier::Sandbox);
        assert!(Tier::Sandbox < Tier::Workspace);
        assert!(Tier::Workspace < Tier::System);
        assert_eq!(NO_APPROVAL_THRESHOLD, Tier::Sandbox);
    }

    #[test]
    fn effective_tier_never_drops_below_classifier() {
        // Advisory raises the enforced tier.
        assert_eq!(
            effective_tier(Tier::Read, Tier::System),
            Tier::System,
            "advisory may escalate"
        );
        // Advisory can never lower the classifier's tier.
        assert_eq!(
            effective_tier(Tier::System, Tier::Read),
            Tier::System,
            "advisory must not downgrade"
        );
        // Equal tiers are preserved.
        assert_eq!(
            effective_tier(Tier::Workspace, Tier::Workspace),
            Tier::Workspace
        );
        // The result is always at least the classifier tier across every pair.
        for &classifier in &[Tier::Read, Tier::Sandbox, Tier::Workspace, Tier::System] {
            for &advisory in &[Tier::Read, Tier::Sandbox, Tier::Workspace, Tier::System] {
                assert!(effective_tier(classifier, advisory) >= classifier);
            }
        }
    }

    // Feature: builtin-tools, Property 6: a second opinion never lowers the enforced tier
    //
    // Property 6: A second opinion never lowers the enforced tier. For any classifier tier and
    // any advisory (model second-opinion) tier, the tier `shell.run` enforces is at least the
    // classifier tier: an advisory input may escalate caution but can never downgrade the
    // classifier's floor. We also check the enforced tier is the maximum of the two, so an
    // advisory input that is itself higher is honoured. See design.md, Property 6
    // (Validates: Requirements 4.12).
    mod property_second_opinion_never_lowers {
        use super::*;
        use proptest::prelude::*;

        /// Generate any of the four tiers with uniform coverage.
        fn any_tier() -> impl Strategy<Value = Tier> {
            prop_oneof![
                Just(Tier::Read),
                Just(Tier::Sandbox),
                Just(Tier::Workspace),
                Just(Tier::System),
            ]
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(256))]

            #[test]
            fn enforced_tier_is_never_below_classifier(
                classifier in any_tier(),
                advisory in any_tier(),
            ) {
                let enforced = effective_tier(classifier, advisory);

                // The advisory second opinion may raise, but never lower, the classifier floor.
                prop_assert!(
                    enforced >= classifier,
                    "enforced {enforced:?} dropped below classifier {classifier:?} (advisory {advisory:?})",
                );
                // The enforced tier is exactly the more restrictive of the two inputs, so a
                // higher advisory tier is still honoured.
                prop_assert_eq!(enforced, classifier.max(advisory));
            }
        }
    }

    #[test]
    fn destructive_file_ops_are_tier_system() {
        let c = DefaultClassifier;
        assert_eq!(c.classify("rm", &args(&["-rf", "foo"])).0, Tier::System);
        assert_eq!(
            c.classify("rm", &args(&["-r", "-f", "foo"])).0,
            Tier::System
        );
        assert_eq!(c.classify("del", &args(&["foo"])).0, Tier::System);
        assert_eq!(c.classify("format", &args(&["D:"])).0, Tier::System);
        assert_eq!(c.classify("diskpart", &[]).0, Tier::System);
        assert_eq!(
            c.classify("Remove-Item", &args(&["-Recurse", "foo"])).0,
            Tier::System
        );
    }

    #[test]
    fn git_push_and_hard_reset_on_protected_ref_are_tier_system() {
        let c = DefaultClassifier;
        assert_eq!(c.classify("git", &args(&["push"])).0, Tier::System);
        assert_eq!(
            c.classify("git", &args(&["reset", "--hard", "main"])).0,
            Tier::System
        );
        // A hard reset onto a non-protected ref stays at sandbox level.
        assert_eq!(
            c.classify("git", &args(&["reset", "--hard", "feature/x"]))
                .0,
            Tier::Sandbox
        );
        // A plain status is sandbox-level under shell.run classification.
        assert_eq!(c.classify("git", &args(&["status"])).0, Tier::Sandbox);
    }

    #[test]
    fn system_config_commands_are_tier_system() {
        let c = DefaultClassifier;
        for cmd in [
            "reg",
            "sc",
            "schtasks",
            "setx",
            "netsh",
            "Set-ExecutionPolicy",
        ] {
            assert_eq!(c.classify(cmd, &[]).0, Tier::System, "{cmd}");
        }
    }

    #[test]
    fn global_installs_are_tier_system() {
        let c = DefaultClassifier;
        assert_eq!(
            c.classify("npm", &args(&["install", "-g", "typescript"])).0,
            Tier::System
        );
        assert_eq!(
            c.classify("cargo", &args(&["install", "ripgrep"])).0,
            Tier::System
        );
    }

    #[test]
    fn known_read_and_sandbox_commands_are_low_tier() {
        let c = DefaultClassifier;
        assert_eq!(c.classify("echo", &args(&["hi"])).0, Tier::Read);
        assert_eq!(c.classify("ls", &[]).0, Tier::Read);
        assert_eq!(c.classify("mkdir", &args(&["out"])).0, Tier::Sandbox);
    }

    #[test]
    fn unknown_command_is_tier_workspace() {
        let c = DefaultClassifier;
        assert_eq!(c.classify("some-random-binary", &[]).0, Tier::Workspace);
    }

    #[test]
    fn default_classifier_never_grants_approval() {
        let c = DefaultClassifier;
        for (cmd, a) in [
            ("rm", args(&["-rf", "x"])),
            ("git", args(&["push"])),
            ("echo", args(&["hi"])),
            ("unknown", vec![]),
        ] {
            assert!(c.classify(cmd, &a).1.is_none(), "{cmd} should not approve");
        }
    }

    #[test]
    fn path_qualified_and_exe_suffixed_commands_normalize() {
        let c = DefaultClassifier;
        assert_eq!(
            c.classify(r"C:\Windows\System32\reg.exe", &[]).0,
            Tier::System
        );
        assert_eq!(
            c.classify("/usr/bin/rm", &args(&["-rf", "x"])).0,
            Tier::System
        );
    }
}
