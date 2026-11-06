//! Renders a review as a GitHub review and matches the human review signals.

// Rust guideline compliant 2026-06-26

use autoreview_policy::{HumanReview, OnFindings, Reviewer, match_glob};

use super::neutralize::{closes_fence, neutralize, opening_fence};
use super::plan::{PublicationPlan, blocking_count};
use super::review_marker;
use crate::error::Error;
use crate::integrations::{GitHubReview, InlineComment};
use crate::messages::excerpt;
use crate::model::review::{PRIOR_ANSWERED, PRIOR_FIXED, PRIOR_OPEN};
use crate::model::{Artifact, Finding, PriorResult, ReviewInput};
use crate::verify::{demoted, verification_line};

/// Configured values that shape the rendered review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RenderSettings {
    /// Signature that ends the review body and every inline comment.
    pub(crate) ai_signature: String,
    /// Largest body and largest inline comment, in bytes, that GitHub accepts.
    pub(crate) github_body_max_bytes: i64,
    /// Character limit of one excerpted human review signal.
    pub(crate) excerpt_characters: i64,
}

/// Separator of the reviewer labels in a review and in a merged finding.
pub(crate) const LABEL_SEPARATOR: &str = " + ";

const NO_NEW_BLOCKING: &str = "No new blocking findings were identified.";
const NO_BLOCKING: &str = "No blocking findings were identified.";
const NOTES_TO_AUTHOR: &str = "No blocking findings were identified, only the non-blocking notes \
     below. Whether to address them is up to the author. Request this review again once you have \
     decided; the pull request then goes to the reviewer for approval.";
const OWN_PULL_REQUEST: &str = "GitHub does not let the reviewing account approve or request \
     changes on its own pull request, so this review is a comment.";

/// Renders the GitHub review of a publication.
///
/// Anchored findings become inline comments and everything else goes in the body. The signature
/// closes the body and every comment, and each block of model text has its fences closed.
pub(crate) fn render_review(
    settings: &RenderSettings,
    input: &ReviewInput,
    artifact: &Artifact,
    plan: &PublicationPlan,
) -> Result<GitHubReview, Error> {
    let pull_request = &input.pull_request;
    let (blocking, notes) = input.split_findings(&artifact.findings);
    let open = artifact.open_prior();
    let marker = review_marker(pull_request, &plan.decision, &blocking, &open);
    let mut sections = vec![
        marker.clone(),
        "## Automated code review".to_owned(),
        format!(
            "Reviewed by {}, {} review of `{}`.",
            reviewer_labels(&input.reviewers),
            artifact.scope.mode,
            head_prefix(&pull_request.head_sha)
        ),
    ];
    if artifact.scope.incremental() {
        sections.push(format!(
            "This re-review covers the changes since the previously reviewed commit `{}`.",
            head_prefix(&artifact.scope.previous_head_sha)
        ));
    }
    for summary in &artifact.summaries {
        sections.push(format!(
            "**{}:** {}",
            neutralize(&summary.label),
            model_text(&summary.summary)
        ));
    }
    let verification = verification_line(artifact.verification.as_ref());
    if !verification.is_empty() {
        sections.push(neutralize(&verification));
    }
    if !artifact.human_review.is_empty() {
        sections.push(neutralize(&format!(
            "This review waited for a human approval because the pull request matched: {}.",
            human_review_text(&artifact.human_review, settings.excerpt_characters)
        )));
    }
    let (comments, unanchored) = blocking_comments(settings, &blocking);
    sections.extend(blocking_sections(
        blocking.len(),
        open.len(),
        comments.len(),
        unanchored,
        plan.notes_to_author,
    ));
    if !artifact.prior_results.is_empty() {
        sections.push(render_prior(&artifact.prior_results));
    }
    if !notes.is_empty() {
        sections.push(render_notes(&notes));
    }
    if !artifact.tests.is_empty() {
        sections.push(validation_section(&artifact.tests));
    }
    if own_pull_request_notice(input, artifact, plan) {
        sections.push(OWN_PULL_REQUEST.to_owned());
    }
    sections.push(settings.ai_signature.clone());
    let body = sections.join("\n\n");
    let max = settings.github_body_max_bytes;
    if too_large(&body, max) || comments.iter().any(|comment| too_large(&comment.body, max)) {
        return Err(Error::workflow("github_review_too_large", true, None));
    }
    Ok(GitHubReview {
        decision: plan.decision.clone(),
        body,
        comments,
        marker,
    })
}

/// Splits the blocking findings into inline comments and the body lines of unanchored ones.
fn blocking_comments(
    settings: &RenderSettings,
    blocking: &[Finding],
) -> (Vec<InlineComment>, Vec<String>) {
    let mut comments = Vec::new();
    let mut unanchored = Vec::new();
    for finding in blocking {
        let heading = finding_heading(finding);
        if finding.anchored {
            comments.push(InlineComment {
                path: finding.path.clone(),
                line: finding.line,
                side: "RIGHT".to_owned(),
                body: format!(
                    "{heading}\n\n{}\n\n{}",
                    model_text(&finding.body),
                    settings.ai_signature
                ),
            });
        } else {
            unanchored.push(format!(
                "{heading}\n`{}:{}`\n\n{}",
                neutralize(&finding.path),
                finding.line,
                model_text(&finding.body)
            ));
        }
    }
    (comments, unanchored)
}

/// Returns the body sections that state the blocking findings and their inline comments.
fn blocking_sections(
    blocking: usize,
    open: usize,
    comments: usize,
    unanchored: Vec<String>,
    notes_to_author: bool,
) -> Vec<String> {
    if blocking == 0 && open > 0 {
        vec![NO_NEW_BLOCKING.to_owned()]
    } else if blocking == 0 && notes_to_author {
        vec![NOTES_TO_AUTHOR.to_owned()]
    } else if blocking == 0 {
        vec![NO_BLOCKING.to_owned()]
    } else if unanchored.is_empty() {
        vec![format!(
            "{comments} blocking finding(s) are posted as inline comments."
        )]
    } else {
        let mut sections = vec![format!(
            "{comments} blocking finding(s) are posted as inline comments. These findings concern \
             code outside the changed lines:"
        )];
        sections.extend(unanchored);
        sections
    }
}

/// Reports whether the reviewing account's own pull request gets a comment instead of its outcome.
fn own_pull_request_notice(
    input: &ReviewInput,
    artifact: &Artifact,
    plan: &PublicationPlan,
) -> bool {
    if !plan.own_pull_request {
        return false;
    }
    let total = blocking_count(input, artifact);
    (total > 0 && input.rule.on_findings == OnFindings::RequestChanges)
        || (total == 0 && input.rule.approves_clean())
}

/// Lists the validation the reviewers performed in a collapsed section, each test neutralized.
fn validation_section(tests: &[String]) -> String {
    let lines: Vec<String> = tests
        .iter()
        .map(|test| format!("- {}", neutralize(test)))
        .collect();
    format!(
        "<details>\n<summary>Validation performed: {} check(s)</summary>\n\n{}\n\n</details>",
        tests.len(),
        lines.join("\n")
    )
}

/// Reports whether a text exceeds a GitHub limit. A length that does not fit `i64` is too large.
fn too_large(text: &str, max: i64) -> bool {
    !i64::try_from(text.len()).is_ok_and(|len| len <= max)
}

/// Heads a finding in the body and in its inline comment.
fn finding_heading(finding: &Finding) -> String {
    format!(
        "**{} {} — {}** (confidence: {}, {})",
        finding.id,
        finding.severity.to_ascii_uppercase(),
        neutralize(&finding.title),
        finding.confidence,
        neutralize(&finding.reviewer_label)
    )
}

/// Lists the non-blocking notes in a collapsed section that opens no inline thread.
///
/// A note shows its heading and location. Only a note the verifier demoted keeps its body,
/// which carries the reason.
fn render_notes(notes: &[Finding]) -> String {
    let items: Vec<String> = notes
        .iter()
        .map(|note| {
            let location = format!(
                "{}\n`{}:{}`",
                finding_heading(note),
                neutralize(&note.path),
                note.line
            );
            if demoted(note) {
                format!("{location}\n\n{}", model_text(&note.body))
            } else {
                location
            }
        })
        .collect();
    format!(
        "<details>\n<summary>{} non-blocking note(s): no action required in this pull request\
         </summary>\n\n{}\n\n</details>",
        notes.len(),
        items.join("\n\n")
    )
}

/// Reports the verdict on every prior finding. An open one keeps its thread from the earlier
/// review, so it gets no new inline comment.
fn render_prior(results: &[PriorResult]) -> String {
    let mut lines = vec!["### Findings from earlier reviews".to_owned()];
    for result in results {
        let finding = &result.finding;
        let mut status = match result.verdict.status.as_str() {
            PRIOR_OPEN => "**Still open, blocking**".to_owned(),
            PRIOR_FIXED => "Fixed".to_owned(),
            PRIOR_ANSWERED => "Settled in the discussion".to_owned(),
            _ => String::new(),
        };
        if result.reopened {
            status.push_str(", reopened by the verification");
        }
        if !result.republished.is_empty() {
            status.push_str(", published again as ");
            status.push_str(&result.republished);
        }
        if !result.verdict.relies_on.is_empty() {
            status.push_str(", relying on a comment by ");
            status.push_str(&neutralize(&result.verdict.relies_on));
        }
        lines.push(format!(
            "- {status}: {} {} — {} `{}:{}` (review of `{}`)\n\n{}",
            finding.id,
            finding.severity.to_ascii_uppercase(),
            neutralize(&finding.title),
            neutralize(&finding.path),
            finding.line,
            head_prefix(&finding.head_sha),
            model_text(&result.verdict.note)
        ));
    }
    lines.join("\n\n")
}

/// Renders multi-line model text as a self-contained block.
fn model_text(value: &str) -> String {
    close_open_fence(&neutralize(value))
}

/// Appends the closing fence of a block that model text leaves open, so the block cannot
/// swallow the markup that follows it.
fn close_open_fence(value: &str) -> String {
    let mut fence = String::new();
    for line in value.split('\n') {
        let indent = line.len() - line.trim_start_matches(' ').len();
        if indent > 3 {
            continue;
        }
        let trimmed = &line[indent..];
        if !fence.is_empty() {
            if closes_fence(trimmed, &fence) {
                fence.clear();
            }
            continue;
        }
        fence = opening_fence(trimmed).unwrap_or_default().to_owned();
    }
    if fence.is_empty() {
        value.to_owned()
    } else {
        format!("{value}\n{fence}")
    }
}

/// Returns the first twelve characters of a commit SHA, as Go's `%.12s` prints them.
pub(crate) fn head_prefix(sha: &str) -> String {
    sha.chars().take(12).collect()
}

/// Joins the reviewer labels of a review.
pub(crate) fn reviewer_labels(reviewers: &[Reviewer]) -> String {
    let labels: Vec<&str> = reviewers
        .iter()
        .map(|reviewer| reviewer.label.as_str())
        .collect();
    labels.join(LABEL_SEPARATOR)
}

/// Lists the signals of a human review that the pull request matches: each configured label it
/// carries, and the deleted files that match a test glob.
pub(crate) fn human_review_signals(
    signals: &HumanReview,
    labels: &[String],
    deleted: &[String],
) -> Vec<String> {
    let mut matched = Vec::new();
    for label in &signals.labels {
        if labels
            .iter()
            .any(|carried| labels_equal_fold(carried, label))
        {
            matched.push(format!("label {label}"));
        }
    }
    let mut tests: Vec<String> = deleted
        .iter()
        .filter(|file| {
            signals
                .deleted_tests
                .iter()
                .any(|glob| match_glob(glob, file))
        })
        .cloned()
        .collect();
    if !tests.is_empty() {
        tests.sort();
        matched.push(format!("deleted test files {}", tests.join(", ")));
    }
    matched
}

/// Joins the matched signals, each cut to the excerpt limit.
pub(crate) fn human_review_text(signals: &[String], limit: i64) -> String {
    let parts: Vec<String> = signals
        .iter()
        .map(|signal| excerpt(signal, limit))
        .collect();
    parts.join("; ")
}

/// Compares a carried label with a configured label as Go's `strings.EqualFold` does.
///
/// The configured label is ASCII (policy `labelPattern`). Among the non-ASCII characters only the
/// Kelvin sign and the long s fold to an ASCII letter in Go, so only those two are mapped.
fn labels_equal_fold(carried: &str, label: &str) -> bool {
    let carried: Vec<char> = carried.chars().collect();
    let label: Vec<char> = label.chars().collect();
    carried.len() == label.len()
        && carried
            .iter()
            .zip(&label)
            .all(|(left, right)| fold_ascii(*left) == fold_ascii(*right))
}

/// Maps one character to the ASCII letter it folds to, or lowercases it when it is ASCII.
fn fold_ascii(character: char) -> char {
    match character {
        '\u{212A}' => 'k',
        '\u{017F}' => 's',
        other => other.to_ascii_lowercase(),
    }
}

#[cfg(test)]
mod tests {
    use autoreview_policy::{
        Checkout, HumanReview, OnClean, OnFindings, OnNotes, Pipeline, Protocol, Reviewer, Rule,
        Trigger, TriggerKind,
    };
    use serde::Deserialize;

    use super::{RenderSettings, human_review_signals, render_review};
    use crate::error::error_code;
    use crate::integrations::GitHubReview;
    use crate::messages::{APPROVAL_REQUESTED_TITLE, approval_requested_message};
    use crate::model::ids::INPUT_VERSION;
    use crate::model::review::{PRIOR_ANSWERED, PRIOR_OPEN, prior_key};
    use crate::model::{
        Artifact, Decision, Finding, PriorFinding, PriorResult, PriorVerdict, PullRequest,
        ReviewInput, ReviewScope,
    };
    use crate::publish::plan::{PublicationPlan, plan_publication};

    /// Go-written render cases: the input and the GitHub review or the error code.
    const GOLDEN: &str = include_str!("../../../../testdata/parity/render.json");

    /// Golden case whose note body the Rust review leaves out (#175).
    const NOTE_BODY_CASE: &str = "notes_handed_to_author";

    /// Note body of `NOTE_BODY_CASE` in the Go review.
    ///
    /// The Rust review leaves it out and renders the rest of the case unchanged.
    const GO_NOTE_BODY: &str = "\n\nRename it:\n```go\nfunc better() {\n```";

    /// One golden case: a named input, and either the Go review or the Go error code.
    #[derive(Debug, Deserialize)]
    struct Case {
        name: String,
        input: Fixture,
        #[serde(default)]
        output: Option<Output>,
        #[serde(default)]
        error: Option<String>,
    }

    /// Input of a golden case, as Go marshals its arguments.
    #[derive(Debug, Deserialize)]
    struct Fixture {
        config: GoConfig,
        input: ReviewInput,
        artifact: Artifact,
        plan: PublicationPlan,
    }

    /// The configuration fields a render reads, under their Go names.
    #[derive(Debug, Deserialize)]
    struct GoConfig {
        #[serde(rename = "AISignature")]
        ai_signature: String,
        #[serde(rename = "GitHubBodyMaxBytes")]
        github_body_max_bytes: i64,
        #[serde(rename = "ExcerptCharacters")]
        excerpt_characters: i64,
    }

    /// The Go review of a golden case.
    #[derive(Debug, Deserialize)]
    struct Output {
        decision: Decision,
        body: String,
        comments: Vec<GoComment>,
        marker: String,
    }

    /// One inline comment of a golden review.
    #[derive(Debug, PartialEq, Eq, Deserialize)]
    struct GoComment {
        path: String,
        line: i64,
        side: String,
        body: String,
    }

    fn sha(character: char) -> String {
        character.to_string().repeat(40)
    }

    fn opus() -> Reviewer {
        Reviewer {
            id: "opus".to_owned(),
            label: "Opus".to_owned(),
            protocol: Protocol::ClaudeStructured,
            agent_profile: "claude-profile".to_owned(),
            finding_prefix: "O".to_owned(),
        }
    }

    fn test_rule(on_findings: OnFindings, on_clean: OnClean) -> Rule {
        Rule {
            match_glob: "acme/*".to_owned(),
            trigger: Trigger {
                kind: TriggerKind::ReviewRequested,
                label: String::new(),
            },
            on_findings,
            on_clean,
            pipeline: Pipeline {
                name: "main".to_owned(),
                protocol: Protocol::ClaudeStructured,
                reviewers: vec![opus()],
                incremental_reviewers: vec![opus()],
                prompt: "Review the pull request.".to_owned(),
                checkout: Checkout::Base,
                project_sources: vec!["manual".to_owned()],
                verifier: None,
            },
            ..Rule::default()
        }
    }

    fn test_pull_request(number: i64) -> PullRequest {
        PullRequest {
            repository: "acme/widget".to_owned(),
            number,
            head_sha: sha('a'),
            review_request_id: 7,
            title: "Add widget".to_owned(),
            url: format!("https://example.invalid/acme/widget/pull/{number}"),
            author: "colleague".to_owned(),
            head_branch: "feature".to_owned(),
            base_branch: "main".to_owned(),
            base_sha: sha('b'),
            ..PullRequest::default()
        }
    }

    fn test_input(rule: Rule, pull_request: PullRequest) -> ReviewInput {
        ReviewInput {
            version: INPUT_VERSION,
            reviewers: rule.pipeline.reviewers.clone(),
            pull_request,
            rule,
            ..ReviewInput::default()
        }
    }

    fn test_settings() -> RenderSettings {
        RenderSettings {
            ai_signature: "🤖 Written by AI: test reviewer".to_owned(),
            github_body_max_bytes: 60_000,
            excerpt_characters: 200,
        }
    }

    fn full_scope() -> ReviewScope {
        ReviewScope {
            mode: "full".to_owned(),
            ..ReviewScope::default()
        }
    }

    /// Asserts that the last line of a text, ignoring blank and HTML comment lines, is the
    /// signature outside any code block.
    fn assert_signed_on_last_line(text: &str) {
        let lines: Vec<&str> = text.split('\n').collect();
        let mut last = lines.len();
        while last > 0 {
            let trimmed = lines[last - 1].trim();
            if !(trimmed.is_empty() || trimmed.starts_with("<!--")) {
                break;
            }
            last -= 1;
        }
        assert!(
            last > 0
                && lines[last - 1]
                    .trim_start_matches(['🤖', '\u{FE0F}', ' '])
                    .starts_with("Written by AI:"),
            "not signed on its last line:\n{text}"
        );
        let head = lines[..last - 1].join("\n");
        assert_eq!(
            super::close_open_fence(&head),
            head,
            "the signature is inside an unclosed code block:\n{text}"
        );
    }

    #[test]
    fn render_review_keeps_code_intact_and_defuses_mentions_and_comments() {
        let rule = test_rule(OnFindings::RequestChanges, OnClean::Approve);
        let input = test_input(rule, test_pull_request(5));
        let body = concat!(
            "Call `$client->send(@payload)` here:\n```php\n",
            "$map = ['a' => $b->c()]; // @param int $x\n```\n",
            "Thanks @octocat, mail me at dev@example.com. <!-- hidden --> done"
        );
        let artifact = Artifact {
            repository: "acme/widget".to_owned(),
            number: 5,
            head_sha: sha('a'),
            scope: full_scope(),
            summaries: vec![crate::model::ReviewerSummary {
                reviewer_id: "opus".to_owned(),
                label: "Opus".to_owned(),
                summary: "Summary mentions @someone.".to_owned(),
            }],
            findings: vec![
                Finding {
                    id: "O001".to_owned(),
                    reviewer_label: "Opus".to_owned(),
                    severity: "high".to_owned(),
                    confidence: "high".to_owned(),
                    path: "src/a.go".to_owned(),
                    line: 3,
                    title: "Inline".to_owned(),
                    body: body.to_owned(),
                    anchored: true,
                    ..Finding::default()
                },
                Finding {
                    id: "O002".to_owned(),
                    reviewer_label: "Opus".to_owned(),
                    severity: "low".to_owned(),
                    confidence: "medium".to_owned(),
                    path: "src/b.go".to_owned(),
                    line: 9,
                    title: "Elsewhere".to_owned(),
                    body: "Outside the diff.".to_owned(),
                    ..Finding::default()
                },
            ],
            ..Artifact::default()
        };
        let settings = test_settings();
        let plan = PublicationPlan {
            decision: Decision::RequestChanges,
            ..PublicationPlan::default()
        };
        let review = render_review(&settings, &input, &artifact, &plan).expect("review renders");
        assert!(
            review.comments.len() == 1
                && review.comments[0].path == "src/a.go"
                && review.comments[0].line == 3
                && review.comments[0].side == "RIGHT",
            "comments = {:?}",
            review.comments
        );
        let comment = &review.comments[0].body;
        assert!(
            comment.ends_with(&format!("\n\n{}", settings.ai_signature)),
            "inline comment must end with the AI signature:\n{comment}"
        );
        assert!(
            review
                .body
                .ends_with(&format!("\n\n{}", settings.ai_signature)),
            "review body must end with the AI signature:\n{}",
            review.body
        );
        for kept in [
            "`$client->send(@payload)`",
            "$map = ['a' => $b->c()]; // @param int $x",
            "dev@example.com",
        ] {
            assert!(
                comment.contains(kept),
                "code or plain text was altered, missing {kept:?} in:\n{comment}"
            );
        }
        assert!(
            !comment.contains("Thanks @octocat") && comment.contains("Thanks @\u{200b}octocat"),
            "mention outside code was not defused:\n{comment}"
        );
        assert!(
            !comment.contains("<!--"),
            "an HTML comment survived:\n{comment}"
        );
        assert!(
            review.body.starts_with(&review.marker)
                && review.body.matches("<!--").count() == 1
                && review.body.contains("`src/b.go:9`")
                && !review.body.contains("@someone"),
            "body = {}",
            review.body
        );
    }

    #[test]
    fn every_published_body_ends_with_the_signature_outside_model_code() {
        let input = test_input(
            test_rule(OnFindings::RequestChanges, OnClean::Await),
            test_pull_request(5),
        );
        let unclosed = "Use this:\n```go\nfunc open() {";
        let summary = vec![crate::model::ReviewerSummary {
            reviewer_id: "opus".to_owned(),
            label: "Opus".to_owned(),
            summary: "Reviewed the change.".to_owned(),
        }];
        let finding = Finding {
            id: "O001".to_owned(),
            reviewer_label: "Opus".to_owned(),
            severity: "high".to_owned(),
            confidence: "high".to_owned(),
            path: "src/a.go".to_owned(),
            line: 3,
            title: "Bug".to_owned(),
            body: unclosed.to_owned(),
            ..Finding::default()
        };
        let anchored = Finding {
            anchored: true,
            ..finding.clone()
        };
        let note = Finding {
            anchored: true,
            non_blocking: true,
            ..finding.clone()
        };
        // Each source of model text on its own, so no later block closes an earlier one by chance.
        let cases = vec![
            (
                "summary",
                Artifact {
                    summaries: vec![crate::model::ReviewerSummary {
                        reviewer_id: "opus".to_owned(),
                        label: "Opus".to_owned(),
                        summary: unclosed.to_owned(),
                    }],
                    ..Artifact::default()
                },
            ),
            (
                "inline finding",
                Artifact {
                    summaries: summary.clone(),
                    findings: vec![anchored],
                    ..Artifact::default()
                },
            ),
            (
                "unanchored finding",
                Artifact {
                    summaries: summary.clone(),
                    findings: vec![finding],
                    ..Artifact::default()
                },
            ),
            (
                "note",
                Artifact {
                    summaries: summary,
                    findings: vec![note],
                    ..Artifact::default()
                },
            ),
        ];
        let settings = test_settings();
        for (name, mut artifact) in cases {
            artifact.repository = "acme/widget".to_owned();
            artifact.number = 5;
            artifact.head_sha = sha('a');
            artifact.scope = full_scope();
            let plan = PublicationPlan {
                decision: Decision::RequestChanges,
                ..PublicationPlan::default()
            };
            let review = render_review(&settings, &input, &artifact, &plan)
                .unwrap_or_else(|err| panic!("{name}: {err}"));
            assert_signed_on_last_line(&review.body);
            for comment in &review.comments {
                assert_signed_on_last_line(&comment.body);
            }
        }
    }

    #[test]
    fn notes_are_collapsed_without_inline_comments_or_count() {
        let input = test_input(
            test_rule(OnFindings::RequestChanges, OnClean::Await),
            test_pull_request(5),
        );
        let artifact = Artifact {
            repository: "acme/widget".to_owned(),
            number: 5,
            head_sha: sha('a'),
            scope: full_scope(),
            summaries: vec![crate::model::ReviewerSummary {
                reviewer_id: "opus".to_owned(),
                label: "Opus".to_owned(),
                summary: "Reviewed the change.".to_owned(),
            }],
            findings: vec![
                Finding {
                    id: "O001".to_owned(),
                    reviewer_label: "Opus".to_owned(),
                    severity: "high".to_owned(),
                    confidence: "high".to_owned(),
                    path: "src/a.go".to_owned(),
                    line: 3,
                    title: "Bug".to_owned(),
                    body: "Breaks.".to_owned(),
                    anchored: true,
                    ..Finding::default()
                },
                Finding {
                    id: "O002".to_owned(),
                    reviewer_label: "Opus".to_owned(),
                    severity: "low".to_owned(),
                    confidence: "high".to_owned(),
                    path: "src/a.go".to_owned(),
                    line: 4,
                    title: "Naming".to_owned(),
                    body: "Rename it:\n```go\nfunc better() {".to_owned(),
                    anchored: true,
                    non_blocking: true,
                    ..Finding::default()
                },
            ],
            ..Artifact::default()
        };
        let plan = PublicationPlan {
            decision: Decision::RequestChanges,
            ..PublicationPlan::default()
        };
        let review =
            render_review(&test_settings(), &input, &artifact, &plan).expect("review renders");
        assert!(
            review.comments.len() == 1 && review.comments[0].line == 3,
            "a note must not open an inline thread: {:?}",
            review.comments
        );
        assert!(
            review.marker.contains(":findings=1:"),
            "the marker must count blocking findings only: {}",
            review.marker
        );
        let start = review.body.find("<details>").unwrap_or_default();
        let notes = &review.body[start..];
        assert!(
            notes.contains("1 non-blocking note(s): no action required in this pull request")
                && notes.contains("O002"),
            "notes section = {notes}"
        );
        let location_only = concat!(
            "**O002 LOW — Naming** (confidence: high, Opus)\n",
            "`src/a.go:4`\n\n</details>"
        );
        assert!(
            notes.contains(location_only) && !notes.contains("Rename it"),
            "a note must show its heading and location only:\n{notes}"
        );
        let message = approval_requested_message(
            200,
            &input,
            &Artifact {
                findings: artifact.findings.iter().skip(1).cloned().collect(),
                ..Artifact::default()
            },
        );
        let reply = message.find("Reply to this message with: approve");
        let notes_at = message.find("Notes:");
        assert!(
            message.starts_with(APPROVAL_REQUESTED_TITLE)
                && message.contains("Blocking findings: 0\nNon-blocking notes: 1")
                && message.contains("O002 [NOTE, LOW")
                && reply.is_some()
                && reply <= notes_at,
            "approval request = {message}"
        );
    }

    #[test]
    fn human_review_signals_name_matched_labels_and_deleted_test_files() {
        let signals = HumanReview {
            labels: vec!["needs-human".to_owned(), "risky".to_owned()],
            deleted_tests: vec!["tests/**".to_owned(), "**/tests/**".to_owned()],
        };
        let strings = |values: &[&str]| values.iter().map(|v| (*v).to_owned()).collect::<Vec<_>>();
        let cases = [
            (
                "nothing matches",
                strings(&["bug"]),
                strings(&["src/a.go", "testsuite/a.go"]),
                strings(&[]),
            ),
            (
                "label case is ignored",
                strings(&["NEEDS-HUMAN"]),
                strings(&[]),
                strings(&["label needs-human"]),
            ),
            (
                "deleted tests are sorted",
                strings(&[]),
                strings(&["pkg/tests/b.php", "tests/a.php", "src/c.go"]),
                strings(&["deleted test files pkg/tests/b.php, tests/a.php"]),
            ),
            (
                "both",
                strings(&["risky"]),
                strings(&["tests/a.php"]),
                strings(&["label risky", "deleted test files tests/a.php"]),
            ),
        ];
        for (name, labels, deleted, want) in cases {
            let got = human_review_signals(&signals, &labels, &deleted);
            assert_eq!(got, want, "{name}");
        }
    }

    #[test]
    fn human_review_signal_makes_a_clean_approval_wait() {
        let pull_request = PullRequest {
            repository: "acme/widget".to_owned(),
            number: 7,
            head_sha: sha('a'),
            author: "author".to_owned(),
            ..PullRequest::default()
        };
        let input = test_input(
            test_rule(OnFindings::RequestChanges, OnClean::Approve),
            pull_request,
        );
        let plan = plan_publication(&input, &Artifact::default(), "reviewer");
        assert!(
            !plan.await_approval,
            "a clean review without signals must approve without the user"
        );
        let signalled = Artifact {
            human_review: vec!["label needs-human".to_owned()],
            ..Artifact::default()
        };
        let plan = plan_publication(&input, &signalled, "reviewer");
        assert!(
            plan.decision == Decision::Approve && plan.await_approval,
            "a matched signal must wait for the user, plan = {plan:?}"
        );
    }

    #[test]
    fn open_prior_finding_blocks_and_is_counted_in_the_marker() {
        let input = test_input(
            test_rule(OnFindings::RequestChanges, OnClean::Await),
            test_pull_request(5),
        );
        let open = PriorResult {
            finding: PriorFinding {
                key: prior_key(&sha('c'), "O001"),
                id: "O001".to_owned(),
                head_sha: sha('c'),
                severity: "high".to_owned(),
                path: "src/a.go".to_owned(),
                line: 3,
                title: "Bug".to_owned(),
                ..PriorFinding::default()
            },
            verdict: PriorVerdict {
                status: PRIOR_OPEN.to_owned(),
                note: "Still there.".to_owned(),
                ..PriorVerdict::default()
            },
            ..PriorResult::default()
        };
        let answered = PriorResult {
            finding: PriorFinding {
                key: prior_key(&sha('c'), "S001"),
                id: "S001".to_owned(),
                head_sha: sha('c'),
                severity: "high".to_owned(),
                path: "src/b.go".to_owned(),
                line: 9,
                title: "Name".to_owned(),
                ..PriorFinding::default()
            },
            verdict: PriorVerdict {
                status: PRIOR_ANSWERED.to_owned(),
                note: "Waived by the reviewer.".to_owned(),
                relies_on: "reviewer".to_owned(),
                ..PriorVerdict::default()
            },
            ..PriorResult::default()
        };
        let mut artifact = Artifact {
            repository: "acme/widget".to_owned(),
            number: 5,
            head_sha: sha('a'),
            scope: ReviewScope {
                mode: "incremental".to_owned(),
                previous_head_sha: sha('c'),
            },
            summaries: vec![crate::model::ReviewerSummary {
                reviewer_id: "opus".to_owned(),
                label: "Opus".to_owned(),
                summary: "Reviewed the change.".to_owned(),
            }],
            findings: Vec::new(),
            prior_results: vec![open, answered.clone()],
            ..Artifact::default()
        };
        let plan = plan_publication(&input, &artifact, "reviewer");
        assert_eq!(
            plan.decision,
            Decision::RequestChanges,
            "an open prior finding must keep asking for changes: {plan:?}"
        );
        let review =
            render_review(&test_settings(), &input, &artifact, &plan).expect("review renders");
        assert!(
            review.comments.is_empty()
                && review.marker.contains(":findings=1:")
                && review.body.contains("**Still open, blocking**: O001 HIGH")
                && review.body.contains("relying on a comment by reviewer"),
            "marker={} comments={} body={}",
            review.marker,
            review.comments.len(),
            review.body
        );
        artifact.prior_results = vec![answered];
        let plan = plan_publication(&input, &artifact, "reviewer");
        assert!(
            plan.decision == Decision::Approve && plan.await_approval,
            "settled prior findings must leave the review clean: {plan:?}"
        );
        let message = approval_requested_message(200, &input, &artifact);
        let check = format!(
            "check before approving:\n{} Name (relies on a comment by reviewer)",
            artifact.prior_results[0].finding.key
        );
        assert!(
            message.contains(&check)
                && message.find("Reply to this message with: approve")
                    <= message.find("check before approving"),
            "an answered prior finding must be shown below the reply instruction: {message}"
        );
    }

    #[test]
    fn notes_handed_to_the_author_ask_for_a_renewed_request() {
        let mut rule = test_rule(OnFindings::RequestChanges, OnClean::Await);
        rule.on_notes = OnNotes::Comment;
        let input = test_input(rule, test_pull_request(1));
        let artifact = Artifact {
            findings: vec![Finding {
                id: "O001".to_owned(),
                non_blocking: true,
                severity: "low".to_owned(),
                confidence: "high".to_owned(),
                path: "a.go".to_owned(),
                line: 1,
                title: "Naming".to_owned(),
                body: "Could be clearer.".to_owned(),
                ..Finding::default()
            }],
            ..Artifact::default()
        };
        let plan = plan_publication(&input, &artifact, "reviewer");
        let review: GitHubReview =
            render_review(&test_settings(), &input, &artifact, &plan).expect("review renders");
        assert!(
            review.decision == Decision::Comment
                && review.comments.is_empty()
                && review
                    .body
                    .contains("Request this review again once you have decided")
                && review.body.contains("1 non-blocking note(s)"),
            "decision={:?} comments={} body={}",
            review.decision,
            review.comments.len(),
            review.body
        );
    }

    #[test]
    fn render_matches_go_golden() {
        let cases: Vec<Case> = serde_json::from_str(GOLDEN).expect("golden is JSON");
        assert_eq!(cases.len(), 10, "golden has the ten render cases");
        for case in &cases {
            let settings = RenderSettings {
                ai_signature: case.input.config.ai_signature.clone(),
                github_body_max_bytes: case.input.config.github_body_max_bytes,
                excerpt_characters: case.input.config.excerpt_characters,
            };
            let result = render_review(
                &settings,
                &case.input.input,
                &case.input.artifact,
                &case.input.plan,
            );
            match (&case.error, &case.output) {
                (Some(code), None) => {
                    let err = result.expect_err("golden case records an error");
                    assert_eq!(error_code(&err), code, "case {}", case.name);
                }
                (None, Some(want)) => {
                    let review = result.expect("golden case renders");
                    assert_eq!(review.decision, want.decision, "case {}", case.name);
                    let mut body = want.body.clone();
                    if case.name == NOTE_BODY_CASE {
                        assert!(body.contains(GO_NOTE_BODY), "case {}", case.name);
                        body = body.replacen(GO_NOTE_BODY, "", 1);
                    }
                    assert_eq!(review.body, body, "case {}", case.name);
                    assert_eq!(review.marker, want.marker, "case {}", case.name);
                    let comments: Vec<GoComment> = review
                        .comments
                        .iter()
                        .map(|comment| GoComment {
                            path: comment.path.clone(),
                            line: comment.line,
                            side: comment.side.clone(),
                            body: comment.body.clone(),
                        })
                        .collect();
                    assert_eq!(comments, want.comments, "case {}", case.name);
                }
                _ => panic!(
                    "case {} must hold exactly one of output and error",
                    case.name
                ),
            }
        }
    }
}
