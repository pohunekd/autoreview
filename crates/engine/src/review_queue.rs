//! The review queue: the waiting requests and notes, posted as one digest.
//!
//! Mirrors `cmd/autoreview/review_queue.go`.

// Rust guideline compliant 2026-06-26

use std::collections::{BTreeMap, BTreeSet};

use autoreview_policy::{Policy, TriggerKind};
use autoreview_schedule::scheduler::Record;
use serde::Serialize;
use tokio_util::sync::CancellationToken;

use crate::discovery::poll::live;
use crate::engine::{Clock, Engine, retry_step};
use crate::error::{Error, error_code};
use crate::integrations::{Integrations, ReviewRequest};
use crate::messages::handoff_age;
use crate::model::{AwaitRecord, PublishedRecord};
use crate::publish::decision_text;

/// Note that closes a digest whose list does not show every request.
const TRUNCATED_TEXT: &str = "The list is truncated: more requests wait for you than are shown.";

/// Prefix of the transaction ID of one queue run (`review_queue.go:135`).
const TRANSACTION_PREFIX: &str = "queue-";

/// Random bytes of the transaction ID of one queue run (`review_queue.go:131`).
const TRANSACTION_RANDOM_BYTES: usize = 16;

/// Error code of a digest that has no room for a single entry.
const REVIEW_QUEUE_TOO_LONG: &str = "review_queue_too_long";

/// Error code of a transaction ID that cannot be drawn.
const MATRIX_REQUEST_INVALID: &str = "matrix_request_invalid";

/// Error code of a queue limit that the configuration loader refuses.
const CONFIGURATION_INVALID: &str = "configuration_invalid";

/// Configuration key of the queue limit.
const QUEUE_MAX_ITEMS_KEY: &str = "notify.review_queue.max_items";

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

impl<I: Integrations, C: Clock> Engine<I, C> {
    /// Posts the review requests and the notes that wait, as one queue digest.
    ///
    /// Nothing is posted when nothing waits. Every retry of the post reuses one transaction ID,
    /// so the homeserver keeps one copy, while a later run posts its own digest.
    ///
    /// # Errors
    ///
    /// Returns the error of the search, the store or the post, `review_queue_too_long` when no
    /// entry fits, `matrix_request_invalid` without a transaction ID, and `configuration_invalid`
    /// without a policy or with a negative limit.
    pub async fn review_queue(&self, cancel: &CancellationToken) -> Result<QueueResult, Error> {
        let limit = self.config.queue_max_items;
        let list = retry_step(self.step_policy(), &self.clock, cancel, || {
            self.integration.list_review_requests(cancel, limit)
        })
        .await?;
        live(cancel)?;
        let discovery = self.store.load_discovery()?;
        let state = QueueState {
            schedule: discovery.schedule,
            published: discovery.published,
            awaiting: discovery.awaiting,
        };
        let mut entries = queue_entries(self.policy()?, &state, &list.requests);
        entries.extend(self.waiting_on_author(cancel, &state, &list.requests).await);
        if entries.is_empty() {
            return Ok(QueueResult::default());
        }
        // The entries are cut after the sort. So a request that needs the user's action is never
        // dropped for one that only waits for the engine.
        let over = i64::try_from(entries.len()).map_or(true, |len| len > limit);
        let truncated = list.truncated || over;
        if over {
            let Ok(kept) = usize::try_from(limit) else {
                return Err(Error::config(
                    CONFIGURATION_INVALID,
                    QUEUE_MAX_ITEMS_KEY,
                    None,
                ));
            };
            entries.truncate(kept);
        }
        let rendered = render_queue(
            &entries,
            truncated,
            self.config.matrix_max_characters,
            self.config.max_title_characters,
        );
        if rendered.shown == 0 {
            return Err(Error::workflow(REVIEW_QUEUE_TOO_LONG, true, None));
        }
        let transaction = queue_transaction_id()?;
        retry_step(self.step_policy(), &self.clock, cancel, || {
            self.integration.send_queue_digest(
                cancel,
                &transaction,
                &rendered.plain,
                &rendered.formatted,
            )
        })
        .await?;
        Ok(QueueResult {
            count: i64::try_from(rendered.shown).unwrap_or(i64::MAX),
            sent: true,
            truncated: rendered.dropped,
        })
    }

    /// Returns the open pull requests whose notes wait for their author.
    ///
    /// The author has not requested the review again. A pull request that cannot be read is
    /// left out of this digest with a warning, also after a cancel, as Go leaves it out.
    async fn waiting_on_author(
        &self,
        cancel: &CancellationToken,
        state: &QueueState,
        requests: &[ReviewRequest],
    ) -> Vec<QueueEntry> {
        let requested: BTreeSet<String> = requests
            .iter()
            .map(|request| format!("{}#{}", request.repository, request.number))
            .collect();
        let now = self.clock.wall_now();
        let mut entries = Vec::new();
        for (key, record) in &state.published {
            if !record.awaits_author || requested.contains(key) {
                continue;
            }
            let read = retry_step(self.step_policy(), &self.clock, cancel, || {
                self.integration
                    .read_pull_request_state(cancel, &record.repository, record.number)
            })
            .await;
            let current = match read {
                Ok(current) => current,
                Err(err) => {
                    tracing::warn!(
                        repository = %record.repository,
                        pull_request = record.number,
                        error_code = error_code(&err),
                        "notes hand-off unreadable"
                    );
                    continue;
                }
            };
            if !current.open || current.requested {
                continue;
            }
            let mut status = format!(
                "notes handed to the author {} ago",
                handoff_age(record, now)
            );
            if record.reminded {
                status.push_str(", reminded");
            }
            entries.push(QueueEntry {
                request: ReviewRequest {
                    repository: record.repository.clone(),
                    number: record.number,
                    title: current.title,
                    author: current.author,
                    url: current.url,
                },
                status,
                priority: QueuePriority::WaitingAuthor,
            });
        }
        entries
    }
}

/// Returns a fresh transaction ID: `queue-` and 16 random bytes in lowercase hex.
///
/// # Errors
///
/// Returns `matrix_request_invalid`, permanent, when the system has no random bytes.
fn queue_transaction_id() -> Result<String, Error> {
    let mut random = [0_u8; TRANSACTION_RANDOM_BYTES];
    getrandom::fill(&mut random)
        .map_err(|err| Error::workflow(MATRIX_REQUEST_INVALID, true, Some(Box::new(err))))?;
    Ok(format!("{TRANSACTION_PREFIX}{}", hex::encode(random)))
}

/// Annotates each request with its engine state and sorts the entries.
/// The order is priority, then repository and number.
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
        let previous_waiting = index
            .checked_sub(1)
            .and_then(|previous| entries.get(previous))
            .is_some_and(|previous| previous.priority == QueuePriority::WaitingAuthor);
        if entry.priority == QueuePriority::WaitingAuthor && !previous_waiting {
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
    use std::future::{Future, ready};
    use std::path::Path;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use autoreview_policy::Level;
    use tempfile::TempDir;
    use time::OffsetDateTime;
    use time::macros::datetime;
    use tokio_util::sync::CancellationToken;

    use super::{
        QueuePriority, QueueResult, QueueState, clean_text, queue_entries, queue_status,
        render_queue,
    };
    use crate::config::Config;
    use crate::engine::{Clock, Engine};
    use crate::error::error_code;
    use crate::integrations::ReviewRequest;
    use crate::model::{AwaitRecord, Decision, PublishedRecord, PullRequest};
    use crate::protocol::fixtures::sha;
    use crate::protocol::validate::tests::test_config;
    use crate::store::Store;
    use crate::testing::fake::FakeIntegration;
    use crate::testing::harness::{integration_config, integration_pull_request};
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

    /// A clock frozen at one wall time; its waits end at once, as in Go's `newQueueEngine`.
    struct FrozenClock {
        /// The instant that every call of `now` returns.
        instant: Instant,
        /// The wall time that every call of `wall_now` returns.
        at: OffsetDateTime,
    }

    impl Clock for FrozenClock {
        fn now(&self) -> Instant {
            self.instant
        }

        fn sleep(&self, _delay: Duration) -> impl Future<Output = ()> + Send {
            ready(())
        }

        fn wall_now(&self) -> OffsetDateTime {
            self.at
        }
    }

    /// The engine of the queue tests.
    type QueueEngine = Engine<FakeIntegration, FrozenClock>;

    /// The deployed test configuration: `testdata/review.env` and `testdata/review-policy.yaml`.
    fn queue_config() -> Config {
        let document = include_str!("../../../testdata/review-policy.yaml");
        let policy = autoreview_policy::parse(document.as_bytes(), |relative| {
            std::fs::read(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../testdata")
                    .join(relative),
            )
        })
        .expect("the deployed policy parses");
        let mut cfg = test_config();
        cfg.policy = Some(policy);
        cfg.github_login = "example-user".to_owned();
        cfg.queue_matrix_room = "!queue:example.org".to_owned();
        cfg.queue_max_items = 50;
        cfg.published_retention = Duration::from_secs(1_209_600);
        cfg.step_retry = Duration::from_secs(1800);
        cfg.step_retry_initial = Duration::from_secs(30);
        cfg.step_retry_max_interval = Duration::from_secs(600);
        cfg.state_entry_limit = 1000;
        cfg.max_title_characters = 200;
        cfg.matrix_max_characters = 12_000;
        cfg.excerpt_characters = 800;
        cfg
    }

    /// Builds an engine on a fresh store, the fake and a clock frozen at `at`.
    ///
    /// The returned directory holds the store and must outlive the engine.
    fn new_queue_engine(
        cfg: Config,
        fake: &Arc<FakeIntegration>,
        at: OffsetDateTime,
    ) -> (QueueEngine, TempDir) {
        let dir = TempDir::new().expect("create the store directory");
        let store = Store::open(
            &dir.path().join("state.db"),
            Box::new(OffsetDateTime::now_utc),
        )
        .expect("open the store");
        let clock = FrozenClock {
            instant: Instant::now(),
            at,
        };
        let engine = Engine::new(cfg, Arc::new(store), Arc::clone(fake), clock);
        (engine, dir)
    }

    /// Reports whether `transaction` is `queue-` followed by 32 lowercase hex digits.
    fn queue_transaction_valid(transaction: &str) -> bool {
        transaction.strip_prefix("queue-").is_some_and(|hex| {
            hex.len() == 32
                && hex
                    .chars()
                    .all(|character| matches!(character, '0'..='9' | 'a'..='f'))
        })
    }

    #[tokio::test]
    async fn review_queue_sends_nothing_when_no_request_waits() {
        let fake = Arc::new(FakeIntegration::new());
        let (engine, _dir) =
            new_queue_engine(queue_config(), &fake, datetime!(2026-10-19 14:00 UTC));
        let result = engine
            .review_queue(&CancellationToken::new())
            .await
            .expect("the queue runs");
        let queued = fake.lock().queued.len();
        assert!(
            result == QueueResult::default() && queued == 0,
            "result {result:?} queued {queued}"
        );
    }

    #[tokio::test]
    async fn review_queue_lists_the_requests_and_truncates_at_the_limit() {
        let mut cfg = queue_config();
        cfg.queue_max_items = 2;
        let fake = Arc::new(FakeIntegration::new());
        fake.lock().requests = vec![
            queue_request("example-org/example-repo", 1, "first"),
            queue_request("example-org/example-repo", 2, "second"),
            queue_request("example-org/example-repo", 3, "third"),
        ];
        let (engine, _dir) = new_queue_engine(cfg, &fake, datetime!(2026-10-19 14:00 UTC));
        let result = engine
            .review_queue(&CancellationToken::new())
            .await
            .expect("the queue runs");
        assert_eq!(
            result,
            QueueResult {
                count: 2,
                sent: true,
                truncated: true
            }
        );
        let queued = fake.lock().queued.clone();
        assert!(
            queued.len() == 1
                && queued[0].plain.contains("Reviews waiting for you (2)")
                && queued[0].plain.contains("truncated"),
            "queued {queued:?}"
        );
    }

    #[tokio::test]
    async fn review_queue_retries_with_one_transaction_per_run() {
        let fake = Arc::new(FakeIntegration::new());
        fake.lock().requests = vec![queue_request("example-org/example-repo", 1, "first")];
        fake.lock().queue_failures = 2;
        let (engine, _dir) =
            new_queue_engine(queue_config(), &fake, datetime!(2026-10-19 14:35 UTC));
        let result = engine
            .review_queue(&CancellationToken::new())
            .await
            .expect("the queue runs");
        let sends = fake.lock().queue_sends;
        assert!(result.sent && sends == 3, "result {result:?} sends {sends}");
        let transactions = fake.lock().queue_transactions.clone();
        let first = transactions[0].clone();
        assert!(queue_transaction_valid(&first), "transaction {first:?}");
        assert!(
            transactions.iter().all(|transaction| *transaction == first),
            "retries used transactions {transactions:?}"
        );
        // A second run in the same hour posts its own message.
        engine
            .review_queue(&CancellationToken::new())
            .await
            .expect("the second run posts");
        let queued = fake.lock().queued.clone();
        assert!(
            queued.len() == 2 && queued[1].transaction != first,
            "queued {queued:?}"
        );
    }

    #[tokio::test]
    async fn review_queue_gives_up_when_the_retry_window_ends() {
        let mut cfg = queue_config();
        cfg.step_retry = Duration::ZERO;
        let fake = Arc::new(FakeIntegration::new());
        fake.lock().requests = vec![queue_request("example-org/example-repo", 1, "first")];
        fake.lock().queue_failures = 1000;
        let (engine, _dir) = new_queue_engine(cfg, &fake, datetime!(2026-10-19 14:00 UTC));
        let result = engine.review_queue(&CancellationToken::new()).await;
        assert!(
            result
                .as_ref()
                .is_err_and(|err| error_code(err) == "matrix_unavailable"),
            "error {result:?}"
        );
        let (sends, queued) = {
            let state = fake.lock();
            (state.queue_sends, state.queued.len())
        };
        assert!(sends == 1 && queued == 0, "sends {sends} queued {queued}");
    }

    /// Records `pr` as notes handed to its author at `published_at`.
    ///
    /// The fake keeps `pr` open, and it no longer requests the review.
    async fn hand_off(
        engine: &QueueEngine,
        fake: &FakeIntegration,
        pr: &PullRequest,
        published_at: OffsetDateTime,
        reminded: bool,
    ) {
        fake.lock().current.insert(pr.key(), pr.clone());
        let record = PublishedRecord {
            repository: pr.repository.clone(),
            number: pr.number,
            head_sha: pr.head_sha.clone(),
            review_request_id: pr.review_request_id,
            decision: Decision::Comment,
            workflow_id: pr.workflow_id(0),
            published_at,
            awaits_author: true,
            reminded,
        };
        engine
            .mark_published(&CancellationToken::new(), &record)
            .await
            .expect("the hand-off is recorded");
    }

    #[tokio::test]
    async fn unanswered_notes_handoff_reminds_once_and_ends_when_closed() {
        let at = datetime!(2026-10-23 12:00 UTC);
        let fake = Arc::new(FakeIntegration::new());
        let config_dir = TempDir::new().expect("create the configuration directory");
        let mut cfg = integration_config(config_dir.path());
        cfg.published_retention = Duration::from_secs(90 * 24 * 60 * 60);
        let (engine, _dir) = new_queue_engine(cfg, &fake, at);
        let mut fresh = integration_pull_request("acme/notes", &sha("a"));
        let mut stale = integration_pull_request("acme/notes", &sha("a"));
        let mut closed = integration_pull_request("acme/notes", &sha("a"));
        (fresh.number, stale.number, closed.number) = (101, 102, 103);
        hand_off(
            &engine,
            &fake,
            &fresh,
            at - time::Duration::hours(23),
            false,
        )
        .await;
        hand_off(
            &engine,
            &fake,
            &stale,
            at - time::Duration::hours(25),
            false,
        )
        .await;
        hand_off(
            &engine,
            &fake,
            &closed,
            at - time::Duration::hours(30),
            false,
        )
        .await;
        fake.close(closed.clone());
        for _ in 0..2 {
            engine
                .poll(&CancellationToken::new())
                .await
                .expect("the discovery poll succeeds");
        }
        let (_, notes, _, _) = fake.snapshot();
        let reminders: Vec<_> = notes
            .iter()
            .filter(|entry| entry.message.contains("wait for the author"))
            .collect();
        assert!(
            reminders.len() == 1
                && reminders[0].level == Level::Action
                && reminders[0]
                    .message
                    .contains("acme/notes PR #102 wait for the author for 25 h")
                && !reminders[0].message.contains("Review ID"),
            "reminders = {reminders:?}"
        );
        let state = engine.store.load_discovery().expect("the state loads");
        let got = state
            .published
            .get(&stale.key())
            .cloned()
            .unwrap_or_default();
        assert!(got.awaits_author && got.reminded, "stale = {got:?}");
        let got = state
            .published
            .get(&fresh.key())
            .cloned()
            .unwrap_or_default();
        assert!(got.awaits_author && !got.reminded, "fresh = {got:?}");
        let got = state
            .published
            .get(&closed.key())
            .cloned()
            .unwrap_or_default();
        assert!(
            !got.awaits_author,
            "a closed pull request must end its hand-off: {got:?}"
        );
    }

    #[tokio::test]
    async fn review_queue_lists_notes_waiting_on_their_author_apart() {
        let at = datetime!(2026-10-23 12:00 UTC);
        let fake = Arc::new(FakeIntegration::new());
        let (engine, _dir) = new_queue_engine(queue_config(), &fake, at);
        let waiting = PullRequest {
            repository: "example-org/example-repo".to_owned(),
            number: 5,
            head_sha: sha("a"),
            review_request_id: 3,
            title: "Optional polish".to_owned(),
            url: "https://github.com/example-org/example-repo/pull/5".to_owned(),
            author: "colleague".to_owned(),
            ..PullRequest::default()
        };
        hand_off(
            &engine,
            &fake,
            &waiting,
            at - time::Duration::hours(30),
            true,
        )
        .await;
        let result = engine
            .review_queue(&CancellationToken::new())
            .await
            .expect("the queue runs");
        let queued = fake.lock().queued.clone();
        assert!(
            result.sent
                && result.count == 1
                && !queued[0].plain.contains("Reviews waiting for you")
                && queued[0].plain.contains(concat!(
                    "Waiting on author (1)\n- Optional polish (example-org/example-repo#5, ",
                    "by colleague): notes handed to the author 30 h ago, reminded"
                )),
            "result {result:?} queued {queued:?}"
        );
        fake.lock().requests = vec![queue_request("example-org/example-repo", 1, "first")];
        engine
            .review_queue(&CancellationToken::new())
            .await
            .expect("the second run posts");
        let queued = fake.lock().queued.clone();
        let (plain, formatted) = (&queued[1].plain, &queued[1].formatted);
        assert!(
            plain.contains("Reviews waiting for you (1)")
                && plain.find("Waiting on author (1)") >= plain.find("first")
                && formatted.matches("<ul>").count() == 2
                && formatted.matches("</ul>").count() == 2,
            "plain={plain} formatted={formatted}"
        );
    }

    #[test]
    fn review_queue_configuration_is_required() {
        let mut cfg = queue_config();
        cfg.queue_matrix_room = String::new();
        let error = cfg.validate().expect_err("a missing queue room is refused");
        assert_eq!(
            error.to_string(),
            "configuration_invalid: config: invalid Matrix queue room"
        );
        let mut cfg = queue_config();
        cfg.queue_max_items = 1000;
        let error = cfg
            .validate()
            .expect_err("queue limit at the gh search cap was accepted");
        assert_eq!(
            error.to_string(),
            "configuration_invalid: config: configured limit is outside the safe range"
        );
        let mut cfg = queue_config();
        cfg.queue_matrix_room = "not-a-room".to_owned();
        let error = cfg.validate().expect_err("invalid queue room was accepted");
        assert_eq!(
            error.to_string(),
            "configuration_invalid: config: invalid Matrix queue room"
        );
    }
}
