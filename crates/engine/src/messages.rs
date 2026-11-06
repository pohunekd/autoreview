//! Text of the messages a review sends to the owner.

// Rust guideline compliant 2026-06-26

use time::OffsetDateTime;

use crate::integrations::{PullRequestState, SessionRequest};
use crate::model::review::PRIOR_ANSWERED;
use crate::model::{
    Artifact, Decision, Finding, PublishResult, PublishedRecord, PullRequest, ReviewInput,
    SessionIdentity,
};
use crate::publish::plan::{PublicationPlan, blocking_count};
use crate::publish::render::{head_prefix, human_review_text, reviewer_labels};
use crate::verify::{VERDICT_NOT_BLOCKING, VERDICT_REFUTED, demoted, escalates, verification_line};

/// Title of the approval request. An approval is accepted only as a reply to a message that
/// carries this title and the Review ID line.
pub(crate) const APPROVAL_REQUESTED_TITLE: &str =
    "GitHub review completed with no blocking findings";

/// Characters kept from the pull request title in the review header. A fixed size, not a tunable.
const HEADER_TITLE_CHARACTERS: i64 = 200;

/// Nanoseconds in one second, the unit of Go's `time.Duration`.
const NANOS_PER_SECOND: u64 = 1_000_000_000;

/// Nanoseconds in one hour, the unit of the handoff age.
const NANOS_PER_HOUR: i64 = 3_600_000_000_000;

/// Cuts a value to `limit` characters after joining its words.
///
/// The last kept character is an ellipsis. Config keeps the limit positive.
pub(crate) fn excerpt(value: &str, limit: i64) -> String {
    let words: Vec<&str> = value.split_whitespace().collect();
    let normalized = words.join(" ");
    let limit_usize = usize::try_from(limit).unwrap_or_default();
    if normalized.chars().count() > limit_usize {
        let kept: String = normalized
            .chars()
            .take(limit_usize.saturating_sub(1))
            .collect();
        format!("{}…", kept.trim())
    } else {
        normalized
    }
}

/// The first lines of a review message: its title, the Review ID, the pull request, the reviewers
/// and the scope.
fn review_header(title: &str, input: &ReviewInput) -> String {
    let pr = &input.pull_request;
    format!(
        "{}\n\nReview ID: {}\nPR: {}\nTitle: {}\nReviewers: {}\nScope: {}",
        title,
        pr.review_id(),
        pr.url,
        excerpt(&pr.title, HEADER_TITLE_CHARACTERS),
        reviewer_labels(&input.reviewers),
        input.scope.mode
    )
}

/// The message that announces the start of an attempt.
#[expect(dead_code, reason = "called by S26")]
pub(crate) fn started_message(input: &ReviewInput) -> String {
    format!(
        "{}\nAttempt: {}",
        review_header("GitHub review started", input),
        input.attempt.wrapping_add(1)
    )
}

/// One line per reviewer summary, each cut to `limit` characters.
fn summary_lines(artifact: &Artifact, limit: i64) -> String {
    let lines: Vec<String> = artifact
        .summaries
        .iter()
        .map(|summary| format!("{}: {}", summary.label, excerpt(&summary.summary, limit)))
        .collect();
    lines.join("\n")
}

/// One line per finding, with its kind: a note unless the finding blocks the review, and the
/// verifier's verdict when it demoted the finding.
fn finding_lines(input: &ReviewInput, findings: &[Finding], limit: i64) -> String {
    let lines: Vec<String> = findings
        .iter()
        .map(|finding| {
            let mut kind = finding.severity.to_ascii_uppercase();
            if finding.verdict == VERDICT_REFUTED {
                kind = format!("NOTE, REFUTED BY VERIFICATION, {kind}");
            } else if finding.verdict == VERDICT_NOT_BLOCKING {
                kind = format!("NOTE, NOT BLOCKING BY VERIFICATION, {kind}");
            } else if !input.blocks(finding) {
                kind = format!("NOTE, {kind}");
            }
            format!(
                "{} [{}, confidence: {}] {}:{} {}",
                finding.id,
                kind,
                finding.confidence,
                finding.path,
                finding.line,
                excerpt(&finding.title, limit)
            )
        })
        .collect();
    lines.join("\n")
}

/// The approval request sent when a clean review waits for the owner.
///
/// The reply instruction precedes the notes, so a cut at the limit keeps it.
#[cfg_attr(not(test), expect(dead_code, reason = "called by S29"))]
pub(crate) fn approval_requested_message(
    excerpt_characters: i64,
    input: &ReviewInput,
    artifact: &Artifact,
) -> String {
    let (_, notes) = input.split_findings(&artifact.findings);
    let mut message = format!(
        concat!(
            "{}\nBlocking findings: 0\nNon-blocking notes: {}\n\n",
            "Reply to this message with: approve\n",
            "Nothing has been posted to GitHub. ",
            "The approval expires after {}.\n\nSummary:\n{}"
        ),
        review_header(APPROVAL_REQUESTED_TITLE, input),
        notes.len(),
        approval_window(input.approval_timeout_seconds),
        summary_lines(artifact, excerpt_characters)
    );
    if let Some(handoff) = &input.handoff {
        message.push_str(&format!(
            "\nThe author requested the review again after the notes were handed to them at `{}`.",
            head_prefix(&handoff.head_sha)
        ));
    }
    let line = verification_line(artifact.verification.as_ref());
    if !line.is_empty() {
        message.push('\n');
        message.push_str(&line);
    }
    if !artifact.human_review.is_empty() {
        message.push_str(concat!(
            "\nThis rule approves clean reviews without you, ",
            "but the pull request matched its human review signals: "
        ));
        message.push_str(&human_review_text(
            &artifact.human_review,
            excerpt_characters,
        ));
        message.push('.');
    }
    if escalates(input, artifact) {
        let demoted_findings: Vec<Finding> = notes
            .iter()
            .filter(|finding| demoted(finding))
            .cloned()
            .collect();
        message.push_str(concat!(
            "\nThis rule approves clean reviews without you, ",
            "but the verification demoted these blocking findings, ",
            "check before approving:\n"
        ));
        message.push_str(&finding_lines(input, &demoted_findings, excerpt_characters));
    }
    let answered = answered_lines(artifact, excerpt_characters);
    if !answered.is_empty() {
        message.push_str(concat!(
            "\nEarlier findings settled in the discussion, ",
            "check before approving:\n"
        ));
        message.push_str(&answered);
    }
    if !notes.is_empty() {
        message.push_str("\nNotes:\n");
        message.push_str(&finding_lines(input, &notes, excerpt_characters));
    }
    message
}

/// One line per prior finding that the discussion settled.
fn answered_lines(artifact: &Artifact, limit: i64) -> String {
    let lines: Vec<String> = artifact
        .prior_results
        .iter()
        .filter(|result| result.verdict.status == PRIOR_ANSWERED)
        .map(|result| {
            let basis = if result.verdict.relies_on.is_empty() {
                "the code".to_owned()
            } else {
                format!("a comment by {}", result.verdict.relies_on)
            };
            format!(
                "{} {} (relies on {}): {}",
                result.finding.key,
                excerpt(&result.finding.title, limit),
                basis,
                excerpt(&result.verdict.note, limit)
            )
        })
        .collect();
    lines.join("\n")
}

/// The message that reports a published review and its decision.
#[expect(dead_code, reason = "called by S28")]
pub(crate) fn published_message(
    excerpt_characters: i64,
    input: &ReviewInput,
    artifact: &Artifact,
    plan: &PublicationPlan,
    result: &PublishResult,
) -> String {
    let pr = &input.pull_request;
    let (_, notes) = input.split_findings(&artifact.findings);
    let blocking = blocking_count(input, artifact);
    let title = if plan.decision == Decision::Approve {
        format!(
            "Approved {} PR #{} after a review without blocking findings",
            pr.repository, pr.number
        )
    } else if plan.decision == Decision::RequestChanges {
        format!(
            "Requested changes on {} PR #{} with {} blocking finding(s)",
            pr.repository, pr.number, blocking
        )
    } else if plan.notes_to_author {
        format!(
            concat!(
                "Handed {} non-blocking note(s) on {} PR #{} to the author; ",
                "their next review request asks for your approval"
            ),
            notes.len(),
            pr.repository,
            pr.number
        )
    } else {
        format!(
            "Commented on {} PR #{} with {} blocking finding(s)",
            pr.repository, pr.number, blocking
        )
    };
    let mut title = title;
    if !notes.is_empty() && !plan.notes_to_author {
        title.push_str(&format!(" and {} non-blocking note(s)", notes.len()));
    }
    let mut message = format!(
        "{}\n\nReview ID: {}\nReview: {}\nReviewers: {}\nScope: {}",
        title,
        pr.review_id(),
        result.review_url,
        reviewer_labels(&input.reviewers),
        artifact.scope.mode
    );
    let line = verification_line(artifact.verification.as_ref());
    if !line.is_empty() {
        message.push('\n');
        message.push_str(&line);
    }
    let open = artifact.open_prior();
    if !open.is_empty() {
        let lines: Vec<String> = open
            .iter()
            .map(|result| {
                format!(
                    "{} [STILL OPEN] {}:{} {}",
                    result.finding.key,
                    result.finding.path,
                    result.finding.line,
                    excerpt(&result.finding.title, excerpt_characters)
                )
            })
            .collect();
        message.push_str("\n\n");
        message.push_str(&lines.join("\n"));
    }
    if !artifact.findings.is_empty() {
        message.push_str("\n\n");
        message.push_str(&finding_lines(
            input,
            &artifact.findings,
            excerpt_characters,
        ));
    } else {
        message.push_str("\nSummary:\n");
        message.push_str(&summary_lines(artifact, excerpt_characters));
    }
    message
}

/// States the approval timeout in hours, or in duration text when not whole.
pub(crate) fn approval_window(seconds: i64) -> String {
    if seconds % 3600 == 0 {
        format!("{} hours", seconds / 3600)
    } else {
        go_duration(seconds.wrapping_mul(NANOS_PER_SECOND as i64))
    }
}

/// Formats nanoseconds as Go's `time.Duration.String` does, such as `1h30m0s` or `1.5s`.
fn go_duration(nanos: i64) -> String {
    if nanos == 0 {
        return "0s".to_owned();
    }
    let neg = nanos < 0;
    let value = nanos.unsigned_abs();
    let inner = if value < NANOS_PER_SECOND {
        sub_second_duration(value)
    } else {
        long_duration(value)
    };
    if neg { format!("-{inner}") } else { inner }
}

/// Formats a non-negative duration below one second with the unit Go picks for it.
fn sub_second_duration(value: u64) -> String {
    if value == 0 {
        return "0s".to_owned();
    }
    let (precision, suffix) = if value < 1_000 {
        (0, "ns")
    } else if value < 1_000_000 {
        (3, "µs")
    } else {
        (6, "ms")
    };
    let (integer, fraction) = split_fraction(value, precision);
    format!("{integer}{fraction}{suffix}")
}

/// Formats a duration of one second or more as `XhYmZs`, omitting zero parts.
fn long_duration(value: u64) -> String {
    let (total_seconds, fraction) = split_fraction(value, 9);
    let seconds = total_seconds % 60;
    let total_minutes = total_seconds / 60;
    let minutes = total_minutes % 60;
    let hours = total_minutes / 60;
    let mut out = String::new();
    if hours > 0 {
        out.push_str(&format!("{hours}h"));
    }
    if total_minutes > 0 {
        out.push_str(&format!("{minutes}m"));
    }
    out.push_str(&format!("{seconds}{fraction}s"));
    out
}

/// Splits a value into its integer part and a trimmed fraction of `precision` digits.
/// The fraction is empty when it is zero, so no decimal point is written.
fn split_fraction(value: u64, precision: u32) -> (u64, String) {
    let divisor = 10_u64.pow(precision);
    let integer = value / divisor;
    let remainder = value % divisor;
    if remainder == 0 {
        (integer, String::new())
    } else {
        let width = precision as usize;
        let mut digits = format!("{remainder:0>width$}");
        while digits.ends_with('0') {
            digits.pop();
        }
        (integer, format!(".{digits}"))
    }
}

/// The message that reports a review superseded by a newer one.
pub(crate) fn superseded_message(input: &ReviewInput, code: &str) -> String {
    format!(
        "Review {} was superseded ({}). Nothing was posted to GitHub.",
        input.pull_request.review_id(),
        code
    )
}

/// The message that reports an approval that expired without a reply.
#[expect(dead_code, reason = "called by S29")]
pub(crate) fn approval_expired_message(input: &ReviewInput) -> String {
    format!(
        concat!(
            "Approval for review {} expired after {} without a reply. ",
            "Nothing was posted to GitHub. ",
            "A new commit or a renewed review request starts a new review."
        ),
        input.pull_request.review_id(),
        approval_window(input.approval_timeout_seconds)
    )
}

/// The message that reports a review stopped after the owner approved it.
pub(crate) fn approved_review_failed_message(input: &ReviewInput, code: &str) -> String {
    format!(
        concat!(
            "Review for {} at {} stopped after your approval: ",
            "it was not published ({}). Nothing was posted to GitHub. ",
            "Retry this PR to review and publish it again."
        ),
        input.pull_request.key(),
        head_prefix(&input.pull_request.head_sha),
        code
    )
}

/// The message that asks the owner to remove a possibly started session.
pub(crate) fn orphaned_session_message(request: &SessionRequest, code: &str) -> String {
    format!(
        concat!(
            "Pohunek may have started reviewer session {} on {} for review {} ",
            "after the start request failed, and it could not be found ",
            "or removed ({}). Check `pohunek --host {} session list --json` and ",
            "remove the session if it exists."
        ),
        request.name(),
        request.route.host,
        request.pull_request.review_id(),
        code,
        request.route.host
    )
}

/// One session that could not be removed, with its allowlisted failure code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionCleanupFailure {
    /// The session that stays on the host.
    pub(crate) session: SessionIdentity,
    /// The allowlisted code of the failed removal.
    pub(crate) code: String,
}

/// The message that lists reviewer sessions which could not be removed.
///
/// It names only identifiers and allowlisted codes, never raw CLI output.
pub(crate) fn session_cleanup_failed_message(
    pr: &PullRequest,
    failures: &[SessionCleanupFailure],
    attempts: i64,
) -> String {
    let details: Vec<String> = failures
        .iter()
        .map(|failure| {
            format!(
                "{} session {} on {} ({})",
                failure.session.reviewer_id,
                failure.session.session_id,
                failure.session.host,
                failure.code
            )
        })
        .collect();
    format!(
        concat!(
            "Pohunek could not remove reviewer sessions of review {} after ",
            "{} attempts: {}. The review continues and the sessions stay on ",
            "the host until removed. Run `pohunek --host <host> session rm ",
            "<session> --json` to list the processes that block removal; ",
            "when none belongs to the session, add ",
            "--accept-unconfirmed-cleanup."
        ),
        pr.review_id(),
        attempts,
        details.join("; ")
    )
}

/// States how long notes have waited for their author, in whole hours.
#[expect(dead_code, reason = "called by S23")]
pub(crate) fn handoff_age(record: &PublishedRecord, now: OffsetDateTime) -> String {
    format!("{} h", go_hours(go_elapsed_nanos(now, record.published_at)))
}

/// Nanoseconds from `earlier` to `later`, saturated to the range of `time.Duration` as Go's
/// `Time.Sub` saturates it.
pub(crate) fn go_elapsed_nanos(later: OffsetDateTime, earlier: OffsetDateTime) -> i64 {
    let diff = later.unix_timestamp_nanos() - earlier.unix_timestamp_nanos();
    if diff > i64::MAX as i128 {
        i64::MAX
    } else if diff < i64::MIN as i128 {
        i64::MIN
    } else {
        diff as i64
    }
}

/// Hours of a duration as Go's `Duration.Hours` computes them, truncated as Go's `int` does.
fn go_hours(nanos: i64) -> i64 {
    let hours = nanos / NANOS_PER_HOUR;
    let rest = nanos % NANOS_PER_HOUR;
    (hours as f64 + rest as f64 / NANOS_PER_HOUR as f64) as i64
}

/// The message that reminds the owner of notes that wait for their author.
///
/// It carries no Review ID: nothing can be approved by replying to it while the review waits for
/// its author.
#[expect(dead_code, reason = "called by S23")]
pub(crate) fn handoff_reminder_message(
    excerpt_characters: i64,
    record: &PublishedRecord,
    current: &PullRequestState,
    now: OffsetDateTime,
) -> String {
    format!(
        concat!(
            "Non-blocking notes on {} PR #{} wait for the author for {}\n\n",
            "{} (by {})\n{}\n\n",
            "The author has not requested the review again. Remind them, ",
            "or review the pull request on GitHub yourself."
        ),
        record.repository,
        record.number,
        handoff_age(record, now),
        excerpt(&current.title, excerpt_characters),
        current.author,
        current.url
    )
}
