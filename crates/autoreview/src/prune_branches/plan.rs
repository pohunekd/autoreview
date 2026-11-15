//! Which review branches of one repository `prune-branches` deletes, decided on plain data.

// Rust guideline compliant 2026-11-14

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use autoreview_engine::model::WORKFLOW_ID_PREFIX;
use autoreview_engine::store::StoredReview;
use autoreview_engine::store::reviews::REVIEW_COMPLETED;
use serde::Serialize;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

/// Prefix of the refs of local branches.
pub(crate) const HEADS: &str = "refs/heads/";

/// Text before the ref of a `branch` field of `git worktree list --porcelain -z`.
const WORKTREE_BRANCH: &str = "branch ";

/// Hex digits of the digest that follows [`WORKFLOW_ID_PREFIX`] in a workflow ID.
const WORKFLOW_DIGEST_LEN: usize = 64;

/// What the store says about the reviews that may own a branch.
#[derive(Debug, Default)]
pub(crate) struct Owners {
    /// Workflow IDs of the reviews that may still use a branch.
    active: BTreeSet<String>,
    /// Last update of every other review by workflow ID; `None` when it is not a time.
    finished: BTreeMap<String, Option<OffsetDateTime>>,
}

impl Owners {
    /// Sorts every stored review into active and finished.
    ///
    /// A review is finished when it is completed and its cleanup is not pending; any other
    /// status, also one this build does not know, is active.
    pub(crate) fn from_reviews(reviews: &[StoredReview]) -> Self {
        let mut owners = Self::default();
        for review in reviews {
            if review.status == REVIEW_COMPLETED && !review.cleanup_pending {
                let updated = OffsetDateTime::parse(&review.updated_at, &Rfc3339).ok();
                owners.finished.insert(review.workflow_id.clone(), updated);
            } else {
                owners.active.insert(review.workflow_id.clone());
            }
        }
        owners
    }
}

/// What `plan` needs besides the git output of the repository.
#[derive(Debug)]
pub(crate) struct Rules<'a> {
    /// `session.branch_prefix`; only branches under it are listed.
    pub(crate) prefix: &'a str,
    /// The reviews of the store.
    pub(crate) owners: &'a Owners,
    /// The branch of every pohunek session of the host.
    pub(crate) sessions: &'a BTreeSet<String>,
    /// `--older-than-days` as a duration; `None` lists every age.
    pub(crate) older_than: Option<Duration>,
    /// `store.review_retention_seconds`, the least age of a review the store no longer holds.
    pub(crate) retention: Duration,
    /// The time the ages are measured at.
    pub(crate) now: OffsetDateTime,
}

/// What happens to one branch under the prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Verdict {
    /// No review, session or worktree needs the branch; `--yes` deletes it.
    Delete,
    /// The name does not hold a workflow ID after the prefix.
    NameUnknown,
    /// Its review is queued, running, awaiting or still being cleaned up.
    ReviewActive,
    /// A pohunek session of the host works on it.
    SessionLive,
    /// A worktree of the repository has it checked out.
    CheckedOut,
    /// Its review finished less than `--older-than-days` ago.
    TooRecent,
    /// Its age is unknown: the last update is not a time, or the review is gone from the store
    /// and `--older-than-days` is longer than the retention.
    AgeUnknown,
}

/// One branch under the prefix, the commit it points at and its verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct BranchPlan {
    /// Branch name without `refs/heads/`.
    pub(crate) branch: String,
    /// Commit the branch points at; `git branch <branch> <commit>` restores it.
    pub(crate) commit: String,
    /// What happens to the branch.
    pub(crate) verdict: Verdict,
    /// Whether `--yes` deleted the branch.
    pub(crate) deleted: bool,
    /// Error code of a failed deletion.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) error: String,
}
