//! Permission tiers, approvals, and command classification for built-in tools.
//!
//! **Relocation (issue #28).** The permission vocabulary now lives in the `nebula-sandbox`
//! crate; this module re-exports it so every issue #27 call site (`nebula_tools::permit::Tier`,
//! `nebula_tools::permit::Approval`, …) keeps resolving unchanged (Requirement 8):
//!
//! - [`Tier`] — the four operation privilege levels (design 7.1).
//! - [`NO_APPROVAL_THRESHOLD`] — the highest tier that runs without an approval decision.
//! - [`Approval`] — the grant that authorizes an above-threshold command.
//! - [`CommandClassifier`] — the object-safe trait `shell.run` consults before running any
//!   child process.
//! - [`effective_tier`] — the advisory-tier merge, kept here (re-exported) so #27 Property 6
//!   is preserved.
//!
//! The real data-driven engine now lives in `nebula-sandbox` as
//! [`RulesClassifier`](nebula_sandbox::engine::RulesClassifier), and the daemon wires it in
//! `builtin_providers.rs` (via `RulesClassifier::embedded`). [`DefaultClassifier`] stays here as a
//! self-contained deterministic classifier over the re-exported vocabulary: it is the fixture the
//! issue #27 `shell.run` tests inject directly, so `nebula_tools::DefaultClassifier` references
//! keep resolving and those tests keep passing unchanged (Requirement 8.6). It is not what the
//! daemon runs.

pub use nebula_sandbox::{
    Approval, CommandClassifier, NO_APPROVAL_THRESHOLD, Tier, effective_tier,
};

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
/// **Scope:** this is the fixture classifier the issue #27 `shell.run` tests inject; it is not
/// what the daemon runs. The daemon wires the data-driven
/// [`RulesClassifier`](nebula_sandbox::engine::RulesClassifier) from `nebula-sandbox`. This type
/// is retained so those #27 tests continue to compile and pass unchanged (Requirement 8.6).
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

    // The permission vocabulary (`Tier` ordering, `NO_APPROVAL_THRESHOLD`, `effective_tier`, and
    // Property 12) is now owned and tested by `nebula-sandbox` (`src/tier.rs`). The tests below
    // cover only the `DefaultClassifier` that still lives in this crate.

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
