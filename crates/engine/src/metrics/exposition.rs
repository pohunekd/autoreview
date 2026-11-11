//! The Prometheus text exposition, format 0.0.4, of the gauges and the review counters.

// Rust guideline compliant 2026-06-26

use super::counts::{Counts, Gauges, PUBLICATION_BUCKETS, ReviewCounts};
use crate::stats::StateCounts;

/// One gauge family of the review states: its name, its help text and the count it shows.
type StateFamily = (&'static str, &'static str, fn(&StateCounts) -> i64);

/// One counter family of repository and rule: its name, its help text and the count it shows.
type CountFamily = (&'static str, &'static str, fn(&ReviewCounts) -> u64);

/// Writes the gauges and the counters in the text exposition format 0.0.4.
///
/// A family without a sample still has its `HELP` and `TYPE` lines.
pub(crate) fn render(gauges: &Gauges, counts: &Counts) -> String {
    let mut out = String::new();
    let states: [StateFamily; 3] = [
        (
            "autoreview_reviews_queued",
            "Reviews dispatched that no worker runs yet.",
            |state| state.queued,
        ),
        (
            "autoreview_reviews_running",
            "Reviews that a worker runs.",
            |state| state.running,
        ),
        (
            "autoreview_reviews_awaiting",
            "Clean reviews that wait for an approval decision.",
            |state| state.awaiting,
        ),
    ];
    for (name, help, value) in states {
        header(&mut out, name, "gauge", help);
        for ((repository, host), state) in &gauges.reviews {
            let labels = [("repository", repository.as_str()), ("host", host.as_str())];
            sample(&mut out, name, &labels, value(state));
        }
    }
    let name = "autoreview_pull_requests_stopped";
    let help = "Pull requests stopped after their retries.";
    header(&mut out, name, "gauge", help);
    for (repository, stopped) in &gauges.stopped {
        sample(&mut out, name, &[("repository", repository)], *stopped);
    }
    let totals: [CountFamily; 4] = [
        (
            "autoreview_reviews_started_total",
            "Review attempts that a worker took from the queue.",
            |review| review.started,
        ),
        (
            "autoreview_reviews_completed_total",
            "Review attempts completed, whatever their result.",
            |review| review.completed,
        ),
        (
            "autoreview_reviews_superseded_total",
            "Review attempts that a newer state replaced.",
            |review| review.superseded,
        ),
        (
            "autoreview_reviews_approval_expired_total",
            "Approval waits that reached their deadline.",
            |review| review.approval_expired,
        ),
    ];
    for (name, help, value) in totals {
        header(&mut out, name, "counter", help);
        for ((repository, rule), review) in &counts.reviews {
            let labels = [("repository", repository.as_str()), ("rule", rule.as_str())];
            sample(&mut out, name, &labels, value(review));
        }
    }
    let name = "autoreview_reviews_published_total";
    header(
        &mut out,
        name,
        "counter",
        "Review attempts that published a review.",
    );
    for ((repository, rule), review) in &counts.reviews {
        for (decision, count) in &review.published {
            let labels = [
                ("repository", repository.as_str()),
                ("rule", rule.as_str()),
                ("decision", *decision),
            ];
            sample(&mut out, name, &labels, count);
        }
    }
    let name = "autoreview_reviews_failed_total";
    header(
        &mut out,
        name,
        "counter",
        "Failed review attempts by error code.",
    );
    for ((repository, rule), review) in &counts.reviews {
        for (code, count) in &review.failed {
            let labels = [
                ("repository", repository.as_str()),
                ("rule", rule.as_str()),
                ("error_code", code.as_str()),
            ];
            sample(&mut out, name, &labels, count);
        }
    }
    histogram(&mut out, counts);
    out
}

/// Writes the histogram of the seconds from creation to completion of published attempts.
fn histogram(out: &mut String, counts: &Counts) {
    let name = "autoreview_review_publication_seconds";
    header(
        out,
        name,
        "histogram",
        "Seconds from creation to completion of publications.",
    );
    let bucket = format!("{name}_bucket");
    for ((repository, rule), review) in &counts.reviews {
        let (repository, rule) = (repository.as_str(), rule.as_str());
        for (bound, count) in PUBLICATION_BUCKETS.iter().zip(review.buckets) {
            let le = bound.to_string();
            let labels = [("repository", repository), ("rule", rule), ("le", &le)];
            sample(out, &bucket, &labels, count);
        }
        let labels = [("repository", repository), ("rule", rule), ("le", "+Inf")];
        sample(out, &bucket, &labels, review.seconds_count);
        let labels = [("repository", repository), ("rule", rule)];
        sample(out, &format!("{name}_sum"), &labels, review.seconds_sum);
        sample(out, &format!("{name}_count"), &labels, review.seconds_count);
    }
}

/// Writes the `HELP` and `TYPE` lines of a family; the help texts need no escaping.
fn header(out: &mut String, name: &str, kind: &str, help: &str) {
    out.push_str(&format!("# HELP {name} {help}\n# TYPE {name} {kind}\n"));
}

/// Writes one sample line with its labels in the given order.
fn sample(out: &mut String, name: &str, labels: &[(&str, &str)], value: impl ToString) {
    out.push_str(name);
    out.push('{');
    for (index, (label, text)) in labels.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(label);
        out.push_str("=\"");
        escape_into(out, text);
        out.push('"');
    }
    out.push_str("} ");
    out.push_str(&value.to_string());
    out.push('\n');
}

/// Escapes a label value: backslash, double quote and line feed, as the format requires.
fn escape_into(out: &mut String, text: &str) {
    for character in text.chars() {
        match character {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            other => out.push(other),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use autoreview_schedule::scheduler::Record;

    use super::render;
    use crate::metrics::counts::{Counts, Gauges};
    use crate::store::StoredReview;

    /// A rule `match` with the three characters that a label value escapes.
    const ODD_RULE: &str = "d/\"x\"\\y\nz";

    /// The exposition of the fixed counts; it starts after the first line break.
    const EXPECTED: &str = r#"
# HELP autoreview_reviews_queued Reviews dispatched that no worker runs yet.
# TYPE autoreview_reviews_queued gauge
autoreview_reviews_queued{repository="a/w",host="host-a"} 2
autoreview_reviews_queued{repository="a/w",host="host-b"} 0
# HELP autoreview_reviews_running Reviews that a worker runs.
# TYPE autoreview_reviews_running gauge
autoreview_reviews_running{repository="a/w",host="host-a"} 0
autoreview_reviews_running{repository="a/w",host="host-b"} 1
# HELP autoreview_reviews_awaiting Clean reviews that wait for an approval decision.
# TYPE autoreview_reviews_awaiting gauge
autoreview_reviews_awaiting{repository="a/w",host="host-a"} 1
autoreview_reviews_awaiting{repository="a/w",host="host-b"} 0
# HELP autoreview_pull_requests_stopped Pull requests stopped after their retries.
# TYPE autoreview_pull_requests_stopped gauge
autoreview_pull_requests_stopped{repository="a/g"} 1
autoreview_pull_requests_stopped{repository="a/w"} 2
# HELP autoreview_reviews_started_total Review attempts that a worker took from the queue.
# TYPE autoreview_reviews_started_total counter
autoreview_reviews_started_total{repository="a/o",rule="d/\"x\"\\y\nz"} 1
autoreview_reviews_started_total{repository="a/w",rule="*"} 2
# HELP autoreview_reviews_completed_total Review attempts completed, whatever their result.
# TYPE autoreview_reviews_completed_total counter
autoreview_reviews_completed_total{repository="a/o",rule="d/\"x\"\\y\nz"} 2
autoreview_reviews_completed_total{repository="a/w",rule="*"} 5
# HELP autoreview_reviews_superseded_total Review attempts that a newer state replaced.
# TYPE autoreview_reviews_superseded_total counter
autoreview_reviews_superseded_total{repository="a/o",rule="d/\"x\"\\y\nz"} 0
autoreview_reviews_superseded_total{repository="a/w",rule="*"} 1
# HELP autoreview_reviews_approval_expired_total Approval waits that reached their deadline.
# TYPE autoreview_reviews_approval_expired_total counter
autoreview_reviews_approval_expired_total{repository="a/o",rule="d/\"x\"\\y\nz"} 1
autoreview_reviews_approval_expired_total{repository="a/w",rule="*"} 0
# HELP autoreview_reviews_published_total Review attempts that published a review.
# TYPE autoreview_reviews_published_total counter
autoreview_reviews_published_total{repository="a/o",rule="d/\"x\"\\y\nz",decision="commented"} 1
autoreview_reviews_published_total{repository="a/w",rule="*",decision="approved_after_wait"} 1
autoreview_reviews_published_total{repository="a/w",rule="*",decision="approved_without_human"} 1
# HELP autoreview_reviews_failed_total Failed review attempts by error code.
# TYPE autoreview_reviews_failed_total counter
autoreview_reviews_failed_total{repository="a/w",rule="*",error_code="github_request_failed"} 2
# HELP autoreview_review_publication_seconds Seconds from creation to completion of publications.
# TYPE autoreview_review_publication_seconds histogram
autoreview_review_publication_seconds_bucket{repository="a/o",rule="d/\"x\"\\y\nz",le="60"} 0
autoreview_review_publication_seconds_bucket{repository="a/o",rule="d/\"x\"\\y\nz",le="300"} 0
autoreview_review_publication_seconds_bucket{repository="a/o",rule="d/\"x\"\\y\nz",le="900"} 0
autoreview_review_publication_seconds_bucket{repository="a/o",rule="d/\"x\"\\y\nz",le="1800"} 0
autoreview_review_publication_seconds_bucket{repository="a/o",rule="d/\"x\"\\y\nz",le="3600"} 0
autoreview_review_publication_seconds_bucket{repository="a/o",rule="d/\"x\"\\y\nz",le="14400"} 0
autoreview_review_publication_seconds_bucket{repository="a/o",rule="d/\"x\"\\y\nz",le="43200"} 0
autoreview_review_publication_seconds_bucket{repository="a/o",rule="d/\"x\"\\y\nz",le="86400"} 0
autoreview_review_publication_seconds_bucket{repository="a/o",rule="d/\"x\"\\y\nz",le="259200"} 0
autoreview_review_publication_seconds_bucket{repository="a/o",rule="d/\"x\"\\y\nz",le="+Inf"} 0
autoreview_review_publication_seconds_sum{repository="a/o",rule="d/\"x\"\\y\nz"} 0
autoreview_review_publication_seconds_count{repository="a/o",rule="d/\"x\"\\y\nz"} 0
autoreview_review_publication_seconds_bucket{repository="a/w",rule="*",le="60"} 0
autoreview_review_publication_seconds_bucket{repository="a/w",rule="*",le="300"} 1
autoreview_review_publication_seconds_bucket{repository="a/w",rule="*",le="900"} 1
autoreview_review_publication_seconds_bucket{repository="a/w",rule="*",le="1800"} 1
autoreview_review_publication_seconds_bucket{repository="a/w",rule="*",le="3600"} 1
autoreview_review_publication_seconds_bucket{repository="a/w",rule="*",le="14400"} 2
autoreview_review_publication_seconds_bucket{repository="a/w",rule="*",le="43200"} 2
autoreview_review_publication_seconds_bucket{repository="a/w",rule="*",le="86400"} 2
autoreview_review_publication_seconds_bucket{repository="a/w",rule="*",le="259200"} 2
autoreview_review_publication_seconds_bucket{repository="a/w",rule="*",le="+Inf"} 2
autoreview_review_publication_seconds_sum{repository="a/w",rule="*"} 7500
autoreview_review_publication_seconds_count{repository="a/w",rule="*"} 2
"#;

    /// Returns a queued review of `repository` under `rule` on `host-a`.
    fn review(id: &str, repository: &str, rule: &str, created_at: &str) -> StoredReview {
        let mut review = StoredReview {
            workflow_id: id.to_owned(),
            status: "queued".to_owned(),
            created_at: created_at.to_owned(),
            ..StoredReview::default()
        };
        review.input.pull_request.repository = repository.to_owned();
        review.input.rule.match_glob = rule.to_owned();
        review.input.route.host = "host-a".to_owned();
        review
    }

    /// Completes `review` at `completed_at` with `result`, the failure code and the outcome.
    fn done(
        mut review: StoredReview,
        completed_at: &str,
        result: &str,
        failure: &str,
        outcome: &str,
    ) -> StoredReview {
        review.status = "completed".to_owned();
        review.result = result.to_owned();
        review.failure = failure.to_owned();
        review.outcome = outcome.to_owned();
        review.completed_at = completed_at.to_owned();
        review
    }

    /// Returns the stored outcome text of a publication with `decision`.
    fn published(decision: &str) -> String {
        format!(
            "{{\"outcome\":\"published\",\"publication\":{{\"decision\":\"{decision}\",\
             \"github_review_id\":1,\"review_url\":\"https://example.invalid/r/1\",\
             \"reused\":false}}}}"
        )
    }

    /// The counters of fixed rows: every family, two keys, label escaping, cumulative buckets.
    #[test]
    fn metrics_exposition_writes_every_family_of_fixed_counts() {
        let start = "2026-11-01T00:00:00Z";
        let widget = |id: &str| review(id, "a/w", "*", start);
        let odd = |id: &str| review(id, "a/o", ODD_RULE, start);
        let mut counts = Counts::default();
        for started in [widget("w1"), widget("w2"), odd("o1")] {
            counts.start(&started);
        }
        let (success, failure) = ("success", "failure");
        let mut waited = widget("w2");
        waited.approval = "approve".to_owned();
        let completed = [
            // 300 seconds, a bucket bound: in every bucket from 300 seconds on.
            done(
                widget("w1"),
                "2026-11-01T00:05:00Z",
                success,
                "",
                &published("APPROVE"),
            ),
            // Two hours after an approval wait: in every bucket from 14400 seconds on.
            done(
                waited,
                "2026-11-01T02:00:00Z",
                success,
                "",
                &published("APPROVE"),
            ),
            done(widget("w3"), start, failure, "github_request_failed", ""),
            done(widget("w4"), start, failure, "github_request_failed", ""),
            done(widget("w5"), start, failure, "head_changed", ""),
            // Completed before it started: published, outside the histogram.
            done(
                odd("o1"),
                "2026-10-31T23:00:00Z",
                success,
                "",
                &published("COMMENT"),
            ),
            done(
                odd("o2"),
                start,
                success,
                "",
                "{\"outcome\":\"approval_expired\"}",
            ),
        ];
        for review in &completed {
            counts.complete(review).expect("the row decodes");
        }
        let broken = done(review("b1", "a/b", "*", start), start, success, "", "[");
        assert!(
            counts.complete(&broken).is_err(),
            "an outcome that does not decode"
        );

        let mut running = widget("r1");
        running.status = "running".to_owned();
        running.input.route.host = "host-b".to_owned();
        let mut awaiting = widget("a1");
        awaiting.status = "awaiting".to_owned();
        let mut decided = widget("a2");
        decided.status = "awaiting".to_owned();
        decided.approval = "approve".to_owned();
        let rows = [widget("q1"), widget("q2"), running, awaiting, decided];
        let schedule: BTreeMap<String, Record> = [
            ("a/w#1", "failed"),
            ("a/w#2", "failed"),
            ("a/g#3", "failed"),
            ("a/g#4", "retry_wait"),
        ]
        .into_iter()
        .map(|(key, phase)| {
            let record = Record {
                phase: phase.to_owned(),
                ..Record::default()
            };
            (key.to_owned(), record)
        })
        .collect();
        let gauges = Gauges::read(&rows, &schedule).expect("the keys have a number");
        assert_eq!(render(&gauges, &counts), EXPECTED.trim_start());
    }
}
