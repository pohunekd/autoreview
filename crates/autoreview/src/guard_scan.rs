//! The `guard scan` command: the text guard over a corpus of pull request texts, offline.
//!
//! It reads a JSONL corpus, one pull request per line, and scans each with the bound and the
//! scan of the review step. It makes no network call and prints signal codes, never text.

#![expect(dead_code, reason = "called by guard scan from round 3")]

// Rust guideline compliant 2026-11-15

use std::collections::BTreeMap;
use std::num::NonZeroU64;

use autoreview_engine::model::GuardSignal;
use autoreview_guard::FieldKind;
use serde::{Deserialize, Deserializer, Serialize};

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
