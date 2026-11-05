//! The pure part of the review queue: the engine state of each request, its order and the digest.

// Rust guideline compliant 2026-06-26

use std::collections::BTreeMap;

use autoreview_policy::{Policy, TriggerKind};
use autoreview_schedule::scheduler::Record;
use serde::Serialize;

use crate::integrations::ReviewRequest;
use crate::model::{AwaitRecord, PublishedRecord};
use crate::publish::decision_text;

/// Note that closes a digest whose list does not show every request.
const TRUNCATED_TEXT: &str = "The list is truncated: more requests wait for you than are shown.";

/// Position of a queue entry. The derived order puts the requests that need the owner's action
/// first, and the notes that wait for their author after the requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum QueuePriority {
    /// The engine waits for the owner's approval.
    AwaitingApproval,
    /// The policy does not cover the repository with a review-request trigger.
    NotCovered,
    /// The last review attempt stopped with an error.
    Stopped,
    /// The engine works on the request or has finished it.
    Other,
    /// Notes wait for their author, who has to act first.
    WaitingAuthor,
}

/// The result the review-queue command prints.
#[expect(dead_code, reason = "called by S34")]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct QueueResult {
    /// Requests listed in the digest.
    pub count: i64,
    /// Whether the digest was sent.
    pub sent: bool,
    /// Whether requests that did not fit were left out.
    pub truncated: bool,
}

/// One request of the queue with the engine state that explains it.
#[cfg_attr(not(test), expect(dead_code, reason = "called by S34"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct QueueEntry {
    /// The request as GitHub reported it.
    pub(crate) request: ReviewRequest,
    /// The status line of the digest.
    pub(crate) status: String,
    /// The position of the entry in the digest order.
    pub(crate) priority: QueuePriority,
}

/// The engine state the queue reads, each map keyed by `repository#number`.
#[cfg_attr(not(test), expect(dead_code, reason = "called by S34"))]
#[derive(Debug, Clone, Default)]
pub(crate) struct QueueState {
    /// Review schedule records.
    pub(crate) schedule: BTreeMap<String, Record>,
    /// Published review records.
    pub(crate) published: BTreeMap<String, PublishedRecord>,
    /// Approvals that wait for the owner.
    pub(crate) awaiting: BTreeMap<String, AwaitRecord>,
}

/// The digest bodies and the count of requests they show.
#[cfg_attr(not(test), expect(dead_code, reason = "called by S34"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RenderedQueue {
    /// The plain-text body.
    pub(crate) plain: String,
    /// The HTML body, which never cuts an entry in half.
    pub(crate) formatted: String,
    /// Requests shown in both bodies.
    pub(crate) shown: usize,
    /// Whether requests that did not fit were left out.
    pub(crate) dropped: bool,
}

/// Annotates each request with its engine state and sorts the entries.
/// The order is priority, then repository and number.
#[cfg_attr(not(test), expect(dead_code, reason = "called by S34"))]
pub(crate) fn queue_entries(
    policy: &Policy,
    state: &QueueState,
    requests: &[ReviewRequest],
) -> Vec<QueueEntry> {
    let mut entries = Vec::with_capacity(requests.len());
    for request in requests {
        let key = format!("{}#{}", request.repository, request.number);
        let (status, priority) = queue_status(policy, state, request, &key);
        entries.push(QueueEntry {
            request: request.clone(),
            status,
            priority,
        });
    }
    entries.sort_by(|a, b| {
        a.priority
            .cmp(&b.priority)
            .then_with(|| a.request.repository.cmp(&b.request.repository))
            .then_with(|| a.request.number.cmp(&b.request.number))
    });
    entries
}

/// Explains where the engine stands with one request.
///
/// The serving engine keeps the state records current, so they are read as stored.
pub(crate) fn queue_status(
    policy: &Policy,
    state: &QueueState,
    request: &ReviewRequest,
    key: &str,
) -> (String, QueuePriority) {
    if state.awaiting.contains_key(key) {
        return (
            "AI review clean, waiting for your approve".to_owned(),
            QueuePriority::AwaitingApproval,
        );
    }
    if let Some(record) = state.schedule.get(key)
        && record.phase != "completed"
    {
        return schedule_status(record);
    }
    if let Some(record) = state.published.get(key) {
        return (
            format!("AI review published {}", decision_text(record.decision)),
            QueuePriority::Other,
        );
    }
    let covered = policy
        .resolve(&request.repository)
        .is_some_and(|rule| rule.trigger.kind == TriggerKind::ReviewRequested);
    if !covered {
        return (
            "not covered by AI review".to_owned(),
            QueuePriority::NotCovered,
        );
    }
    if state.schedule.contains_key(key) {
        return ("AI review completed".to_owned(), QueuePriority::Other);
    }
    ("AI review not started yet".to_owned(), QueuePriority::Other)
}

/// The status line and priority of a schedule record that has not completed.
fn schedule_status(record: &Record) -> (String, QueuePriority) {
    match record.phase.as_str() {
        "running" => ("AI review running".to_owned(), QueuePriority::Other),
        "waiting" | "retry_wait" => ("AI review queued".to_owned(), QueuePriority::Other),
        "failed" => {
            let code = if record.error_code.is_empty() {
                "unknown"
            } else {
                record.error_code.as_str()
            };
            (format!("AI review stopped: {code}"), QueuePriority::Stopped)
        }
        "withdrawn" => ("AI review withdrawn".to_owned(), QueuePriority::Other),
        _ => (
            format!("AI review state unknown: {}", record.phase),
            QueuePriority::Other,
        ),
    }
}

/// Builds the plain-text and HTML digests with as many entries as fit.
/// Each body holds at most `limit` characters and never cuts an entry in half.
#[cfg_attr(not(test), expect(dead_code, reason = "called by S34"))]
pub(crate) fn render_queue(
    entries: &[QueueEntry],
    truncated: bool,
    limit: i64,
    title_limit: i64,
) -> RenderedQueue {
    let mut shown = 0_usize;
    let mut plain = String::new();
    let mut formatted = String::new();
    for count in 1..=entries.len() {
        let (candidate_plain, candidate_formatted) =
            build_digest(entries, count, truncated, title_limit);
        if exceeds(&candidate_plain, limit) || exceeds(&candidate_formatted, limit) {
            break;
        }
        shown = count;
        plain = candidate_plain;
        formatted = candidate_formatted;
    }
    let dropped = truncated || shown < entries.len();
    RenderedQueue {
        plain,
        formatted,
        shown,
        dropped,
    }
}

/// Renders the first `count` entries as the plain-text and the HTML body.
fn build_digest(
    entries: &[QueueEntry],
    count: usize,
    truncated: bool,
    title_limit: i64,
) -> (String, String) {
    let waiting = entries[..count]
        .iter()
        .filter(|entry| entry.priority == QueuePriority::WaitingAuthor)
        .count();
    let mut plain = String::new();
    let mut formatted = String::new();
    if count > waiting {
        push_section(
            &mut plain,
            &mut formatted,
            &format!("Reviews waiting for you ({})", count - waiting),
        );
    }
    for (index, entry) in entries[..count].iter().enumerate() {
        if entry.priority == QueuePriority::WaitingAuthor
            && (index == 0 || entries[index - 1].priority != QueuePriority::WaitingAuthor)
        {
            push_section(
                &mut plain,
                &mut formatted,
                &format!("Waiting on author ({waiting})"),
            );
        }
        let title = clean_text(&entry.request.title, title_limit);
        let mut author = clean_text(&entry.request.author, title_limit);
        if author.is_empty() {
            author = "unknown".to_owned();
        }
        let reference = format!("{}#{}", entry.request.repository, entry.request.number);
        plain.push_str(&format!(
            "- {title} ({reference}, by {author}): {}\n  {}\n",
            entry.status, entry.request.url
        ));
        formatted.push_str(&format!(
            "<li><a href=\"{}\">{}</a> ({}, by {}): {}</li>\n",
            escape_html(&entry.request.url),
            escape_html(&title),
            escape_html(&reference),
            escape_html(&author),
            escape_html(&entry.status)
        ));
    }
    formatted.push_str("</ul>");
    if truncated || count < entries.len() {
        plain.push_str(TRUNCATED_TEXT);
        plain.push('\n');
        formatted.push_str("\n<p><em>");
        formatted.push_str(&escape_html(TRUNCATED_TEXT));
        formatted.push_str("</em></p>");
    }
    let plain = plain.trim_end_matches('\n').to_owned();
    (plain, formatted)
}

/// Starts a section of both bodies. The HTML list of the previous section is closed first.
fn push_section(plain: &mut String, formatted: &mut String, header: &str) {
    if !plain.is_empty() {
        formatted.push_str("</ul>\n");
    }
    plain.push_str(header);
    plain.push('\n');
    formatted.push_str("<p><strong>");
    formatted.push_str(&escape_html(header));
    formatted.push_str("</strong></p>\n<ul>\n");
}

/// Reports whether a body has more characters than `limit`.
fn exceeds(text: &str, limit: i64) -> bool {
    let count = text.chars().count();
    if count > i64::MAX as usize {
        return true;
    }
    (count as i64) > limit
}

/// Escapes the five characters that HTML gives meaning, with the escapes Go's `html.EscapeString`
/// writes.
fn escape_html(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '\'' => out.push_str("&#39;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&#34;"),
            _ => out.push(character),
        }
    }
    out
}

/// Turns GitHub-provided text into one line of at most `max` characters.
pub(crate) fn clean_text(text: &str, max: i64) -> String {
    let parts: Vec<&str> = text
        .split(|character: char| character.is_whitespace() || character.is_control())
        .filter(|part| !part.is_empty())
        .collect();
    let line = parts.join(" ");
    let max_usize = usize::try_from(max).unwrap_or(0);
    if line.chars().count() > max_usize {
        let kept: String = line.chars().take(max_usize.saturating_sub(1)).collect();
        format!("{kept}…")
    } else {
        line
    }
}

#[cfg(test)]
mod tests {
    use time::OffsetDateTime;

    use super::{QueuePriority, QueueState, clean_text, queue_entries, queue_status, render_queue};
    use crate::integrations::ReviewRequest;
    use crate::model::{AwaitRecord, Decision, PublishedRecord};
    use autoreview_policy::Policy;
    use autoreview_schedule::scheduler::Record;

    /// The policy of the queue tests: one repository glob with a review-request trigger, one
    /// repository with a label trigger, and no rule for `other/unlisted`.
    const TEST_POLICY: &str = "version: 1
reviewers:
  opus:
    protocol: claude-structured
    agent_profile: claude-profile
    label: Opus
    finding_prefix: O
pipelines:
  main:
    reviewers: [opus]
    incremental_reviewers: [opus]
    prompt: prompts/main.md
    checkout: base
    project_sources: [manual]
repositories:
  - match: example-org/*
    trigger: review-requested
    pipeline: main
    on_findings: request_changes
    on_clean: await
  - match: example-owner/tool
    trigger: label:ai:review
    pipeline: main
    on_findings: comment
    on_clean: comment
unmatched: skip
approval:
  timeout_hours: 24
notifications:
  approval_requested: action
  findings_on_own_pr: action
  review_stopped: action
  cleanup_manual: action
  command_failed: action
  approved: digest
  changes_requested: digest
  commented: digest
  approval_expired: digest
  started: log
  retry_scheduled: log
  superseded: log
  handoff_reminder: action
";

    fn test_policy() -> Policy {
        autoreview_policy::parse(TEST_POLICY.as_bytes(), |_| {
            Ok(b"Review the change.".to_vec())
        })
        .expect("the test policy parses")
    }

    fn record(phase: &str, error_code: &str) -> Record {
        Record {
            blocking_workflows: Vec::new(),
            identity: String::new(),
            phase: phase.to_owned(),
            attempt: 0,
            retries: 0,
            workflow_id: String::new(),
            due_at: OffsetDateTime::UNIX_EPOCH,
            error_code: error_code.to_owned(),
        }
    }

    fn queue_request(repository: &str, number: i64, title: &str) -> ReviewRequest {
        ReviewRequest {
            repository: repository.to_owned(),
            number,
            title: title.to_owned(),
            author: "someone".to_owned(),
            url: format!("https://github.com/{repository}/pull/{number}"),
        }
    }

    #[test]
    fn queue_status_annotates_each_engine_state() {
        let policy = test_policy();
        let connection = "example-org/example-repo";
        let labelled = "example-owner/tool";
        let unlisted = "other/unlisted";
        type Setup = fn(&mut QueueState, &str);
        let cases: Vec<(&str, &str, Setup, &str, QueuePriority)> = vec![
            (
                "awaiting approval",
                connection,
                |state, key| {
                    state.awaiting.insert(
                        key.to_owned(),
                        AwaitRecord {
                            repository: "example-org/example-repo".to_owned(),
                            number: 7,
                            ..AwaitRecord::default()
                        },
                    );
                },
                "AI review clean, waiting for your approve",
                QueuePriority::AwaitingApproval,
            ),
            (
                "running",
                connection,
                |state, key| {
                    state.schedule.insert(key.to_owned(), record("running", ""));
                },
                "AI review running",
                QueuePriority::Other,
            ),
            (
                "waiting",
                connection,
                |state, key| {
                    state.schedule.insert(key.to_owned(), record("waiting", ""));
                },
                "AI review queued",
                QueuePriority::Other,
            ),
            (
                "retry wait",
                connection,
                |state, key| {
                    state
                        .schedule
                        .insert(key.to_owned(), record("retry_wait", ""));
                },
                "AI review queued",
                QueuePriority::Other,
            ),
            (
                "stopped with code",
                connection,
                |state, key| {
                    state
                        .schedule
                        .insert(key.to_owned(), record("failed", "pohunek_session_failed"));
                },
                "AI review stopped: pohunek_session_failed",
                QueuePriority::Stopped,
            ),
            (
                "stopped without code",
                connection,
                |state, key| {
                    state.schedule.insert(key.to_owned(), record("failed", ""));
                },
                "AI review stopped: unknown",
                QueuePriority::Stopped,
            ),
            (
                "withdrawn",
                connection,
                |state, key| {
                    state
                        .schedule
                        .insert(key.to_owned(), record("withdrawn", ""));
                },
                "AI review withdrawn",
                QueuePriority::Other,
            ),
            (
                "unknown phase",
                connection,
                |state, key| {
                    state.schedule.insert(key.to_owned(), record("paused", ""));
                },
                "AI review state unknown: paused",
                QueuePriority::Other,
            ),
            (
                "published while still requested",
                connection,
                |state, key| {
                    state
                        .schedule
                        .insert(key.to_owned(), record("completed", ""));
                    state.published.insert(
                        key.to_owned(),
                        PublishedRecord {
                            decision: Decision::Comment,
                            ..PublishedRecord::default()
                        },
                    );
                },
                "AI review published COMMENT",
                QueuePriority::Other,
            ),
            (
                "completed without published record",
                connection,
                |state, key| {
                    state
                        .schedule
                        .insert(key.to_owned(), record("completed", ""));
                },
                "AI review completed",
                QueuePriority::Other,
            ),
            (
                "covered and not yet started",
                connection,
                |_, _| {},
                "AI review not started yet",
                QueuePriority::Other,
            ),
            (
                "label trigger without engine state",
                labelled,
                |_, _| {},
                "not covered by AI review",
                QueuePriority::NotCovered,
            ),
            (
                "no policy rule",
                unlisted,
                |_, _| {},
                "not covered by AI review",
                QueuePriority::NotCovered,
            ),
        ];
        for (name, repository, setup, status, priority) in cases {
            let mut state = QueueState::default();
            let request = queue_request(repository, 7, "title");
            let key = format!("{repository}#7");
            setup(&mut state, &key);
            let (got_status, got_priority) = queue_status(&policy, &state, &request, &key);
            assert!(
                got_status == status && got_priority == priority,
                "{name}: status {got_status:?} priority {got_priority:?}, \
                 want {status:?} priority {priority:?}"
            );
        }
    }

    #[test]
    fn queue_entries_put_action_items_first_and_sort_by_key() {
        let policy = test_policy();
        let mut state = QueueState::default();
        state.schedule.insert(
            "example-org/example-repo#1".to_owned(),
            record("running", ""),
        );
        state.awaiting.insert(
            "example-org/zeta#9".to_owned(),
            AwaitRecord {
                repository: "example-org/zeta".to_owned(),
                number: 9,
                ..AwaitRecord::default()
            },
        );
        state
            .schedule
            .insert("example-org/beta#2".to_owned(), record("failed", "x"));
        let requests = vec![
            queue_request("example-org/example-repo", 1, "running"),
            queue_request("other/unlisted", 3, "not covered"),
            queue_request("example-org/beta", 2, "stopped"),
            queue_request("example-org/zeta", 9, "awaiting"),
            queue_request("example-org/alpha", 4, "not started"),
        ];
        let entries = queue_entries(&policy, &state, &requests);
        let order: Vec<&str> = entries
            .iter()
            .map(|entry| entry.request.title.as_str())
            .collect();
        let want = [
            "awaiting",
            "not covered",
            "stopped",
            "not started",
            "running",
        ];
        assert!(order == want, "order {order:?}, want {want:?}");
    }

    #[test]
    fn render_queue_escapes_text_and_links_only_the_verified_url() {
        let mut request = queue_request(
            "example-org/example-repo",
            12,
            "<img src=x onerror=\"alert(1)\"> & \"quoted\"",
        );
        request.author = "dev<script>".to_owned();
        let entries = vec![super::QueueEntry {
            request,
            status: "AI review running".to_owned(),
            priority: QueuePriority::Other,
        }];
        let rendered = render_queue(&entries, false, 8000, 512);
        assert!(
            rendered.shown == 1 && !rendered.dropped,
            "shown {} dropped {}",
            rendered.shown,
            rendered.dropped
        );
        assert!(
            !rendered.formatted.contains("<img") && !rendered.formatted.contains("<script"),
            "formatted body carries raw markup: {}",
            rendered.formatted
        );
        assert!(
            rendered
                .formatted
                .contains("<a href=\"https://github.com/example-org/example-repo/pull/12\">"),
            "missing verified link: {}",
            rendered.formatted
        );
        assert!(
            rendered
                .formatted
                .contains("&lt;img src=x onerror=&#34;alert(1)&#34;&gt; &amp; &#34;quoted&#34;"),
            "title not escaped: {}",
            rendered.formatted
        );
        assert!(
            rendered.plain.contains("Reviews waiting for you (1)")
                && rendered
                    .plain
                    .contains("https://github.com/example-org/example-repo/pull/12"),
            "plain body lacks header or URL: {}",
            rendered.plain
        );
    }

    #[test]
    fn render_queue_drops_whole_entries_that_do_not_fit() {
        let entries: Vec<super::QueueEntry> = (1..=5)
            .map(|number| super::QueueEntry {
                request: queue_request("example-org/example-repo", number, &"t".repeat(40)),
                status: "AI review running".to_owned(),
                priority: QueuePriority::AwaitingApproval,
            })
            .collect();
        let rendered = render_queue(&entries, false, 600, 512);
        assert!(
            rendered.shown > 0 && rendered.shown < entries.len() && rendered.dropped,
            "shown {} dropped {}",
            rendered.shown,
            rendered.dropped
        );
        assert!(
            rendered.plain.contains("truncated") && rendered.formatted.contains("truncated"),
            "truncation note missing"
        );
        assert!(
            rendered.formatted.matches("<li>").count() == rendered.shown
                && rendered.formatted.ends_with("</p>"),
            "formatted body is not a complete list: {}",
            rendered.formatted
        );
        assert!(
            rendered.plain.chars().count() <= 600 && rendered.formatted.chars().count() <= 600,
            "bodies exceed the limit: {} {}",
            rendered.plain.chars().count(),
            rendered.formatted.chars().count()
        );
    }

    #[test]
    fn clean_text_collapses_control_characters_and_limits_length() {
        assert_eq!(clean_text("a\n\tb\u{0}  c", 10), "a b c");
        assert_eq!(clean_text("abcdef", 4), "abc…");
    }
}
