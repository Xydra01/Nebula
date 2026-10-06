//! The permission tier vocabulary: the [`Tier`] ordinal, the no-approval threshold, and the
//! [`effective_tier`] merge of a classifier opinion with an advisory tier.
//!
//! Relocated from `nebula-tools/src/permit.rs`: this crate now owns the permission-tier
//! vocabulary, and `nebula-tools` re-exports it. The definitions and [`Tier`] ordering are
//! kept identical so [`effective_tier`] still never lowers an enforced tier below the
//! classifier's floor.

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

/// The highest [`Tier`] that runs without an approval decision (Tier 1, [`Tier::Sandbox`]).
///
/// Any command classified strictly above this threshold ([`Tier::Workspace`] or
/// [`Tier::System`]) must be refused unless an approval is present.
pub const NO_APPROVAL_THRESHOLD: Tier = Tier::Sandbox;

/// Combine the classifier tier with a model second opinion into the tier `shell.run` enforces
/// (Requirement 4.12, design 7.2).
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

#[cfg(test)]
mod tests {
    use super::*;

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

    // Feature: permission-tiers, Property 12: a second opinion never lowers the enforced tier
    //
    // Property 12: A second opinion never lowers the enforced tier. For any classifier tier and
    // any advisory (model second-opinion) tier, the enforced tier is the maximum of the two and
    // is never below the classifier's floor: an advisory input may escalate caution but can
    // never downgrade the classifier's tier. This preserves the guarantee of issue #27's
    // Property 6 now that `nebula-sandbox` owns the permission-tier vocabulary.
    // (Validates: Requirements 8.3, 8.4, 8.5).
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
}
