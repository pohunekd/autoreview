//! The review queue: the waiting requests and notes, posted as one digest.

// Rust guideline compliant 2026-06-26

use std::collections::{BTreeMap, BTreeSet};

use autoreview_policy::{Policy, TriggerKind};
use autoreview_schedule::phase;
use autoreview_schedule::scheduler::Record;
use serde::Serialize;
use tokio_util::sync::CancellationToken;

use crate::config::Credential;
use crate::discovery::SHUTDOWN;
use crate::discovery::poll::{STEP_READ_PULL_REQUEST_STATE, live};
use crate::engine::{Clock, Engine, retry_step};
use crate::error::{Error, error_code};
use crate::integrations::{Integrations, ReviewRequest, ReviewRequestList};
use crate::messages::{excerpt, handoff_age};
use crate::model::{AwaitRecord, PublishedRecord};
use crate::publish::decision_text;

/// Note that closes a digest whose list does not show every request.
const TRUNCATED_TEXT: &str = "The list is truncated: more requests wait for you than are shown.";

/// Prefix of the transaction ID of one queue run.
const TRANSACTION_PREFIX: &str = "queue-";

/// Random bytes of the transaction ID of one queue run.
const TRANSACTION_RANDOM_BYTES: usize = 16;

/// Error code of a digest that has no room for a single entry.
const REVIEW_QUEUE_TOO_LONG: &str = "review_queue_too_long";

/// Error code of a transaction ID that cannot be drawn.
const MATRIX_REQUEST_INVALID: &str = "matrix_request_invalid";

/// Error code of a queue limit that the configuration loader refuses.
const CONFIGURATION_INVALID: &str = "configuration_invalid";

/// Configuration key of the queue limit.
const QUEUE_MAX_ITEMS_KEY: &str = "notify.review_queue.max_items";

/// Step name of the review request search in the retry log.
const STEP_LIST_REVIEW_REQUESTS: &str = "list_review_requests";

/// Code of a hand-off whose stored credential no longer fits its review.
const CREDENTIAL_NOT_COVERED: &str = "credential_not_covered";

/// Position of a queue entry. The derived order puts the requests that need the owner's action
/// first, and the notes that wait for their author after the requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum QueuePriority {
    /// The engine waits for the owner's approval.
    AwaitingApproval,
    /// The policy does not cover the repository with a review-request trigger.
    NotCovered,
    /// The rule skips the untrusted author, so only a human reviews the pull request.
    Skipped,
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
    /// The account of the serving credential, `login (credential name)`, with several.
    pub(crate) account: Option<String>,
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
    /// Reasons of the pull requests that the last discovery pass skipped.
    pub(crate) skipped: BTreeMap<String, String>,
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
    /// Nothing is posted when nothing waits. The post gets one attempt, bounded by the
    /// notifier's request timeout; a failure is logged and returned. Each run has its own
    /// transaction ID.
    ///
    /// # Errors
    ///
    /// Returns the error of the search, the store or the post, `review_queue_too_long` when no
    /// entry fits, `matrix_request_invalid` without a transaction ID, and `configuration_invalid`
    /// without a policy or with a negative limit.
    pub async fn review_queue(&self, cancel: &CancellationToken) -> Result<QueueResult, Error> {
        let (entries, truncated) = self.queue_snapshot(cancel).await?;
        if entries.is_empty() {
            return Ok(QueueResult::default());
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
        self.send_queue(cancel, &transaction, &rendered).await?;
        Ok(QueueResult {
            count: i64::try_from(rendered.shown).unwrap_or(i64::MAX),
            sent: true,
            truncated: rendered.dropped,
        })
    }

    /// Posts `rendered` to each backend of the review queue, in configured order.
    ///
    /// Every backend gets one attempt; each failure is logged with its backend, and the first
    /// failure in queue order is returned. After a shutdown the remaining backends get none.
    ///
    /// # Errors
    ///
    /// Returns the error of the first backend that failed.
    async fn send_queue(
        &self,
        cancel: &CancellationToken,
        transaction: &str,
        rendered: &RenderedQueue,
    ) -> Result<(), Error> {
        let (plain, formatted) = (&rendered.plain, &rendered.formatted);
        let mut first = None;
        for backend in &self.config.notify.queue {
            let sent = (self.integration).send_queue_digest(
                cancel,
                backend,
                transaction,
                plain,
                formatted,
            );
            let Err(err) = sent.await else {
                continue;
            };
            // The notifier fails at once on a cancelled token, so the shutdown is the cause.
            let shutdown = cancel.is_cancelled();
            let code = if shutdown { SHUTDOWN } else { error_code(&err) };
            tracing::warn!(
                count = rendered.shown,
                backend = backend.as_str(),
                error_code = code,
                "review queue digest dropped"
            );
            first.get_or_insert(err);
            if shutdown {
                break;
            }
        }
        first.map_or(Ok(()), Err)
    }

    /// Returns the queue entries in digest order, at most `queue_max_items` of them.
    ///
    /// The flag reports that requests were left out. Nothing is sent.
    ///
    /// # Errors
    ///
    /// Returns the search error when every credential fails, the error of the store, and
    /// `configuration_invalid` without a policy or with a negative limit.
    pub(crate) async fn queue_snapshot(
        &self,
        cancel: &CancellationToken,
    ) -> Result<(Vec<QueueEntry>, bool), Error> {
        let limit = self.config.queue_max_items;
        let (searches, failed) = self
            .search_each(STEP_LIST_REVIEW_REQUESTS, true, cancel, |credential| {
                self.integration
                    .list_review_requests(cancel, credential, limit)
            })
            .await?;
        // A failed credential's requests are missing, so the queue reports itself truncated.
        let mut list = ReviewRequestList {
            requests: Vec::new(),
            truncated: !failed.is_empty(),
        };
        for (credential, found) in searches {
            let owned = |request: &ReviewRequest| {
                (self.config.credentials.serving(&request.repository))
                    .is_ok_and(|serving| serving.name == credential.name)
            };
            list.requests
                .extend(found.requests.into_iter().filter(|request| owned(request)));
            list.truncated |= found.truncated;
        }
        live(cancel)?;
        let discovery = self.store.load_discovery()?;
        let state = QueueState {
            schedule: discovery.schedule,
            published: discovery.published,
            awaiting: discovery.awaiting,
            skipped: discovery.skipped,
        };
        let mut entries = queue_entries(self.policy()?, &state, &list.requests);
        let waiting = self.waiting_on_author(cancel, &state, &list.requests, &failed);
        entries.extend(waiting.await);
        if self.config.credentials.single().is_none() {
            // A hand-off already names the account of its review.
            for entry in entries.iter_mut().filter(|entry| entry.account.is_none()) {
                let serving = self.config.credentials.serving(&entry.request.repository);
                entry.account = serving.ok().map(account_label);
            }
        }
        if entries.is_empty() {
            return Ok((entries, list.truncated));
        }
        // The entries are cut after the sort. So a request that needs the user's action is never
        // dropped for one that only waits for the engine.
        let over = i64::try_from(entries.len()).map_or(true, |len| len > limit);
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
        Ok((entries, list.truncated || over))
    }

    /// Returns the open pull requests whose notes wait for their author.
    ///
    /// The author has not requested the review again. A pull request that cannot be read is
    /// left out of this digest with a warning, also after a cancel. One
    /// that no credential serves is left out unread. The read uses the credential of the
    /// hand-off's review; one whose credential no longer fits or failed its search is left out.
    async fn waiting_on_author(
        &self,
        cancel: &CancellationToken,
        state: &QueueState,
        requests: &[ReviewRequest],
        failed: &BTreeMap<String, String>,
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
            let reader = (self.config.credentials.serving(&record.repository))
                .and_then(|_| self.handoff_credential(record))
                .map_err(|err| error_code(&err).to_owned())
                .and_then(|found| found.ok_or_else(|| CREDENTIAL_NOT_COVERED.to_owned()))
                .and_then(|credential| match failed.get(&credential.name) {
                    Some(code) => Err(code.clone()),
                    None => Ok(credential),
                });
            let credential = match reader {
                Ok(credential) => credential,
                Err(code) => {
                    tracing::warn!(
                        repository = %record.repository,
                        pull_request = record.number,
                        error_code = %code,
                        "notes hand-off unreadable"
                    );
                    continue;
                }
            };
            let read = retry_step(
                STEP_READ_PULL_REQUEST_STATE,
                self.step_policy(),
                &self.clock,
                cancel,
                || {
                    self.integration.read_pull_request_state(
                        cancel,
                        credential,
                        &record.repository,
                        record.number,
                    )
                },
            )
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
                account: (self.config.credentials.single())
                    .is_none()
                    .then(|| account_label(credential)),
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
            account: None,
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
/// A skip comes from the latest discovery pass, so it outranks a published record, which is
/// history, and every schedule phase but `running`.
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
    let pending = state
        .schedule
        .get(key)
        .filter(|record| record.phase != phase::COMPLETED);
    if let Some(record) = pending
        && record.phase == phase::RUNNING
    {
        return schedule_status(record);
    }
    if let Some(reason) = state.skipped.get(key) {
        return (
            format!("AI review skipped: {reason}"),
            QueuePriority::Skipped,
        );
    }
    if let Some(record) = pending {
        return schedule_status(record);
    }
    if let Some(record) = state.published.get(key) {
        return (
            format!("AI review published {}", decision_text(&record.decision)),
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
        phase::RUNNING => ("AI review running".to_owned(), QueuePriority::Other),
        phase::WAITING | phase::RETRY_WAIT if record.awaits_host => (
            format!("AI review waits for a review host: {}", status_code(record)),
            QueuePriority::Other,
        ),
        phase::WAITING | phase::RETRY_WAIT => ("AI review queued".into(), QueuePriority::Other),
        phase::FAILED => (
            format!("AI review stopped: {}", status_code(record)),
            QueuePriority::Stopped,
        ),
        phase::WITHDRAWN => ("AI review withdrawn".to_owned(), QueuePriority::Other),
        _ => (
            format!("AI review state unknown: {}", record.phase),
            QueuePriority::Other,
        ),
    }
}

/// Returns the record's error code, or `unknown` when it is empty.
fn status_code(record: &Record) -> &str {
    if record.error_code.is_empty() {
        "unknown"
    } else {
        record.error_code.as_str()
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
///
/// The entries are grouped by account, in the order of the account labels, and keep their
/// order within an account. Each account has its requests first, then its notes.
fn build_digest(
    entries: &[QueueEntry],
    count: usize,
    truncated: bool,
    title_limit: i64,
) -> (String, String) {
    let mut shown: Vec<&QueueEntry> = entries[..count].iter().collect();
    shown.sort_by(|a, b| a.account.cmp(&b.account));
    let section = |entry: &QueueEntry| {
        let waiting = entry.priority == QueuePriority::WaitingAuthor;
        (entry.account.clone(), waiting)
    };
    let mut plain = String::new();
    let mut formatted = String::new();
    for (index, entry) in shown.iter().enumerate() {
        let current = section(entry);
        let previous = index
            .checked_sub(1)
            .and_then(|previous| shown.get(previous));
        if previous.is_none_or(|previous| section(previous) != current) {
            let size = shown
                .iter()
                .filter(|other| section(other) == current)
                .count();
            let header = section_header(current.0.as_deref(), current.1, size);
            push_section(&mut plain, &mut formatted, &header);
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

/// Returns the account label of `credential` in the digest: `login (credential name)`.
fn account_label(credential: &Credential) -> String {
    format!("{} (credential {})", credential.login, credential.name)
}

/// Returns the header of a section of `size` entries: requests, or notes when `waiting`.
///
/// With one credential the account is `None` and the header names no account.
fn section_header(account: Option<&str>, waiting: bool, size: usize) -> String {
    match (account, waiting) {
        (None, false) => format!("Reviews waiting for you ({size})"),
        (None, true) => format!("Waiting on author ({size})"),
        (Some(account), false) => format!("Reviews waiting for {account} ({size})"),
        (Some(account), true) => format!("Waiting on author for {account} ({size})"),
    }
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

/// Escapes the five characters that HTML gives meaning as `&amp;`, `&#39;`, `&lt;`, `&gt;`
/// and `&#34;`.
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

/// Turns GitHub-provided text into one line of at most `max` characters (see `excerpt`).
pub(crate) fn clean_text(text: &str, max: i64) -> String {
    excerpt(text, max)
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

    use super::{QueuePriority, clean_text, render_queue};
    use crate::config::Config;
    use crate::engine::{Clock, Engine};
    use crate::integrations::ReviewRequest;
    use crate::model::{Decision, PublishedRecord, PullRequest};
    use crate::protocol::fixtures::sha;
    use crate::protocol::validate::tests::test_config;
    use crate::store::Store;
    use crate::testing::fake::FakeIntegration;
    use crate::testing::harness::{integration_config, integration_pull_request};

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
    fn render_queue_escapes_text_and_links_only_the_verified_url() {
        let mut request = queue_request(
            "example-org/example-repo",
            12,
            "<img src=x onerror=\"alert(1)\"> & \"quoted\" \u{202e}gnp",
        );
        request.author = "dev<script>".to_owned();
        let entries = vec![super::QueueEntry {
            request,
            status: "AI review running".to_owned(),
            priority: QueuePriority::Other,
            account: None,
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
            rendered.formatted.contains(concat!(
                "&lt;img src=x onerror=&#34;alert(1)&#34;&gt; &amp; &#34;quoted&#34; ",
                "&lt;U+202E&gt;gnp"
            )),
            "title not escaped: {}",
            rendered.formatted
        );
        assert!(
            rendered.plain.contains("Reviews waiting for you (1)")
                && rendered
                    .plain
                    .contains("- <img src=x onerror=\"alert(1)\"> & \"quoted\" <U+202E>gnp (")
                && rendered
                    .plain
                    .contains("https://github.com/example-org/example-repo/pull/12"),
            "plain body lacks header or URL: {}",
            rendered.plain
        );
    }

    #[test]
    fn clean_text_collapses_control_characters_and_limits_length() {
        assert_eq!(clean_text("a\n\tb\u{0}  c", 10), "a b c");
        assert_eq!(clean_text("abcdef", 4), "abc…");
        assert_eq!(
            clean_text("Fix\n\u{202e}gnp.exe", 200),
            "Fix <U+202E>gnp.exe"
        );
        // The limit counts written characters; a cut keeps a `<U+XXXX>` whole or drops it.
        assert_eq!(clean_text("a\u{202e}b", 10), "a<U+202E>b");
        assert_eq!(clean_text("a\u{202e}bc", 10), "a<U+202E>…");
        assert_eq!(clean_text("ab\u{202e}cd", 10), "ab…");
    }

    /// A clock frozen at one wall time; its waits end at once.
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

    /// The deployed test configuration, built from `crate::testing::deployed_policy_document`.
    fn queue_config() -> Config {
        let document = crate::testing::deployed_policy_document();
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
        cfg.credentials = crate::config::Credentials::single_for_tests("example-user");
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
            base_branch: pr.base_branch.clone(),
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

    #[test]
    fn review_queue_configuration_is_required() {
        let mut cfg = queue_config();
        cfg.notify.queue = Vec::new();
        let error = cfg
            .validate()
            .expect_err("a queue without backends is refused");
        assert_eq!(
            error.to_string(),
            "configuration_invalid: config: invalid notification route queue"
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
        cfg.notify.queue = vec!["missing".to_owned()];
        let error = cfg
            .validate()
            .expect_err("an unknown queue backend was accepted");
        assert_eq!(
            error.to_string(),
            "configuration_invalid: config: invalid notification route queue"
        );
    }
}
