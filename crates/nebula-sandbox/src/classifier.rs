//! The object-safe `CommandClassifier` trait: the seam over which a command and its arguments
//! are classified into a tier and an optional approval, storable as `Arc<dyn CommandClassifier>`.
//!
//! Relocated from `nebula-tools/src/permit.rs`: this crate now owns the classifier seam, and
//! `nebula-tools` re-exports it. The trait signature is kept unchanged so `shell.run` keeps
//! working without touching any issue #27 call site.

use crate::approval::Approval;
use crate::tier::Tier;

/// Classifies a shell command and reports the approval decision for it.
///
/// This is the injectable boundary `shell.run` consults before starting any child process
/// (Requirement 4.2, 4.11). The real engine (issue #28) implements it; `RulesClassifier` is the
/// data-driven implementation that supersedes the issue #27 `DefaultClassifier`. The trait is
/// object-safe so it can be stored as `Arc<dyn CommandClassifier>` on the tool context.
pub trait CommandClassifier: Send + Sync {
    /// Return the [`Tier`] and any [`Approval`] decision for `command` with `args`.
    ///
    /// A returned `Some(Approval)` authorizes execution of a command classified strictly above
    /// the [`NO_APPROVAL_THRESHOLD`]; `None` means no approval was granted and such a command
    /// must be refused.
    ///
    /// [`NO_APPROVAL_THRESHOLD`]: crate::tier::NO_APPROVAL_THRESHOLD
    fn classify(&self, command: &str, args: &[String]) -> (Tier, Option<Approval>);
}
