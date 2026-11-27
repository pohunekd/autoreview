//! The review queue: the waiting requests and notes, posted as one digest.

// Rust guideline compliant 2026-06-26

use std::collections::{BTreeMap, BTreeSet};

use autoreview_go_compat::go_time::{format_rfc3339_seconds, parse_rfc3339};
use autoreview_policy::{Policy, TriggerKind};
use autoreview_schedule::phase;
use autoreview_schedule::scheduler::Record;
use serde::Serialize;
use time::{Date, OffsetDateTime, UtcOffset};
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
use crate::report::ReportRequest;

/// UTC days from the last weekly report send to the next, and the days before today it covers.
const WEEKLY_REPORT_DAYS: i64 = 7;

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
    /// Whether the digest ended with the weekly report.
    pub weekly_report: bool,
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
    /// Whether both bodies end with the weekly report.
    pub(crate) weekly: bool,
}

impl<I: Integrations, C: Clock> Engine<I, C> {
    /// Posts the review requests and the notes that wait, as one queue digest.
    ///
    /// A due weekly report ends the digest, and is the whole digest when nothing waits; nothing
    /// is posted when neither is there. The report is left out with a warning when it and one
    /// entry do not fit. The post gets one attempt, bounded by the notifier's request timeout; a
    /// failure is logged and returned. Each run has its own transaction ID. The send of the
    /// report is recorded after every backend took the digest.
    ///
    /// # Errors
    ///
    /// Returns the error of the search, the store or the post, `review_queue_too_long` when no
    /// entry fits, `matrix_request_invalid` without a transaction ID, `configuration_invalid`
    /// without a policy or with a negative limit, and the error of the discovery lock.
    pub async fn review_queue(&self, cancel: &CancellationToken) -> Result<QueueResult, Error> {
        let (entries, truncated) = self.queue_snapshot(cancel).await?;
        let now = self.clock.wall_now().to_offset(UtcOffset::UTC);
        let weekly = self.weekly_report(cancel, now)?;
        if entries.is_empty() && weekly.is_none() {
            return Ok(QueueResult::default());
        }
        let rendered = render_queue(
            &entries,
            truncated,
            weekly.as_deref(),
            self.config.matrix_max_characters,
            self.config.max_title_characters,
        );
        if weekly.is_some() && !rendered.weekly {
            tracing::warn!(
                error_code = REVIEW_QUEUE_TOO_LONG,
                "weekly report dropped: it does not fit notify.max_characters"
            );
        }
        if rendered.shown == 0 && !rendered.weekly {
            if entries.is_empty() {
                return Ok(QueueResult::default());
            }
            return Err(Error::workflow(REVIEW_QUEUE_TOO_LONG, true, None));
        }
        let transaction = queue_transaction_id()?;
        self.send_queue(cancel, &transaction, &rendered).await?;
        if rendered.weekly {
            self.with_discovery(|| async {
                live(cancel)?;
                self.store.update_discovery(|state, _| {
                    state.weekly_report_sent = Some(format_rfc3339_seconds(now));
                    Ok(())
                })
            })
            .await?;
        }
        Ok(QueueResult {
            count: i64::try_from(rendered.shown).unwrap_or(i64::MAX),
            sent: true,
            truncated: rendered.dropped,
            weekly_report: rendered.weekly,
        })
    }

    /// Returns the text of the weekly report when the policy asks for it and it is due at `now`.
    ///
    /// The report covers every repository and pipeline from `WEEKLY_REPORT_DAYS` UTC days
    /// before today to today. A failed store read or report is logged and left out, so the
    /// queue is still sent.
    ///
    /// # Errors
    ///
    /// Returns `configuration_invalid` without a policy, and the shutdown error after a cancel.
    fn weekly_report(
        &self,
        cancel: &CancellationToken,
        now: OffsetDateTime,
    ) -> Result<Option<String>, Error> {
        if !self.policy()?.weekly_report() {
            return Ok(None);
        }
        let report = self.store.load_discovery().and_then(|state| {
            let last = state.weekly_report_sent.as_deref();
            let Some(since) = weekly_report_since(last, now) else {
                return Ok(None);
            };
            let request = ReportRequest {
                since: since.to_string(),
                ..ReportRequest::default()
            };
            let document = self.review_report(cancel, &request)?;
            Ok(Some(document.to_string()))
        });
        report.or_else(|err| {
            live(cancel)?;
            tracing::warn!(error_code = error_code(&err), "weekly report skipped");
            Ok(None)
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

/// Returns the first UTC day of the weekly report when one is due at `now`.
///
/// It is due without a last send, with one that does not parse, `WEEKLY_REPORT_DAYS` or more
/// UTC days after the day of the last send, and when that day is after today, because the
/// clock went back: one send then records the current time.
pub(crate) fn weekly_report_since(last: Option<&str>, now: OffsetDateTime) -> Option<Date> {
    let today = now.to_offset(UtcOffset::UTC).date();
    let since = today.checked_sub(time::Duration::days(WEEKLY_REPORT_DAYS))?;
    let sent = last.and_then(|text| parse_rfc3339(text).ok());
    match sent.map(|at| at.to_offset(UtcOffset::UTC).date()) {
        Some(day) if day > since && day <= today => None,
        _ => Some(since),
    }
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

/// Builds the plain-text and HTML digests with as many entries as fit, then the `weekly`
/// report when given.
///
/// Each body holds at most `limit` characters and never cuts an entry or the report. The
/// report is left out when it does not fit with at least one entry, or alone without entries.
pub(crate) fn render_queue(
    entries: &[QueueEntry],
    truncated: bool,
    weekly: Option<&str>,
    limit: i64,
    title_limit: i64,
) -> RenderedQueue {
    let mut fitted = None;
    // Without entries, the digest is the report alone.
    for count in usize::from(!entries.is_empty())..=entries.len() {
        let (plain, formatted) = build_digest(entries, count, truncated, weekly, title_limit);
        if exceeds(&plain, limit) || exceeds(&formatted, limit) {
            break;
        }
        fitted = Some((count, plain, formatted));
    }
    let Some((shown, plain, formatted)) = fitted else {
        if weekly.is_some() {
            return render_queue(entries, truncated, None, limit, title_limit);
        }
        return RenderedQueue {
            plain: String::new(),
            formatted: String::new(),
            shown: 0,
            dropped: true,
            weekly: false,
        };
    };
    RenderedQueue {
        plain,
        formatted,
        shown,
        dropped: truncated || shown < entries.len(),
        weekly: weekly.is_some(),
    }
}

/// Renders the first `count` entries as the plain-text and the HTML body, then `weekly`.
///
/// The entries are grouped by account, in the order of the account labels, and keep their
/// order within an account. Each account has its requests first, then its notes. The report
/// follows a blank line in the plain body and is preformatted text in the HTML body.
fn build_digest(
    entries: &[QueueEntry],
    count: usize,
    truncated: bool,
    weekly: Option<&str>,
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
    if count > 0 {
        formatted.push_str("</ul>");
    }
    if count > 0 && (truncated || count < entries.len()) {
        plain.push_str(TRUNCATED_TEXT);
        plain.push('\n');
        formatted.push_str("\n<p><em>");
        formatted.push_str(&escape_html(TRUNCATED_TEXT));
        formatted.push_str("</em></p>");
    }
    let mut plain = plain.trim_end_matches('\n').to_owned();
    if let Some(report) = weekly {
        let report = report.trim_end_matches('\n');
        if count > 0 {
            plain.push_str("\n\n");
            formatted.push('\n');
        }
        plain.push_str(report);
        formatted.push_str("<pre>");
        formatted.push_str(&escape_html(report));
        formatted.push_str("</pre>");
    }
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

    use autoreview_policy::{Level, Policy};
    use tempfile::TempDir;
    use time::OffsetDateTime;
    use time::macros::{date, datetime};
    use tokio_util::sync::CancellationToken;

    use crate::config::{Config, Credential, Credentials};
    use crate::engine::{Clock, Engine};
    use crate::integrations::ReviewRequest;
    use crate::model::{Decision, PublishedRecord, PullRequest, ReviewInput};
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
        let mut cfg = test_config();
        cfg.policy = Some(deployed_policy(""));
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

    /// The deployed test policy with `notifications` lines `extra` added.
    fn deployed_policy(extra: &str) -> Policy {
        let document = crate::testing::deployed_policy_document().replacen(
            "notifications:\n",
            &format!("notifications:\n{extra}"),
            1,
        );
        autoreview_policy::parse(document.as_bytes(), |relative| {
            std::fs::read(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../testdata")
                    .join(relative),
            )
        })
        .expect("the deployed policy parses")
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
        // Several credentials: each account has its own sections. The hand-off of a failed
        // credential or of a repository that no credential serves is left out unread. A
        // hand-off of a review stored with `main` is read with it and listed under its account
        // after `team` was put before it (#373).
        let account = |name: &str, login: &str, pattern: &str| Credential {
            name: name.to_owned(),
            login: login.to_owned(),
            token: format!("{name}-token"),
            matches: vec![pattern.to_owned()],
            review_teams: Vec::new(),
        };
        let mut cfg = queue_config();
        cfg.credentials = Credentials::new(vec![
            account("team", "team-bot", "example-org/beta"),
            account("main", "example-user", "example-org/*"),
        ])
        .expect("the credentials load");
        // No read is retried, so a read that must not happen shows in the counter.
        cfg.step_retry = Duration::ZERO;
        let fake = Arc::new(FakeIntegration::new());
        let (engine, _dir) = new_queue_engine(cfg, &fake, at);
        let (mut team, mut foreign) = (waiting.clone(), waiting.clone());
        team.repository = "example-org/beta".to_owned();
        foreign.repository = "other-org/tool".to_owned();
        let mut stored = team.clone();
        stored.number = 6;
        let input = ReviewInput {
            pull_request: stored.clone(),
            publisher_login: "example-user".to_owned(),
            credential: "main".to_owned(),
            ..ReviewInput::default()
        };
        (engine.store)
            .in_tx(|transaction| {
                (engine.store).enqueue_review(transaction, &stored.workflow_id(0), &input)
            })
            .expect("the review of the hand-off is stored");
        for pr in [&team, &foreign, &stored] {
            hand_off(&engine, &fake, pr, at - time::Duration::hours(30), false).await;
        }
        fake.lock().requests = vec![
            queue_request("example-org/example-repo", 1, "first"),
            queue_request("example-org/beta", 2, "second"),
        ];
        let cancel = CancellationToken::new();
        engine.review_queue(&cancel).await.expect("the queue runs");
        fake.lock().failing_credentials = [("team".into(), "github_request_failed".into())].into();
        let result = engine.review_queue(&cancel).await.expect("the queue runs");
        let state = fake.lock();
        let (plain, failed) = (&state.queued[0].plain, &state.queued[1].plain);
        let main = plain.find("Reviews waiting for example-user (credential main) (1)\n- first");
        let team = plain.find("Reviews waiting for team-bot (credential team) (1)\n- second");
        let notes = plain.find("Waiting on author for team-bot (credential team) (1)\n");
        let own = "Waiting on author for example-user (credential main) (1)\n- Optional polish \
                   (example-org/beta#6,";
        let reads = [
            "example-org/beta#5 team",
            "example-org/beta#6 main",
            "example-org/beta#6 main",
        ];
        assert!(
            main.is_some()
                && main < team
                && team < notes
                && result.truncated
                && failed.starts_with("Reviews waiting for example-user (credential main) (1)")
                && !failed.contains("team-bot")
                && plain.contains(own)
                && failed.contains(own)
                && state.state_reads == reads,
            "plain={plain} failed={failed} reads={:?}",
            state.state_reads
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

    #[test]
    fn weekly_report_is_due_seven_utc_days_after_the_last_send() {
        let now = datetime!(2027-01-03 00:30 UTC);
        let since = Some(date!(2026 - 12 - 27));
        let cases = [
            ("no send yet", None, since),
            ("earlier today", Some("2027-01-03T00:10:00Z"), None),
            ("six UTC days", Some("2026-12-28T00:00:00Z"), None),
            (
                "seven UTC days, under 168 hours",
                Some("2026-12-27T23:59:59Z"),
                since,
            ),
            ("thirty days", Some("2026-12-04T12:00:00Z"), since),
            (
                "an offset on the sixth UTC day",
                Some("2026-12-28T01:00:00+01:00"),
                None,
            ),
            (
                "an offset on the seventh UTC day",
                Some("2026-12-28T00:30:00+01:00"),
                since,
            ),
            (
                "later today, the clock went back",
                Some("2027-01-03T08:00:00Z"),
                None,
            ),
            (
                "a later day, the clock went back",
                Some("2027-01-04T00:00:00Z"),
                since,
            ),
            ("unparseable", Some("last week"), since),
        ];
        for (name, last, want) in cases {
            assert_eq!(super::weekly_report_since(last, now), want, "{name}");
        }
    }

    #[tokio::test]
    async fn weekly_report_ends_the_queue_digest_once_a_week() {
        let at = datetime!(2027-01-03 12:00 UTC);
        let weekly_config = || {
            let mut cfg = queue_config();
            cfg.policy = Some(deployed_policy("  weekly_report: digest\n"));
            cfg
        };
        let fake = Arc::new(FakeIntegration::new());
        let (engine, _dir) = new_queue_engine(weekly_config(), &fake, at);
        let cancel = CancellationToken::new();
        let sent = |engine: &QueueEngine| {
            let state = engine.store.load_discovery().expect("the state loads");
            state.weekly_report_sent
        };
        let set_sent = |value: &str| {
            (engine.store)
                .update_discovery(|state, _| {
                    state.weekly_report_sent = Some(value.to_owned());
                    Ok(())
                })
                .expect("the last send is stored");
        };
        // The range crosses the year boundary.
        let header = concat!(
            "Report 2026-12-27 to 2027-01-03 (UTC days), ",
            "repository every, pipeline every"
        );
        // Nothing waits: the digest is the report alone, and a second run sends nothing.
        let first = engine.review_queue(&cancel).await.expect("the queue runs");
        let second = engine.review_queue(&cancel).await.expect("the queue runs");
        let queued = fake.lock().queued.clone();
        assert!(
            first.sent
                && first.count == 0
                && first.weekly_report
                && second == super::QueueResult::default()
                && queued.len() == 1
                && queued[0].plain.starts_with(header)
                && queued[0].formatted.starts_with(&format!("<pre>{header}"))
                && sent(&engine).as_deref() == Some("2027-01-03T12:00:00Z"),
            "first {first:?} second {second:?} queued {queued:?}"
        );
        // Six UTC days after the last send, the waiting request goes out without the report.
        fake.lock().requests = vec![queue_request("example-org/example-repo", 1, "first")];
        set_sent("2026-12-28T00:00:00Z");
        let early = engine.review_queue(&cancel).await.expect("the queue runs");
        // Seven UTC days: a failed post records nothing, so the next run sends the report.
        set_sent("2026-12-27T23:59:59Z");
        fake.lock().queue_failure = Some("webhook_unavailable".to_owned());
        let failed = engine.review_queue(&cancel).await;
        fake.lock().queue_failure = None;
        let unsent = sent(&engine);
        let due = engine.review_queue(&cancel).await.expect("the queue runs");
        let queued = fake.lock().queued.clone();
        assert!(
            !early.weekly_report
                && !queued[1].plain.contains("Report ")
                && failed.is_err()
                && unsent.as_deref() == Some("2026-12-27T23:59:59Z")
                && due.weekly_report
                && due.count == 1
                && queued.len() == 3
                && queued[2].plain.contains(&format!("/pull/1\n\n{header}"))
                && queued[2]
                    .formatted
                    .contains(&format!("</ul>\n<pre>{header}"))
                && sent(&engine).as_deref() == Some("2027-01-03T12:00:00Z"),
            "early {early:?} failed {failed:?} due {due:?} queued {queued:?}"
        );
        // A report that does not fit with one entry is left out and not recorded.
        let mut cfg = weekly_config();
        let entry_only = [&queued[1].plain, &queued[1].formatted].map(|body| body.chars().count());
        let limit = entry_only[0].max(entry_only[1]);
        cfg.matrix_max_characters = i64::try_from(limit).expect("the length fits");
        let fake = Arc::new(FakeIntegration::new());
        fake.lock().requests = vec![queue_request("example-org/example-repo", 1, "first")];
        let (small, _small_dir) = new_queue_engine(cfg, &fake, at);
        let result = small.review_queue(&cancel).await.expect("the queue runs");
        let queued = fake.lock().queued.clone();
        assert!(
            result.sent
                && result.count == 1
                && !result.weekly_report
                && !queued[0].plain.contains("Report ")
                && sent(&small).is_none(),
            "result {result:?} queued {queued:?}"
        );
    }
}
