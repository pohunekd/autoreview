//! The Prometheus text exposition, format 0.0.4, of the gauges, the review and step counters.

// Rust guideline compliant 2026-06-26

use super::counts::{Counts, Gauges, PUBLICATION_BUCKETS, ReviewCounts, STEP_BUCKETS};
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
    steps(&mut out, counts);
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

/// Writes the counters and the duration histogram of the recorded step runs.
fn steps(out: &mut String, counts: &Counts) {
    let name = "autoreview_review_steps_total";
    let help = "Recorded runs of the review steps by outcome.";
    header(out, name, "counter", help);
    for (step, totals) in &counts.steps {
        for (outcome, runs) in &totals.runs {
            sample(out, name, &[("step", step), ("outcome", outcome)], runs);
        }
    }
    let name = "autoreview_review_step_retries_total";
    let help = "Repeated attempts inside the recorded step runs.";
    header(out, name, "counter", help);
    for (step, totals) in &counts.steps {
        sample(out, name, &[("step", step)], totals.retries);
    }
    let name = "autoreview_review_step_seconds";
    let help = "Seconds of the succeeded runs of the review steps.";
    header(out, name, "histogram", help);
    let (bucket, sum, count) = (
        format!("{name}_bucket"),
        format!("{name}_sum"),
        format!("{name}_count"),
    );
    for (step, totals) in &counts.steps {
        for (bound, hits) in STEP_BUCKETS.iter().zip(totals.buckets) {
            let le = bound.to_string();
            sample(out, &bucket, &[("step", step), ("le", &le)], hits);
        }
        let measured = totals.measured;
        sample(out, &bucket, &[("step", step), ("le", "+Inf")], measured);
        // Milliseconds as seconds with three decimals, exact for any sum.
        let total = totals.milliseconds_sum;
        let seconds = format!("{}.{:03}", total / 1000, total % 1000);
        sample(out, &sum, &[("step", step)], seconds);
        sample(out, &count, &[("step", step)], measured);
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
