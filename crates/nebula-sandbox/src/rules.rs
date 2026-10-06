//! The rules table: the serde model for the checked-in `rules.toml`, the validating loader, and
//! the command index used for fast matching.
//!
//! This module defines the deserialization model for `rules.toml` — the [`RuleId`] newtype, the
//! [`Rule`]/[`MatchCondition`]/[`PathCondition`]/[`PathTargets`] shapes a rule is built from —
//! and the [`LoadError`] the validating loader surfaces. Every struct denies unknown fields
//! (AGENTS.md config convention) so a typo in `rules.toml` fails the load rather than being
//! silently ignored. [`RulesTable::load`] parses, validates, and indexes the table into a
//! [`RulesTable`] the classifier engine consults for fast matching.

/// A stable, unique rule identifier (Req 1.3).
///
/// Newtype over the TOML string `id` of a `[[rule]]` entry. The identifier must be unique across
/// the table and stable across loads of the same table, so a recorded [`crate::rules::RuleId`]
/// on a decision names the same rule on every run. Deserializes transparently from the bare
/// string, so `id = "fs.delete.recursive-force"` yields `RuleId("fs.delete.recursive-force")`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Deserialize)]
#[serde(transparent)]
pub struct RuleId(pub String);

/// Top-level deserialization target for `rules.toml`.
///
/// Private because it is only an intermediate parse target: the validating [`RulesTable::load`]
/// consumes it and produces the public, indexed [`RulesTable`]. The `[[rule]]` array-of-tables in
/// the TOML deserializes into `rule`.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RulesFile {
    /// The rules, in file order, one per `[[rule]]` entry.
    rule: Vec<Rule>,
}

/// One rule: a stable id, a match condition, and the tier it assigns (Req 1.3).
///
/// The `[rule.match]` sub-table deserializes into [`Self::condition`] (the TOML key is `match`,
/// a Rust keyword, so it is renamed). The `tier` is read as a raw `u8` here and validated into
/// the `0..=3` range by the loader (task 5.2); an out-of-range value is rejected there rather
/// than at parse time, so the load error can name the offending rule.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    /// The stable, unique identifier of this rule.
    pub id: RuleId,
    /// What this rule matches on (the `[rule.match]` sub-table).
    #[serde(rename = "match")]
    pub condition: MatchCondition,
    /// The tier this rule assigns; validated into `0..=3` at load (Req 1.3).
    pub tier: u8,
}

/// What a rule matches on: command name/aliases, argument patterns, and an optional path
/// condition.
///
/// Any field left unset is unconstrained. All fields default, so a rule may constrain on just a
/// command name, just an argument pattern, or any combination. A token, flag, or path matched by
/// no rule's condition is "unknown" and forces Tier 3 at classification time (Req 3.1); that
/// escalation is the engine's job (task 6), not this model's.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MatchCondition {
    /// Command base name(s), compared case-insensitively after normalizing path and `.exe`.
    #[serde(default)]
    pub command: Vec<String>,
    /// Argument patterns (literal flags/tokens, or simple globs) of which ANY must be present.
    #[serde(default)]
    pub args_any: Vec<String>,
    /// Argument patterns (literal flags/tokens, or simple globs) of which ALL must be present.
    #[serde(default)]
    pub args_all: Vec<String>,
    /// Marks this rule as an encoded/indirection sink that cannot be evaluated (Req 3.4).
    #[serde(default)]
    pub encoded_indirection: bool,
    /// Marks this rule as a network-configuration modifier (Req 2.9).
    #[serde(default)]
    pub network_config: bool,
    /// Optional path condition, evaluated through the `Path_Resolver` at classify time (Req 2.2).
    #[serde(default)]
    pub path: Option<PathCondition>,
}

/// How a resolved target path contributes to the tier (Req 2.4–2.7, 2.10).
///
/// Names which argument(s) of the command carry target paths (see [`PathTargets`]) and whether
/// the operation is destructive on an existing path. The engine resolves each named target
/// through the `Path_Resolver` and maps the result to a tier floor; a destructive operation on a
/// path outside the worktree escalates to Tier 3 (Req 2.10).
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathCondition {
    /// Which argument(s) carry target paths (by index, or "all non-flag args").
    pub targets: PathTargets,
    /// Whether the operation is destructive on an existing path (delete/overwrite/truncate).
    #[serde(default)]
    pub destructive: bool,
}

/// Which command arguments carry the target path(s) a [`PathCondition`] evaluates.
///
/// Deserializes from one of two TOML forms:
///
/// - the string `"all-non-flag"` — every argument that is not a flag is treated as a target
///   path (`targets = "all-non-flag"`);
/// - an array of zero-based argument indices — only those positional arguments are targets
///   (`targets = [0, 2]`).
///
/// The variants are tried in order (string first, then indices), so the string spelling must
/// match exactly; any other value fails the load with [`LoadError::Parse`].
#[derive(Debug, serde::Deserialize)]
#[serde(untagged)]
pub enum PathTargets {
    /// Every non-flag argument is a target path. TOML form: `targets = "all-non-flag"`.
    AllNonFlag(AllNonFlag),
    /// Only the listed zero-based argument indices are target paths. TOML form:
    /// `targets = [0, 2]`.
    Indices(Vec<usize>),
}

/// The marker accepted by [`PathTargets::AllNonFlag`].
///
/// A string-valued enum with the single spelling `all-non-flag`, so `targets = "all-non-flag"`
/// deserializes and any other string is rejected. Kept as a distinct type (rather than a bare
/// unit) so the accepted spelling is pinned by `#[serde(rename_all = "kebab-case")]`.
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AllNonFlag {
    /// The `"all-non-flag"` literal.
    AllNonFlag,
}

/// Why loading the rules table (or the red-team fixture) failed (Req 1.8).
///
/// Every variant rejects the WHOLE table: on any error no rule is usable and no command can be
/// classified, so a misconfiguration fails closed rather than booting into an unprotected state.
/// The variants name the offending entry wherever possible so the fault is easy to locate.
#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    /// The TOML could not be parsed (syntax error, wrong type, or unknown field).
    #[error("rules table failed to parse: {0}")]
    Parse(String),
    /// Two or more rules share the same identifier; the identifier is reported.
    #[error("duplicate rule id {0:?} in rules table")]
    DuplicateRuleId(RuleId),
    /// A rule is internally invalid (for example a `tier` outside `0..=3`); the rule id and a
    /// detail are reported.
    #[error("malformed rule {id:?}: {detail}")]
    MalformedRule {
        /// The identifier of the offending rule.
        id: RuleId,
        /// A human-readable description of why the rule is malformed.
        detail: String,
    },
    /// The embedded red-team fixture is malformed (consumed by task 8).
    #[error("red-team fixture invalid: {0}")]
    RedTeamFixture(String),
}

/// The lowest and highest valid tier values a rule may assign (Req 1.3).
///
/// A rule's raw `tier` must fall within this inclusive range; anything outside rejects the
/// whole table with [`LoadError::MalformedRule`].
const MIN_TIER: u8 = 0;
/// The highest valid tier value a rule may assign (see [`MIN_TIER`]).
const MAX_TIER: u8 = 3;

/// A validated, indexed rules table ready for the classifier engine (Req 1.7).
///
/// Built only by [`RulesTable::load`], which parses `rules.toml`, validates every rule, and
/// builds the command index. Because the table is produced solely by a successful load, a
/// `RulesTable` value is proof that loading completed and validation passed before any decision
/// can be produced from it — a misconfiguration fails the load and never yields a table.
///
/// # Indexing
///
/// [`Self::by_command`] maps a normalized command base name (see [`normalize_command`]) to the
/// indices, into [`Self::rules`], of the rules that name that command. Rules whose
/// [`MatchCondition::command`] list is empty (pure path/argument rules) are **not** present in
/// the index under any key; the engine (task 6) reaches them by iterating [`Self::rules`]
/// directly for command-less rules. The index is purely an acceleration structure for rules
/// that do constrain on a command name.
#[derive(Debug)]
pub struct RulesTable {
    /// Every validated rule, in file order. Rule indices used by [`Self::by_command`] and
    /// returned by [`Self::candidates`] point into this slice.
    rules: Vec<Rule>,
    /// Normalized command base name → indices (into [`Self::rules`]) of rules naming that
    /// command. Command-less rules are absent (see the type docs).
    by_command: std::collections::HashMap<String, Vec<usize>>,
}

impl RulesTable {
    /// Parse, validate, and index a rules table from TOML text (Req 1.3, 1.7, 1.8).
    ///
    /// Parsing uses the private [`RulesFile`] serde model, so a syntax error, a wrong type, or
    /// an unknown field all surface as [`LoadError::Parse`]. After a successful parse the whole
    /// table is validated: every rule's `tier` must lie within `0..=3` (otherwise
    /// [`LoadError::MalformedRule`] names the offending rule) and no two rules may share an `id`
    /// (otherwise [`LoadError::DuplicateRuleId`] names the duplicated id). Validation rejects on
    /// the first offender, but always names it, so the fault is easy to locate. Only once every
    /// rule is valid is the [`Self::by_command`] index built and the table returned, so no
    /// decision can ever be produced from a rejected table.
    ///
    /// # Errors
    ///
    /// Returns [`LoadError::Parse`] on a parse failure, [`LoadError::MalformedRule`] on a rule
    /// with a tier outside `0..=3`, or [`LoadError::DuplicateRuleId`] on a repeated rule id.
    pub fn load(toml_text: &str) -> Result<Self, LoadError> {
        let file: RulesFile =
            toml::from_str(toml_text).map_err(|e| LoadError::Parse(e.to_string()))?;

        let mut seen: std::collections::HashSet<RuleId> =
            std::collections::HashSet::with_capacity(file.rule.len());
        for rule in &file.rule {
            if !(MIN_TIER..=MAX_TIER).contains(&rule.tier) {
                return Err(LoadError::MalformedRule {
                    id: rule.id.clone(),
                    detail: format!("tier {} out of range {MIN_TIER}..={MAX_TIER}", rule.tier),
                });
            }
            if !seen.insert(rule.id.clone()) {
                return Err(LoadError::DuplicateRuleId(rule.id.clone()));
            }
        }

        let rules = file.rule;
        let mut by_command: std::collections::HashMap<String, Vec<usize>> =
            std::collections::HashMap::new();
        for (index, rule) in rules.iter().enumerate() {
            for command in &rule.condition.command {
                let normalized = normalize_command(command);
                by_command.entry(normalized).or_default().push(index);
            }
        }

        Ok(Self { rules, by_command })
    }

    /// All validated rules, in file order (consumed by the engine, task 6).
    ///
    /// The engine iterates this slice directly to reach command-less rules (pure path/argument
    /// rules), which are not represented in the command index.
    #[must_use]
    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }

    /// The indices (into [`Self::rules`]) of rules naming `normalized_command`, or an empty
    /// slice when none do.
    ///
    /// `normalized_command` must already be normalized by [`normalize_command`]; callers pass
    /// the normalized base name of the segment's command. Returns an empty slice for an unknown
    /// command, which the engine treats as "no command-named rule matched" (its own unknown →
    /// Tier 3 handling applies, Req 1.6/3.1).
    #[must_use]
    pub fn candidates(&self, normalized_command: &str) -> &[usize] {
        self.by_command
            .get(normalized_command)
            .map_or(&[], Vec::as_slice)
    }
}

/// Normalize a command string to its comparable base name (Req 1.2).
///
/// Mirrors the normalization the issue #27 placeholder classifier used, so a rule naming `reg`
/// matches `C:\Windows\System32\reg.exe` and `/usr/bin/rm` matches a rule naming `rm`: the input
/// is trimmed, reduced to the final path component (after the last `/` or `\`), stripped of a
/// trailing `.exe`, and lower-cased. Exposed to the crate so the engine normalizes a segment's
/// command the same way the index was built.
#[must_use]
pub(crate) fn normalize_command(command: &str) -> String {
    let trimmed = command.trim();
    let last = trimmed.rsplit(['/', '\\']).next().unwrap_or(trimmed);
    let base = last.strip_suffix(".exe").unwrap_or(last);
    base.to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_strips_path_exe_and_lowercases() {
        assert_eq!(normalize_command(r"C:\Windows\System32\reg.exe"), "reg");
        assert_eq!(normalize_command("/usr/bin/RM"), "rm");
        assert_eq!(normalize_command("  Git  "), "git");
    }

    #[test]
    fn load_accepts_a_well_formed_table_and_indexes_commands() {
        let toml_text = r#"
            [[rule]]
            id = "fs.read.ls"
            tier = 0
            [rule.match]
            command = ["ls", "dir"]

            [[rule]]
            id = "fs.delete.recursive-force"
            tier = 3
            [rule.match]
            command = ["rm"]
            args_all = ["-rf"]

            [[rule]]
            id = "path.only.rule"
            tier = 2
            [rule.match]
            path = { targets = "all-non-flag", destructive = true }
        "#;

        let Ok(table) = RulesTable::load(toml_text) else {
            unreachable!("well-formed table should load");
        };
        assert_eq!(table.rules().len(), 3);

        // Command-named rules are indexed under their normalized names.
        assert_eq!(table.candidates("ls"), &[0]);
        assert_eq!(table.candidates("dir"), &[0]);
        assert_eq!(table.candidates("rm"), &[1]);
        // An unknown command yields no candidates.
        assert!(table.candidates("unknown").is_empty());
        // The command-less (path-only) rule is reachable only via rules(), not the index.
        assert_eq!(table.rules()[2].id, RuleId("path.only.rule".to_string()));
    }

    #[test]
    fn load_rejects_a_tier_out_of_range_naming_the_rule() {
        let toml_text = r#"
            [[rule]]
            id = "bad.tier"
            tier = 4
            [rule.match]
            command = ["rm"]
        "#;

        let result = RulesTable::load(toml_text);
        let Err(LoadError::MalformedRule { id, detail }) = result else {
            unreachable!("expected MalformedRule, got {result:?}");
        };
        assert_eq!(id, RuleId("bad.tier".to_string()));
        assert!(
            detail.contains('4'),
            "detail should name the bad tier: {detail}"
        );
    }

    #[test]
    fn load_rejects_a_duplicate_rule_id_naming_the_id() {
        let toml_text = r#"
            [[rule]]
            id = "dup"
            tier = 0
            [rule.match]
            command = ["ls"]

            [[rule]]
            id = "dup"
            tier = 1
            [rule.match]
            command = ["mkdir"]
        "#;

        let result = RulesTable::load(toml_text);
        let Err(LoadError::DuplicateRuleId(id)) = result else {
            unreachable!("expected DuplicateRuleId, got {result:?}");
        };
        assert_eq!(id, RuleId("dup".to_string()));
    }

    #[test]
    fn load_maps_a_parse_error_to_parse() {
        // Unknown field under a rule is rejected by deny_unknown_fields → Parse.
        let toml_text = r#"
            [[rule]]
            id = "x"
            tier = 0
            bogus = true
            [rule.match]
            command = ["ls"]
        "#;

        assert!(matches!(
            RulesTable::load(toml_text),
            Err(LoadError::Parse(_))
        ));
    }

    #[test]
    fn embedded_rules_table_loads_and_is_well_formed() {
        // The checked-in `rules.toml` is compiled in via `include_str!` and must load cleanly,
        // assign only in-range tiers, carry the expected rule ids, and index real commands
        // (Req 1.1, 1.7).
        let Ok(table) = RulesTable::load(include_str!("data/rules.toml")) else {
            unreachable!("embedded rules.toml should load");
        };

        // A real, non-empty table.
        assert!(
            !table.rules().is_empty(),
            "embedded table should contain rules"
        );

        // Every rule assigns a tier within 0..=3 (Req 1.3).
        for rule in table.rules() {
            assert!(
                (MIN_TIER..=MAX_TIER).contains(&rule.tier),
                "rule {:?} has out-of-range tier {}",
                rule.id,
                rule.tier
            );
        }

        // A representative set of the ids authored in data/rules.toml is present.
        let present: std::collections::HashSet<&str> = table
            .rules()
            .iter()
            .map(|rule| rule.id.0.as_str())
            .collect();
        for expected in [
            "fs.delete.rm-recursive-force",
            "disk.format",
            "disk.diskpart",
            "registry.reg",
            "net.netsh",
            "git.push",
            "shell.powershell.encoded",
            "install.npm-global",
            "fs.write.destructive-outside",
            "fs.read.listing",
            "fs.write.create",
        ] {
            assert!(
                present.contains(expected),
                "expected rule id {expected:?} missing from embedded table"
            );
        }

        // Indexing works on real data: representative commands resolve to candidate rules.
        assert!(
            !table.candidates("rm").is_empty(),
            "`rm` should resolve to at least one candidate rule"
        );
        assert!(
            !table.candidates("netsh").is_empty(),
            "`netsh` should resolve to at least one candidate rule"
        );
    }

    // Feature: permission-tiers, Property 7: load validation accepts well-formed tables and
    // rejects malformed or duplicate-id ones.
    //
    // Over randomly generated rule tables — spanning in-range (0..=3) and out-of-range tiers,
    // and unique and duplicate ids — serialized to TOML and fed to `RulesTable::load`, the
    // loader succeeds if and only if every rule has a tier in `0..=3` and all ids are unique.
    // Otherwise it rejects the whole table with a `LoadError` that names the first offender, in
    // file order, mirroring the loader's per-rule precedence (tier range checked before
    // duplicate id). A rejected table yields no `RulesTable`, so no classification can be
    // produced from it (holds by construction: the `Err` branch carries no table).
    // (Validates: Requirements 1.3, 1.8).
    mod property_load_validation {
        use super::*;
        use proptest::prelude::*;

        /// One generated rule: a simple id, a tier spanning valid and invalid values, and a
        /// single command name. Kept to lower-case ASCII so TOML serialization needs no escaping.
        #[derive(Clone, Debug)]
        struct GenRule {
            id: String,
            tier: u8,
            command: String,
        }

        /// Generate a rule whose id is drawn from a small pool (so duplicates arise naturally),
        /// whose tier spans both the valid `0..=3` range and the invalid `4..=6` range, and whose
        /// command is a short lower-case token.
        fn any_rule() -> impl Strategy<Value = GenRule> {
            (
                "[a-e]",      // small id alphabet → duplicates occur frequently
                0u8..=6,      // spans valid 0..=3 and invalid 4..=6
                "[a-z]{1,8}", // simple command token, no escaping needed
            )
                .prop_map(|(id, tier, command)| GenRule { id, tier, command })
        }

        /// Serialize generated rules to the `rules.toml` wire form the loader parses.
        fn to_toml(rules: &[GenRule]) -> String {
            let mut text = String::new();
            for rule in rules {
                text.push_str("[[rule]]\n");
                text.push_str(&format!("id = \"{}\"\n", rule.id));
                text.push_str(&format!("tier = {}\n", rule.tier));
                text.push_str("[rule.match]\n");
                text.push_str(&format!("command = [\"{}\"]\n\n", rule.command));
            }
            text
        }

        /// The precise oracle: replicate the loader's file-order precedence (for each rule, check
        /// the tier range first, then the duplicate id) and return the expected outcome.
        enum Expected {
            Ok,
            Malformed(String),
            Duplicate(String),
        }

        fn expected_outcome(rules: &[GenRule]) -> Expected {
            let mut seen = std::collections::HashSet::new();
            for rule in rules {
                if !(MIN_TIER..=MAX_TIER).contains(&rule.tier) {
                    return Expected::Malformed(rule.id.clone());
                }
                if !seen.insert(rule.id.clone()) {
                    return Expected::Duplicate(rule.id.clone());
                }
            }
            Expected::Ok
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(256))]

            #[test]
            fn load_succeeds_iff_well_formed_and_unique_else_names_offender(
                // At least one rule: an empty vector would serialize to TOML with no `[[rule]]`
                // entries, which the loader rejects as a `Parse` error (the `rule` field is
                // required) — an orthogonal case the unit tests already cover.
                rules in prop::collection::vec(any_rule(), 1..12),
            ) {
                let toml_text = to_toml(&rules);
                let result = RulesTable::load(&toml_text);

                match expected_outcome(&rules) {
                    Expected::Ok => {
                        // Well-formed and unique: the whole table loads, preserving every rule.
                        let Ok(table) = result else {
                            return Err(TestCaseError::fail(format!(
                                "well-formed table rejected: {result:?}"
                            )));
                        };
                        prop_assert_eq!(table.rules().len(), rules.len());
                    }
                    Expected::Malformed(offender) => {
                        // First offender (in file order) is a tier out of 0..=3: the whole table
                        // is rejected, no RulesTable is produced, and the error names the rule.
                        prop_assert!(result.is_err());
                        let Err(LoadError::MalformedRule { id, .. }) = result else {
                            return Err(TestCaseError::fail(format!(
                                "expected MalformedRule, got {result:?}"
                            )));
                        };
                        prop_assert_eq!(id, RuleId(offender));
                    }
                    Expected::Duplicate(offender) => {
                        // First repeated id (in file order), with every prior rule in range:
                        // the whole table is rejected and the error names the duplicated id.
                        prop_assert!(result.is_err());
                        let Err(LoadError::DuplicateRuleId(id)) = result else {
                            return Err(TestCaseError::fail(format!(
                                "expected DuplicateRuleId, got {result:?}"
                            )));
                        };
                        prop_assert_eq!(id, RuleId(offender));
                    }
                }
            }
        }
    }
}
