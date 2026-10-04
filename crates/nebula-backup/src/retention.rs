//! Which backups to keep: the newest N, plus the newest of each recent day, ISO week and
//! month. Pure, so the schedule is tested without a clock or a cloud.

use std::collections::BTreeSet;

use nebula_config::BackupConfig;
use time::OffsetDateTime;

/// Counts per tier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Policy {
    /// Newest backups kept regardless of age.
    pub recent: usize,
    /// Days.
    pub daily: usize,
    /// ISO weeks.
    pub weekly: usize,
    /// Months.
    pub monthly: usize,
}

impl Policy {
    /// From `[backup]`.
    #[must_use]
    pub const fn from_config(c: &BackupConfig) -> Self {
        Self {
            recent: c.keep_recent,
            daily: c.keep_daily,
            weekly: c.keep_weekly,
            monthly: c.keep_monthly,
        }
    }
}

/// The newest time in each of the first `n` distinct buckets, walking newest first.
fn newest_per_bucket<K: PartialEq>(
    sorted_desc: &[OffsetDateTime],
    n: usize,
    key: impl Fn(OffsetDateTime) -> K,
    keep: &mut BTreeSet<OffsetDateTime>,
) {
    let mut last: Option<K> = None;
    let mut buckets = 0;
    for &t in sorted_desc {
        if buckets == n {
            break;
        }
        let k = key(t);
        if last.as_ref() != Some(&k) {
            keep.insert(t);
            buckets += 1;
            last = Some(k);
        }
    }
}

/// The backups to keep; everything else may be deleted.
#[must_use]
pub fn keep(times: &[OffsetDateTime], p: Policy) -> BTreeSet<OffsetDateTime> {
    let mut sorted: Vec<OffsetDateTime> = times.to_vec();
    sorted.sort_unstable_by(|a, b| b.cmp(a));
    sorted.dedup();
    let mut keep: BTreeSet<OffsetDateTime> = sorted.iter().take(p.recent).copied().collect();
    newest_per_bucket(&sorted, p.daily, OffsetDateTime::date, &mut keep);
    newest_per_bucket(
        &sorted,
        p.weekly,
        |t| {
            let (y, w, _) = t.to_iso_week_date();
            (y, w)
        },
        &mut keep,
    );
    newest_per_bucket(&sorted, p.monthly, |t| (t.year(), t.month()), &mut keep);
    keep
}
