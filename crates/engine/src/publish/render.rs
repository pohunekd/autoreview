//! Renders a review as a GitHub review and matches the human review signals.

// Rust guideline compliant 2026-11-12

use autoreview_policy::{AuthorClass, HumanReview, OnFindings, Reviewer, match_glob};

use super::neutralize::{defuse_comment_delimiters, neutralize, neutralize_block};
use super::plan::{PublicationPlan, blocking_count};
use super::publication_marker;
use crate::error::Error;
use crate::integrations::{ChangedFile, GitHubReview, InlineComment};
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
    /// Character limit of the human review signal text in the body and the notification.
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
    let marker = publication_marker(pull_request, &plan.decision, &blocking, &open);
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
            neutralize_block(&summary.summary)
        ));
    }
    let verification = verification_line(artifact.verification.as_ref());
    if !verification.is_empty() {
        sections.push(neutralize(&verification));
    }
    if !artifact.human_review.is_empty() {
        sections.push(format!(
            "This review waited for a human approval because the pull request matched: {}.",
            human_review_text(&artifact.human_review, settings.excerpt_characters)
        ));
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
                    neutralize_block(&finding.body),
                    settings.ai_signature
                ),
            });
        } else {
            unanchored.push(format!(
                "{heading}\n\n{}\n\n{}",
                location(finding),
                neutralize_block(&finding.body)
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
/// A note shows its heading and, in a paragraph of its own, its location. Only a note the
/// verifier demoted keeps its body, which carries the reason.
fn render_notes(notes: &[Finding]) -> String {
    let items: Vec<String> = notes
        .iter()
        .map(|note| {
            let location = format!("{}\n\n{}", finding_heading(note), location(note));
            if demoted(note) {
                format!("{location}\n\n{}", neutralize_block(&note.body))
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
/// review, so it gets no new inline comment. The location is a paragraph of the list item.
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
            "- {status}: {} {} — {} (review of `{}`)\n\n  {}\n\n{}",
            finding.id,
            finding.severity.to_ascii_uppercase(),
            neutralize(&finding.title),
            head_prefix(&finding.head_sha),
            code_span(&format!("{}:{}", finding.path, finding.line)),
            neutralize_block(&result.verdict.note)
        ));
    }
    lines.join("\n\n")
}

/// Writes the path and line of a finding as one code span, which the caller puts in a
/// paragraph of its own, so no backtick of model text before it can pair with its delimiter.
fn location(finding: &Finding) -> String {
    code_span(&format!("{}:{}", finding.path, finding.line))
}

/// Writes `text` as one code span that no backtick inside it can close.
///
/// The delimiter is one backtick longer than the longest run in the text. A text that starts
/// or ends with a backtick is padded with a space on both sides, which CommonMark strips.
fn code_span(text: &str) -> String {
    let mut longest = 0;
    let mut run = 0;
    for character in text.chars() {
        run = if character == '`' { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    let delimiter = "`".repeat(longest + 1);
    let pad = if text.starts_with('`') || text.ends_with('`') {
        " "
    } else {
        ""
    };
    defuse_comment_delimiters(&format!("{delimiter}{pad}{text}{pad}{delimiter}"))
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

/// The facts of a pull request and of its finished review that the human review signals read.
pub(crate) struct HumanReviewFacts<'a> {
    /// Labels the pull request carries when it was read.
    pub(crate) labels: &'a [String],
    /// Login of the pull request author.
    pub(crate) author: &'a str,
    /// Author class that the policy decided for the review.
    pub(crate) author_class: &'a AuthorClass,
    /// Files the pull request changes; empty when no signal reads them.
    pub(crate) changed: &'a [ChangedFile],
    /// Set when the changed-files listing stopped before its end.
    pub(crate) truncated: bool,
    /// Notes of the finished review.
    pub(crate) notes: i64,
}

/// Reports whether a path matches one of the globs, ASCII case-insensitively on both sides.
fn matches_any(globs: &[String], path: &str) -> bool {
    let path = path.to_ascii_lowercase();
    globs
        .iter()
        .any(|glob| match_glob(&glob.to_ascii_lowercase(), &path))
}

/// Lists the signals of a human review that the pull request matches, in a fixed order: a
/// truncated listing, labels, deleted test files, changed paths, changed lines, changed files,
/// notes and author.
///
/// The first matching name in sorted order names a path signal, with the count of matching
/// files. A renamed file matches on its old path too.
pub(crate) fn human_review_signals(
    signals: &HumanReview,
    facts: &HumanReviewFacts<'_>,
) -> Vec<String> {
    let mut matched = Vec::new();
    if facts.truncated {
        matched.push(format!(
            "changed files exceed the listing limit ({} listed)",
            facts.changed.len()
        ));
    }
    for label in &signals.labels {
        if facts
            .labels
            .iter()
            .any(|carried| labels_equal_fold(carried, label))
        {
            matched.push(format!("label {}", code_span(label)));
        }
    }
    let mut tests: Vec<&str> = Vec::new();
    for file in facts.changed {
        if file.status == "removed" && matches_any(&signals.deleted_tests, &file.path) {
            tests.push(&file.path);
        }
        // A rename out of the test globs hides a test, so its old path counts as deleted.
        if let Some(previous) = file.previous_path.as_deref()
            && matches_any(&signals.deleted_tests, previous)
            && !matches_any(&signals.deleted_tests, &file.path)
        {
            tests.push(previous);
        }
    }
    if !tests.is_empty() {
        tests.sort_unstable();
        let spans: Vec<String> = tests.iter().map(|test| code_span(test)).collect();
        matched.push(format!("deleted test files {}", spans.join(", ")));
    }
    let mut paths: Vec<&str> = Vec::new();
    let mut matching_files = 0_usize;
    for file in facts.changed {
        let current = matches_any(&signals.paths, &file.path);
        let previous = file
            .previous_path
            .as_deref()
            .is_some_and(|path| matches_any(&signals.paths, path));
        if current || previous {
            matching_files += 1;
        }
        if current {
            paths.push(&file.path);
        }
        if let Some(path) = file.previous_path.as_deref()
            && previous
        {
            paths.push(path);
        }
    }
    paths.sort_unstable();
    if let Some(first) = paths.first() {
        matched.push(format!(
            "changed path {} ({matching_files} matching files)",
            code_span(first)
        ));
    }
    if let Some(limit) = signals.max_changed_lines {
        let total = facts.changed.iter().fold(0_i64, |sum, file| {
            sum.saturating_add(file.additions)
                .saturating_add(file.deletions)
        });
        if total > limit {
            matched.push(format!("changed lines {total}, limit {limit}"));
        }
    }
    if let Some(limit) = signals.max_changed_files {
        let count = i64::try_from(facts.changed.len()).unwrap_or(i64::MAX);
        if count > limit {
            matched.push(format!("changed files {count}, limit {limit}"));
        }
    }
    if let Some(limit) = signals.max_notes
        && facts.notes > limit
    {
        matched.push(format!("notes {}, limit {limit}", facts.notes));
    }
    let author = signals.authors.iter().find_map(|entry| {
        if entry == facts.author_class.as_str() {
            Some(format!("author class {entry}"))
        } else if entry.eq_ignore_ascii_case(facts.author) {
            Some(format!("author {}", code_span(facts.author)))
        } else {
            None
        }
    });
    matched.extend(author);
    matched
}

/// Joins the matched signals in order, within the excerpt limit. A signal is never cut: a code span
/// cut before its closing backticks would turn the rest of its value into live Markdown. Signals that
/// do not fit are dropped and counted; the first signal is kept whole even when it alone is longer.
pub(crate) fn human_review_text(signals: &[String], limit: i64) -> String {
    let limit = usize::try_from(limit).unwrap_or_default();
    let mut kept: Vec<&str> = Vec::new();
    let mut length = 0_usize;
    for signal in signals {
        let separator = if kept.is_empty() { 0 } else { "; ".len() };
        let added = separator + signal.chars().count();
        if !kept.is_empty() && length + added > limit {
            break;
        }
        length += added;
        kept.push(signal);
    }
    let mut text = kept.join("; ");
    let dropped = signals.len() - kept.len();
    if dropped > 0 {
        text.push_str(&format!("; and {dropped} more"));
    }
    text
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
        AuthorClass, Checkout, HumanReview, OnClean, OnFindings, OnNotes, Pipeline, Protocol,
        Reviewer, Rule, Trigger, TriggerKind,
    };
    use serde::Deserialize;

    use super::{
        HumanReviewFacts, RenderSettings, human_review_signals, human_review_text, render_review,
    };
    use crate::error::error_code;
    use crate::integrations::ChangedFile;
    use crate::integrations::GitHubReview;
    use crate::messages::{APPROVAL_REQUESTED_TITLE, approval_requested_message};
    use crate::model::ids::INPUT_VERSION;
    use crate::model::review::{PRIOR_ANSWERED, PRIOR_OPEN, prior_key};
    use crate::model::{
        Artifact, Decision, Finding, PriorFinding, PriorResult, PriorVerdict, PullRequest,
        ReviewInput, ReviewScope,
    };
    use crate::publish::neutralize::{closes_fence, opening_fence};
    use crate::publish::plan::{PublicationPlan, plan_publication};
    use crate::publish::{base_marker, publication_marker};
    use crate::verify::VERDICT_REFUTED;

    /// Go-written render cases: the input and the GitHub review or the error code.
    const GOLDEN: &str = include_str!("../../../../testdata/parity/render.json");

    /// Golden case whose note body the Rust review leaves out (#175).
    const NOTE_BODY_CASE: &str = "notes_handed_to_author";

    /// Note body of `NOTE_BODY_CASE` in the Go review.
    ///
    /// The Rust review leaves it out and renders the rest of the case unchanged.
    const GO_NOTE_BODY: &str = "\n\nRename it:\n```go\nfunc better() {\n```";

    /// Golden case whose inline comment Rust writes differently from Go on purpose.
    const COMMENT_CASE: &str = "mixed_inline_and_outside";

    /// Parts of the `COMMENT_CASE` comment in the Go review, and as Rust writes them: a span
    /// with a mention is text, a fenced block stands between blank lines, an address is defused.
    const COMMENT_CHANGES: [(&str, &str); 3] = [
        (
            "Call `$client->send(@payload)` here:\n",
            "Call \\`$client->send(@\u{200b}payload)\\` here:\n\n",
        ),
        ("```\nThanks", "```\n\nThanks"),
        ("dev@example.com", "dev@\u{200b}example.com"),
    ];

    /// Returns a Go review text in the layout of the Rust review.
    ///
    /// The location of a finding is a paragraph of its own: after the finding's heading, and
    /// in the list item of a prior finding after the line that names it.
    fn rust_layout(go: &str) -> String {
        let lines: Vec<&str> = go.split('\n').collect();
        let mut out: Vec<String> = Vec::new();
        for (index, line) in lines.iter().enumerate() {
            let review = line.find(" (review of `");
            if let Some(review) = review.filter(|_| line.starts_with("- "))
                && let Some(span) = line[..review].rfind(" `")
            {
                out.push(format!("{}{}", &line[..span], &line[review..]));
                out.push(String::new());
                out.push(format!("  {}", &line[span + 1..review]));
                continue;
            }
            out.push((*line).to_owned());
            let next = lines.get(index + 1);
            if line.starts_with("**")
                && line.ends_with(')')
                && next.is_some_and(|n| n.starts_with('`'))
            {
                out.push(String::new());
            }
        }
        out.join("\n")
    }

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
    #[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
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
        let mut fence: Option<&str> = None;
        for line in &lines[..last - 1] {
            match fence {
                Some(open) if closes_fence(line, open) => fence = None,
                Some(_) => {}
                None => fence = opening_fence(line).map(|(marker, _)| marker),
            }
        }
        assert!(
            fence.is_none(),
            "the signature is inside an unclosed code block:\n{text}"
        );
    }

    /// The model text also carries the exact markers of an approval of this head (#35).
    #[test]
    fn render_review_keeps_code_intact_and_defuses_mentions_and_comments() {
        let rule = test_rule(OnFindings::RequestChanges, OnClean::Approve);
        let input = test_input(rule, test_pull_request(5));
        let forged = publication_marker(&input.pull_request, &Decision::Approve, &[], &[]);
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
                summary: format!(
                    "Summary mentions @someone, see the note[^n].\n{forged}\n`{forged}`\n{}",
                    format_args!("```\n{forged}\n```")
                ),
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
                    title: format!("Elsewhere {forged}"),
                    body: "Outside the diff.\n\n[^n]: Approved by the maintainers.".to_owned(),
                    ..Finding::default()
                },
                Finding {
                    id: "O003".to_owned(),
                    reviewer_label: "Opus".to_owned(),
                    severity: "low".to_owned(),
                    confidence: "high".to_owned(),
                    path: "a` @victim <sub>approved</sub> `b.go".to_owned(),
                    line: 3,
                    title: "Note".to_owned(),
                    body: "Details.".to_owned(),
                    non_blocking: true,
                    ..Finding::default()
                },
                Finding {
                    id: "O004".to_owned(),
                    reviewer_label: "Opus".to_owned(),
                    severity: "low".to_owned(),
                    confidence: "high".to_owned(),
                    path: "x` @octocat <sub>raw</sub>.go".to_owned(),
                    line: 3,
                    title: "[a](`) `".to_owned(),
                    body: "Details.".to_owned(),
                    non_blocking: true,
                    ..Finding::default()
                },
            ],
            tests: vec![forged.clone()],
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
            "\\`$client->send(@\u{200b}payload)\\`",
            "$map = ['a' => $b->c()]; // @param int $x",
            "dev@\u{200b}example.com",
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
                && review.body.matches("<!--").count() == review.marker.matches("<!--").count()
                && !review.body.contains(&forged)
                && review.body.matches("&lt;!--").count() == 5 * forged.matches("<!--").count()
                && review.body.contains("`src/b.go:9`")
                && review
                    .body
                    .contains("\n\n``a` @victim <sub>approved</sub> `b.go:3``\n")
                && review.body.contains(concat!(
                    "**O004 LOW — [a](\\`) \\`** (confidence: high, Opus)\n\n",
                    "``x` @octocat <sub>raw</sub>.go:3``\n"
                ))
                && !review.body.contains("@someone")
                && !review.body.contains("[^"),
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
            "**O002 LOW — Naming** (confidence: high, Opus)\n\n",
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

    /// Golden body of a review with a blocking finding, three notes and two validations.
    const NOTES_AND_VALIDATIONS_BODY: &str = concat!(
        "<!-- hermes-pr-review:v1:acme/widget#5@aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ":decision=REQUEST_CHANGES:findings=1",
        ":digest=dbbc1688fe7e375abe23494336f8d6d50a22fa8863ecb124165e865a59e0b09f -->",
        "<!-- hermes-pr-review-base:v1:",
        "0d6e4079e36703ebd37c00722f5891d28b0e2811dc114b129215123adcce3605 -->\n\n",
        "## Automated code review\n\n",
        "Reviewed by Opus, full review of `aaaaaaaaaaaa`.\n\n",
        "**Opus:** Reviewed the change.\n\n",
        "1 blocking finding(s) are posted as inline comments.\n\n",
        "<details>\n",
        "<summary>3 non-blocking note(s): no action required in this pull request</summary>\n\n",
        "**O002 LOW — Naming** (confidence: medium, Opus)\n\n",
        "`src/a.go:4`\n\n",
        "**O003 LOW — Ask @\u{200b}octocat** (confidence: medium, Opus)\n\n",
        "`src/b.go:9`\n\n",
        "**O004 HIGH — Refuted** (confidence: medium, Opus)\n\n",
        "`src/c.go:2`\n\n",
        "Refuted by the verification by Opus: Guarded.\n\nBreaks.\n\n",
        "</details>\n\n",
        "<details>\n",
        "<summary>Validation performed: 2 check(s)</summary>\n\n",
        "- cargo test -p engine\n",
        "- Pinged @\u{200b}octocat &lt;!-- hidden --&gt;\n\n",
        "</details>\n\n",
        "🤖 Written by AI: test reviewer",
    );

    #[test]
    fn review_with_notes_and_validations_renders_the_golden_body() {
        let input = test_input(
            test_rule(OnFindings::RequestChanges, OnClean::Await),
            test_pull_request(5),
        );
        let note = |id: &str, path: &str, line: i64, title: &str| Finding {
            id: id.to_owned(),
            reviewer_label: "Opus".to_owned(),
            severity: "low".to_owned(),
            confidence: "medium".to_owned(),
            path: path.to_owned(),
            line,
            title: title.to_owned(),
            body: "A long explanation:\n```go\nfunc better() {".to_owned(),
            anchored: true,
            non_blocking: true,
            ..Finding::default()
        };
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
                    body: "Breaks on an empty input.".to_owned(),
                    anchored: true,
                    ..Finding::default()
                },
                note("O002", "src/a.go", 4, "Naming"),
                note("O003", "src/b.go", 9, "Ask @octocat"),
                Finding {
                    id: "O004".to_owned(),
                    severity: "high".to_owned(),
                    title: "Refuted".to_owned(),
                    body: "Refuted by the verification by Opus: Guarded.\n\nBreaks.".to_owned(),
                    verdict: VERDICT_REFUTED.to_owned(),
                    ..note("O004", "src/c.go", 2, "Refuted")
                },
            ],
            tests: vec![
                "cargo test -p engine".to_owned(),
                "Pinged @octocat <!-- hidden -->".to_owned(),
            ],
            ..Artifact::default()
        };
        let plan = PublicationPlan {
            decision: Decision::RequestChanges,
            ..PublicationPlan::default()
        };
        let settings = test_settings();
        let review = render_review(&settings, &input, &artifact, &plan).expect("review renders");
        assert_eq!(review.body, NOTES_AND_VALIDATIONS_BODY);
        assert_eq!(
            review.comments.len(),
            1,
            "only the blocking finding is an inline comment"
        );
        assert_eq!(
            review.comments[0].body,
            "**O001 HIGH — Bug** (confidence: high, Opus)\n\nBreaks on an empty input.\n\n\
             🤖 Written by AI: test reviewer"
        );
    }

    #[test]
    fn human_review_signals_name_each_matched_signal() {
        let signals = HumanReview {
            labels: vec!["needs-human".to_owned(), "risky".to_owned()],
            deleted_tests: vec!["tests/**".to_owned(), "**/tests/**".to_owned()],
            paths: vec!["db/**".to_owned(), "**/Dockerfile".to_owned()],
            max_changed_lines: Some(100),
            max_changed_files: Some(3),
            max_notes: Some(1),
            authors: vec!["trusted".to_owned(), "octocat".to_owned()],
        };
        let strings = |values: &[&str]| values.iter().map(|v| (*v).to_owned()).collect::<Vec<_>>();
        let file = |path: &str, status: &str, additions: i64, deletions: i64| ChangedFile {
            path: path.to_owned(),
            status: status.to_owned(),
            additions,
            deletions,
            previous_path: None,
        };
        let renamed = |path: &str, previous: &str, additions: i64, deletions: i64| ChangedFile {
            path: path.to_owned(),
            status: "renamed".to_owned(),
            additions,
            deletions,
            previous_path: Some(previous.to_owned()),
        };
        // Each case: name, labels, changed files, notes, author login, author class, signals.
        type Case = (
            &'static str,
            Vec<String>,
            Vec<ChangedFile>,
            i64,
            &'static str,
            AuthorClass,
            bool,
            Vec<String>,
        );
        let cases: Vec<Case> = vec![
            (
                "nothing matches",
                strings(&["bug"]),
                vec![
                    file("src/a.go", "modified", 5, 1),
                    file("testsuite/a.go", "modified", 1, 1),
                ],
                0,
                "someone",
                AuthorClass::Untrusted,
                false,
                strings(&[]),
            ),
            (
                "label case is ignored",
                strings(&["NEEDS-HUMAN"]),
                vec![],
                0,
                "someone",
                AuthorClass::Untrusted,
                false,
                strings(&["label `needs-human`"]),
            ),
            (
                "deleted tests are sorted",
                strings(&[]),
                vec![
                    file("pkg/tests/b.php", "removed", 0, 40),
                    file("tests/a.php", "removed", 0, 3),
                    file("src/c.go", "removed", 0, 2),
                ],
                0,
                "someone",
                AuthorClass::Untrusted,
                false,
                strings(&["deleted test files `pkg/tests/b.php`, `tests/a.php`"]),
            ),
            (
                "paths name the first sorted match and the count",
                strings(&[]),
                vec![
                    file("docker/Dockerfile", "modified", 1, 0),
                    file("db/migrations/2.sql", "added", 4, 0),
                    file("db/migrations/1.sql", "added", 2, 0),
                ],
                0,
                "someone",
                AuthorClass::Untrusted,
                false,
                strings(&["changed path `db/migrations/1.sql` (3 matching files)"]),
            ),
            (
                "lines at the limit do not match",
                strings(&[]),
                vec![file("src/a.go", "modified", 60, 40)],
                0,
                "someone",
                AuthorClass::Untrusted,
                false,
                strings(&[]),
            ),
            (
                "lines over the limit match",
                strings(&[]),
                vec![file("src/a.go", "modified", 60, 41)],
                0,
                "someone",
                AuthorClass::Untrusted,
                false,
                strings(&["changed lines 101, limit 100"]),
            ),
            (
                "files over the limit match",
                strings(&[]),
                vec![
                    file("src/a.go", "modified", 1, 0),
                    file("src/b.go", "modified", 1, 0),
                    file("src/c.go", "modified", 1, 0),
                    file("src/d.go", "modified", 1, 0),
                ],
                0,
                "someone",
                AuthorClass::Untrusted,
                false,
                strings(&["changed files 4, limit 3"]),
            ),
            (
                "notes over the limit match",
                strings(&[]),
                vec![],
                2,
                "someone",
                AuthorClass::Untrusted,
                false,
                strings(&["notes 2, limit 1"]),
            ),
            (
                "author login is case-insensitive",
                strings(&[]),
                vec![],
                0,
                "OctoCat",
                AuthorClass::Untrusted,
                false,
                strings(&["author `OctoCat`"]),
            ),
            (
                "author `class`",
                strings(&[]),
                vec![],
                0,
                "someone",
                AuthorClass::Trusted,
                false,
                strings(&["author class trusted"]),
            ),
            (
                "listing truncated",
                strings(&[]),
                vec![file("src/a.go", "modified", 1, 0)],
                0,
                "someone",
                AuthorClass::Untrusted,
                true,
                strings(&["changed files exceed the listing limit (1 listed)"]),
            ),
            (
                "a rename out of a path glob matches its old path",
                strings(&[]),
                vec![renamed("archive/x.sql", "db/x.sql", 1, 0)],
                0,
                "someone",
                AuthorClass::Untrusted,
                false,
                strings(&["changed path `db/x.sql` (1 matching files)"]),
            ),
            (
                "a rename out of a test glob counts as a deleted test",
                strings(&[]),
                vec![renamed("src/a.php", "tests/a.php", 2, 2)],
                0,
                "someone",
                AuthorClass::Untrusted,
                false,
                strings(&["deleted test files `tests/a.php`"]),
            ),
            (
                "a rename inside the test globs is not a deleted test",
                strings(&[]),
                vec![renamed("tests/b.php", "tests/a.php", 1, 1)],
                0,
                "someone",
                AuthorClass::Untrusted,
                false,
                strings(&[]),
            ),
            (
                "paths ignore ASCII case",
                strings(&[]),
                vec![file("DB/X.SQL", "added", 1, 0)],
                0,
                "someone",
                AuthorClass::Untrusted,
                false,
                strings(&["changed path `DB/X.SQL` (1 matching files)"]),
            ),
            (
                "deleted tests ignore ASCII case",
                strings(&[]),
                vec![file("Tests/A.php", "removed", 0, 1)],
                0,
                "someone",
                AuthorClass::Untrusted,
                false,
                strings(&["deleted test files `Tests/A.php`"]),
            ),
            (
                "a filename with Markdown, a backtick and a mention stays in its code span",
                strings(&[]),
                vec![file("db/[x](y)*a*`b`@octocat.sql", "added", 1, 0)],
                0,
                "someone",
                AuthorClass::Untrusted,
                false,
                strings(&["changed path ``db/[x](y)*a*`b`@octocat.sql`` (1 matching files)"]),
            ),
            (
                "every signal at once",
                strings(&["risky"]),
                vec![
                    file("db/x.sql", "added", 3, 0),
                    file("tests/a.php", "removed", 0, 1),
                    file("src/big.go", "modified", 100, 1),
                ],
                2,
                "octocat",
                AuthorClass::Untrusted,
                false,
                strings(&[
                    "label `risky`",
                    "deleted test files `tests/a.php`",
                    "changed path `db/x.sql` (1 matching files)",
                    "changed lines 105, limit 100",
                    "notes 2, limit 1",
                    "author `octocat`",
                ]),
            ),
        ];
        for (name, labels, changed, notes, author, class, truncated, want) in cases {
            let facts = HumanReviewFacts {
                labels: &labels,
                author,
                author_class: &class,
                changed: &changed,
                truncated,
                notes,
            };
            let got = human_review_signals(&signals, &facts);
            assert_eq!(got, want, "{name}");
        }
    }

    #[test]
    fn human_review_text_drops_whole_signals_and_never_cuts_a_code_span() {
        let hostile = "changed path ``db/[x](y)*a*`b`@octocat.sql`` (1 matching files)".to_owned();
        let label = "label `needs-human`".to_owned();
        let cases = [
            (
                "the limit falls inside the hostile span",
                vec![hostile.clone(), label.clone()],
                40,
                format!("{hostile}; and 1 more"),
            ),
            (
                "a signal that fits is kept",
                vec![label.clone(), hostile.clone()],
                200,
                format!("{label}; {hostile}"),
            ),
            (
                "a first signal longer than the limit is kept whole",
                vec![hostile.clone()],
                10,
                hostile.clone(),
            ),
        ];
        for (name, signals, limit, want) in cases {
            let got = human_review_text(&signals, limit);
            assert_eq!(got, want, "{name}");
            assert!(
                got.matches('`').count().is_multiple_of(2),
                "{name}: unclosed backtick in {got}"
            );
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
            human_review: vec!["label `needs-human`".to_owned()],
            ..Artifact::default()
        };
        let plan = plan_publication(&input, &signalled, "reviewer");
        assert!(
            plan.decision == Decision::Approve && plan.await_approval,
            "a matched signal must wait for the user, plan = {plan:?}"
        );
    }

    /// The answered verdict's note also defines a link for the engine-written login (#338).
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
                note: "Waived by the reviewer.\n\n[bot]: https://example.org/x".to_owned(),
                relies_on: "name[bot]".to_owned(),
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
                && review.body.contains("relying on a comment by name[bot]")
                && review.body.contains("\n[bot]\\: https://example.org/x"),
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
            "check before approving:\n{} Name (relies on a comment by name[bot])",
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
                    // Go writes the legacy marker alone; the marker of the base branch
                    // follows it (#297).
                    let base = base_marker(&case.input.input.pull_request.base_branch);
                    let marker = format!("{}{base}", want.marker);
                    assert!(want.body.starts_with(&want.marker), "case {}", case.name);
                    let mut body = want.body.replacen(&want.marker, &marker, 1);
                    if case.name == NOTE_BODY_CASE {
                        assert!(body.contains(GO_NOTE_BODY), "case {}", case.name);
                        body = body.replacen(GO_NOTE_BODY, "", 1);
                    }
                    assert_eq!(review.body, rust_layout(&body), "case {}", case.name);
                    assert_eq!(review.marker, marker, "case {}", case.name);
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
                    let mut want_comments = want.comments.clone();
                    for comment in &mut want_comments {
                        comment.body = rust_layout(&comment.body);
                        if case.name == COMMENT_CASE {
                            for (go, rust) in COMMENT_CHANGES {
                                assert!(comment.body.contains(go), "case {}", case.name);
                                comment.body = comment.body.replacen(go, rust, 1);
                            }
                        }
                    }
                    assert_eq!(comments, want_comments, "case {}", case.name);
                }
                _ => panic!(
                    "case {} must hold exactly one of output and error",
                    case.name
                ),
            }
        }
    }
}
