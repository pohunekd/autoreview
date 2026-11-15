//! The text guard step: it reads, bounds and scans the texts its rule names, once per review.

// Rust guideline compliant 2026-11-15

use std::collections::BTreeSet;

use autoreview_guard::{Field, FieldKind, Report, scan};
use autoreview_policy::{GuardField, OnSuspect, RuleGuard};

use super::input::STEP_GUARD;
use super::sessions::Review;
use crate::engine::Clock;
use crate::error::Error;
use crate::integrations::{GuardText, GuardTexts, Integrations};
use crate::model::{GuardOutcome, GuardSignal};

/// Reports whether this engine cannot fully read a stored guard, which then fails closed.
///
/// It is the contract of [`RuleGuard`]: such a guard is suspect and behaves as `await`.
fn unreadable(guard: &RuleGuard) -> bool {
    guard.fields.is_empty()
        || (guard.fields.iter()).any(|field| matches!(field, GuardField::Other(_)))
        || matches!(guard.on_suspect, OnSuspect::Unset | OnSuspect::Other(_))
        || guard.threshold == 0
}

/// Orders `texts` by field kind and keeps them up to `max_bytes`.
///
/// The text that crosses the bound is cut at a character boundary and the rest is dropped.
/// Returns the kept texts and whether any text was left out.
fn bound_texts(mut texts: Vec<GuardText>, max_bytes: usize) -> (Vec<GuardText>, bool) {
    texts.sort_by_key(|text| text.kind);
    let mut left = max_bytes;
    let mut kept = Vec::with_capacity(texts.len());
    for mut text in texts {
        if text.text.len() > left {
            let end = text.text.floor_char_boundary(left);
            text.text.truncate(end);
            kept.push(text);
            return (kept, true);
        }
        left -= text.text.len();
        kept.push(text);
    }
    (kept, false)
}

/// Returns the outcome of a guard that could not scan: no lexicon is configured.
fn unscanned(guard: &RuleGuard) -> GuardOutcome {
    GuardOutcome {
        threshold: guard.threshold,
        unscanned: true,
        suspect: true,
        outcome: OnSuspect::Await,
        ..GuardOutcome::default()
    }
}

/// Returns the outcome of `report` under `guard`; a cut text and an unreadable guard are suspect.
fn outcome_of(guard: &RuleGuard, report: &Report, truncated: bool) -> GuardOutcome {
    let unreadable = unreadable(guard);
    let signal = |hit: &autoreview_guard::Hit| GuardSignal {
        field: hit.field.as_str().to_owned(),
        signal: hit.signal.as_str().to_owned(),
        count: hit.count,
    };
    GuardOutcome {
        signals: report.hits.iter().map(signal).collect(),
        score: report.score,
        threshold: guard.threshold,
        truncated,
        unscanned: false,
        suspect: unreadable || truncated || report.score >= guard.threshold,
        outcome: if unreadable {
            OnSuspect::Await
        } else {
            guard.on_suspect.clone()
        },
    }
}

impl<I: Integrations, C: Clock> Review<'_, I, C> {
    /// Runs the text guard of the rule once per review.
    ///
    /// A resumed review reads the stored result. A failed read gets the step retries and then
    /// fails the review, so nothing is published unscanned.
    ///
    /// # Errors
    ///
    /// Returns the error of the read or of the store.
    pub(crate) async fn text_guard(&self, guard: &RuleGuard) -> Result<GuardOutcome, Error> {
        let outcome = self
            .once::<GuardOutcome, _>(STEP_GUARD, || {
                let integration = &*self.engine.integration;
                let (cancel, credential) = (&self.cancel, self.credential());
                let (input, settings) = (&self.input, self.engine.config.text_guard.as_ref());
                async move {
                    let Some(settings) = settings else {
                        return Ok(unscanned(guard));
                    };
                    let fields: BTreeSet<FieldKind> = (guard.fields.iter())
                        .filter_map(|field| match field {
                            GuardField::Kind(kind) => Some(*kind),
                            GuardField::Other(_) => None,
                        })
                        .collect();
                    let read = if fields.is_empty() {
                        GuardTexts::default()
                    } else {
                        let (pull_request, max) = (&input.pull_request, settings.max_input_bytes);
                        let publisher = &input.publisher_login;
                        (integration.read_guard_texts(
                            cancel,
                            credential?,
                            pull_request,
                            &fields,
                            max,
                            publisher,
                        ))
                        .await?
                    };
                    let (texts, cut) = bound_texts(read.texts, settings.max_input_bytes);
                    let fields: Vec<Field<'_>> = (texts.iter())
                        .map(|text| Field {
                            kind: text.kind,
                            text: &text.text,
                        })
                        .collect();
                    let report = scan(&fields, &settings.lexicon);
                    Ok(outcome_of(guard, &report, read.truncated || cut))
                }
            })
            .await?;
        Ok(outcome)
    }
}
