//! The `guard scan` command: the text guard over a corpus of pull request texts, offline.
//!
//! It reads a JSONL corpus, one pull request per line, and scans each with the bound and the
//! scan of the review step. It makes no network call and prints signal codes, never text.

#![expect(dead_code, reason = "called by guard scan from round 3")]

// Rust guideline compliant 2026-11-15

use std::collections::{BTreeMap, BTreeSet};
use std::io::BufRead;
use std::num::NonZeroU64;

use autoreview_engine::config::TextGuard;
use autoreview_engine::integrations::{GuardText, GuardTexts};
use autoreview_engine::model::GuardSignal;
use autoreview_engine::{Error, guard_outcome};
use autoreview_guard::{FieldKind, Signal};
use autoreview_policy::RuleGuard;
use serde::{Deserialize, Deserializer, Serialize};

/// Error code of an unreadable, empty or malformed corpus file.
const GUARD_CORPUS_INVALID: &str = "guard_corpus_invalid";

/// One line of the corpus: one pull request; every key is required.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    /// The repository, as `owner/name`.
    repository: String,
    /// The pull request number.
    number: NonZeroU64,
    /// The texts of the pull request, in any order.
    texts: Vec<RecordText>,
    /// Whether the fetch left text out, by the rules of the review step's read.
    truncated: bool,
}

/// One text of a corpus line.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordText {
    /// The field kind code, such as `body`.
    #[serde(deserialize_with = "field_kind")]
    kind: FieldKind,
    /// The text; for `patch` the unified diff of one file.
    text: String,
}

/// What `guard scan` prints: one entry per corpus line, then the summary.
#[derive(Debug, Serialize, PartialEq)]
pub(crate) struct ScanResult {
    /// The pull requests, in corpus order.
    pull_requests: Vec<PullRequestScan>,
    /// The counts over the corpus.
    summary: Summary,
}

/// The guard result of one pull request: codes only, never text.
#[derive(Debug, Serialize, PartialEq)]
struct PullRequestScan {
    /// The repository of the corpus line.
    repository: String,
    /// The pull request number of the corpus line.
    number: u64,
    /// The score of the scan.
    score: u32,
    /// The signals found, one per field kind and signal.
    signals: Vec<GuardSignal>,
    /// Whether the fetch or the input bound left text out.
    truncated: bool,
    /// Whether the pull request counts as suspect: the score reaches the threshold, or truncated.
    suspect: bool,
}

/// The counts of a corpus scan.
#[derive(Debug, Serialize, PartialEq)]
struct Summary {
    /// Pull requests scanned.
    pull_requests: u64,
    /// Pull requests that count as suspect.
    flagged: u64,
    /// `flagged` as a percentage of `pull_requests`, rounded down to two decimals.
    flagged_percent: f64,
    /// Pull requests with at least one signal of the hidden-text class.
    hidden_text: u64,
    /// Pull requests whose text was left out in part.
    truncated: u64,
    /// For each signal code found, the pull requests with that signal.
    signals: BTreeMap<String, u64>,
}

/// Reads a field kind from its code; an unknown code is refused.
fn field_kind<'de, D: Deserializer<'de>>(deserializer: D) -> Result<FieldKind, D::Error> {
    let code = String::deserialize(deserializer)?;
    (FieldKind::ALL.into_iter())
        .find(|kind| kind.as_str() == code)
        .ok_or_else(|| serde::de::Error::custom("unknown field kind"))
}

/// Returns the `guard_corpus_invalid` error with `cause`.
fn corpus_invalid(cause: impl Into<autoreview_engine::BoxError>) -> Error {
    Error::workflow(GUARD_CORPUS_INVALID, true, Some(cause.into()))
}

/// Scans every line of `corpus` with `guard` and `settings` and counts the results.
///
/// A malformed line names its number and the kind of error, never the text of the line.
///
/// # Errors
///
/// Returns `guard_corpus_invalid` for a read error, a malformed line, an empty line or a
/// corpus without a line.
fn scan_corpus(
    corpus: impl BufRead,
    guard: &RuleGuard,
    settings: &TextGuard,
) -> Result<ScanResult, Error> {
    let hidden: BTreeSet<&str> = (Signal::ALL.into_iter())
        .filter(|signal| signal.hides_text())
        .map(Signal::as_str)
        .collect();
    let mut pull_requests = Vec::new();
    let mut signals: BTreeMap<String, u64> = BTreeMap::new();
    let (mut flagged, mut hidden_text, mut truncated) = (0_u64, 0_u64, 0_u64);
    for (index, line) in corpus.lines().enumerate() {
        let number = index.saturating_add(1);
        let line =
            line.map_err(|cause| corpus_invalid(format!("corpus line {number}: {cause}")))?;
        let record: Record = serde_json::from_str(&line).map_err(|cause| {
            let (kind, column) = (cause.classify(), cause.column());
            corpus_invalid(format!(
                "corpus line {number}: {kind:?} error at column {column}"
            ))
        })?;
        let read = GuardTexts {
            texts: (record.texts.into_iter())
                .map(|text| GuardText {
                    kind: text.kind,
                    text: text.text,
                })
                .collect(),
            truncated: record.truncated,
        };
        let outcome = guard_outcome(guard, settings, read);
        let codes: BTreeSet<&str> = (outcome.signals.iter())
            .map(|signal| signal.signal.as_str())
            .collect();
        for code in &codes {
            *signals.entry((*code).to_owned()).or_insert(0) += 1;
        }
        flagged += u64::from(outcome.suspect);
        hidden_text += u64::from(codes.iter().any(|code| hidden.contains(code)));
        truncated += u64::from(outcome.truncated);
        pull_requests.push(PullRequestScan {
            repository: record.repository,
            number: record.number.get(),
            score: outcome.score,
            signals: outcome.signals,
            truncated: outcome.truncated,
            suspect: outcome.suspect,
        });
    }
    let count = u64::try_from(pull_requests.len()).unwrap_or(u64::MAX);
    if count == 0 {
        return Err(corpus_invalid("the corpus has no pull request"));
    }
    // Basis points never exceed 10000, so the conversion to `u32` is exact.
    let basis_points = u32::try_from(flagged.saturating_mul(10_000) / count).unwrap_or(u32::MAX);
    Ok(ScanResult {
        pull_requests,
        summary: Summary {
            pull_requests: count,
            flagged,
            flagged_percent: f64::from(basis_points) / 100.0,
            hidden_text,
            truncated,
            signals,
        },
    })
}
