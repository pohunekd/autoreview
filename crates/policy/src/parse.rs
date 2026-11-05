//! Reading and validating the policy file and the prompt files it references.

// Rust guideline compliant 2026-06-26

use std::collections::BTreeMap;
use std::fmt;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;
use std::time::Duration;

use serde::Deserialize;
use serde::de::{self, Deserializer, Unexpected, Visitor};

use crate::Error;
use crate::digest::digest_of;
use crate::human_review::{FileHumanReview, parse_human_review};
use crate::types::{
    Checkout, EVENTS, Event, Level, MAX_APPROVAL_HOURS, MAX_POLICY_BYTES, MAX_PROMPT_BYTES,
    MAX_REVIEWERS_PER_PR, MAX_RULES, OnClean, OnFindings, OnNotes, Pipeline, Policy, Protocol,
    Reviewer, Rule, Trigger, TriggerKind, VERIFIER_REVIEWER_ID, Verifier, is_finding_prefix,
    is_identifier, is_label, is_owner, is_profile, is_repo_name, is_source,
};

/// Most characters a reviewer label may have.
const MAX_REVIEWER_LABEL_CHARS: usize = 64;

/// The only policy file version.
const POLICY_VERSION: i64 = 1;

/// The only accepted value of unmatched.
const UNMATCHED_SKIP: &str = "skip";

/// Prefix of a label trigger.
const LABEL_TRIGGER_PREFIX: &str = "label:";

/// Severities a blocking finding can have, so the only ones a verifier can demote.
const BLOCKING_SEVERITIES: &[&str] = &["critical", "high"];

/// Reads the prompt at a path as written in the policy file.
type ReadPrompt<'a> = &'a dyn Fn(&str) -> io::Result<Vec<u8>>;

/// A reviewer as written in the policy file.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct FileReviewer {
    /// Protocol name.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    protocol: String,
    /// Agent profile name.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    agent_profile: String,
    /// Display label.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    label: String,
    /// Finding prefix letter.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    finding_prefix: String,
}

/// A pipeline as written in the policy file.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct FilePipeline {
    /// Reviewer ids of a full review.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    reviewers: Vec<String>,
    /// Reviewer ids of an incremental review.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    incremental_reviewers: Vec<String>,
    /// Prompt path relative to the policy directory.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    prompt: String,
    /// Checkout name.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    checkout: String,
    /// Project source names.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    project_sources: Vec<String>,
    /// Verifier section, absent for unchecked findings.
    verifier: Option<FileVerifier>,
}

/// A verifier as written in the policy file.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct FileVerifier {
    /// Reviewer id of the verifier.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    reviewer: String,
    /// Prompt path relative to the policy directory.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    prompt: String,
    /// Severities whose demotion escalates to the user.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    escalate_severities: Vec<String>,
    /// Blocking findings count that escalates to the user.
    #[serde(deserialize_with = "go_int")]
    escalate_count: i64,
}

/// A repository rule as written in the policy file.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct FileRule {
    /// Repository glob.
    #[serde(
        rename = "match",
        deserialize_with = "autoreview_go_compat::null_default::deserialize"
    )]
    match_glob: String,
    /// Trigger text.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    trigger: String,
    /// Pipeline name.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    pipeline: String,
    /// Outcome name for findings.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    on_findings: String,
    /// Outcome name for clean reviews.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    on_clean: String,
    /// Outcome name for notes-only reviews; empty when unset.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    on_notes: String,
    /// Human review section, absent when the rule has none.
    human_review: Option<FileHumanReview>,
}

/// The approval section of the policy file.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct FileApproval {
    /// Hours a review waits for the user's approval.
    #[serde(deserialize_with = "go_int")]
    timeout_hours: i64,
}

/// The notes_handoff section of the policy file.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct FileNotesHandoff {
    /// Hours handed-off notes wait before the user is reminded.
    #[serde(deserialize_with = "go_int")]
    reminder_hours: i64,
}

/// The policy file as written.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct FilePolicy {
    /// File format version.
    #[serde(deserialize_with = "go_int")]
    version: i64,
    /// Reviewers by id.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    reviewers: BTreeMap<String, FileReviewer>,
    /// Pipelines by name.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    pipelines: BTreeMap<String, FilePipeline>,
    /// Repository rules in priority order.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    repositories: Vec<FileRule>,
    /// What happens to repositories no rule matches.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    unmatched: String,
    /// Approval settings.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    approval: FileApproval,
    /// Notes handoff settings, absent when no rule hands notes off.
    notes_handoff: Option<FileNotesHandoff>,
    /// Level names by event name; a null level reads as empty.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    notifications: BTreeMap<String, Option<String>>,
    /// Coding agents that open drafts and never mark them ready for review.
    #[serde(deserialize_with = "autoreview_go_compat::null_default::deserialize")]
    ready_draft_authors: Vec<String>,
}

/// Reads an integer field as Go's YAML decoder does: null is zero and text is refused.
fn go_int<'de, D: Deserializer<'de>>(deserializer: D) -> Result<i64, D::Error> {
    deserializer.deserialize_any(GoIntVisitor)
}

/// Accepts a YAML integer or null for go_int.
struct GoIntVisitor;

impl Visitor<'_> for GoIntVisitor {
    type Value = i64;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an integer")
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(value)
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        i64::try_from(value).map_err(|_| E::invalid_value(Unexpected::Unsigned(value), &self))
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(0)
    }

    fn visit_none<E>(self) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(0)
    }
}

/// Reads the policy file and the prompt files it references.
///
/// Prompt paths are relative to the policy file and must stay inside its directory.
///
/// # Errors
///
/// Returns [`Error::PolicyFile`] when the policy file cannot be read or exceeds its size limit, and
/// any error of [`parse`].
pub fn load(path: &Path) -> Result<Policy, Error> {
    let data = read_bounded(path, MAX_POLICY_BYTES).map_err(|source| Error::PolicyFile {
        path: path.to_path_buf(),
        source,
    })?;
    // A root or empty path has no parent; Go's filepath.Dir then returns the path itself.
    let directory = path.parent().unwrap_or(path);
    parse(&data, |relative| {
        read_bounded(&directory.join(relative), MAX_PROMPT_BYTES)
    })
}

/// Reads a file of at most limit bytes.
fn read_bounded(path: &Path, limit: usize) -> io::Result<Vec<u8>> {
    let file = File::open(path)?;
    let limit_u64 = u64::try_from(limit).map_err(io::Error::other)?;
    let mut limited = file.take(limit_u64.saturating_add(1));
    let mut buffer = Vec::new();
    let read = limited.read_to_end(&mut buffer)?;
    if read > limit {
        return Err(io::Error::new(
            io::ErrorKind::FileTooLarge,
            format!("file exceeds {limit} bytes"),
        ));
    }
    Ok(buffer)
}

/// Validates a policy document; read_prompt returns the content of a prompt path as written.
///
/// # Errors
///
/// Returns an error when the document is not one YAML document of the policy structure or breaks
/// a policy rule, or when a prompt cannot be read or is not valid prompt text.
pub fn parse(
    data: &[u8],
    read_prompt: impl Fn(&str) -> io::Result<Vec<u8>>,
) -> Result<Policy, Error> {
    let text = std::str::from_utf8(data).map_err(|error| Error::InvalidYaml {
        message: error.to_string(),
    })?;
    let document: FilePolicy = serde_saphyr::from_str(text).map_err(|error| yaml_error(&error))?;
    if document.version != POLICY_VERSION {
        return Err(Error::Version);
    }
    if document.unmatched != UNMATCHED_SKIP {
        return Err(Error::Unmatched);
    }
    if !(1..=MAX_APPROVAL_HOURS).contains(&document.approval.timeout_hours) {
        return Err(Error::ApprovalTimeout {
            max: MAX_APPROVAL_HOURS,
        });
    }
    let reviewers = parse_reviewers(&document.reviewers)?;
    let pipelines = parse_pipelines(&document.pipelines, &reviewers, &read_prompt)?;
    let rules = parse_rules(&document.repositories, &pipelines)?;
    let notifications = parse_notifications(&document.notifications)?;
    let reminder_hours = parse_notes_handoff(document.notes_handoff.as_ref(), &rules)?;
    let mut authors = Vec::new();
    for author in &document.ready_draft_authors {
        if !is_owner(author) || authors.contains(author) {
            return Err(Error::ReadyDraftAuthor {
                author: author.clone(),
            });
        }
        authors.push(author.clone());
    }
    authors.sort();
    let digest = digest_of(
        &rules,
        &notifications,
        document.approval.timeout_hours,
        reminder_hours,
        &authors,
    );
    Ok(Policy {
        rules,
        ready_draft_authors: authors,
        notifications,
        approval_timeout: Duration::from_hours(document.approval.timeout_hours.unsigned_abs()),
        handoff_reminder: Duration::from_hours(reminder_hours.unsigned_abs()),
        digest,
    })
}

/// Maps a YAML reader error to the policy error.
///
/// Unknown and repeated keys get Go's wording.
fn yaml_error(error: &serde_saphyr::Error) -> Error {
    let without = error.without_snippet();
    match without {
        serde_saphyr::Error::MultipleDocuments { .. } => Error::MultipleDocuments,
        serde_saphyr::Error::SerdeUnknownField {
            field, location, ..
        } => Error::InvalidYaml {
            message: format!("line {}: field {field} not found", location.line()),
        },
        serde_saphyr::Error::DuplicateMappingKey {
            key: Some(key),
            location,
            ..
        } => Error::InvalidYaml {
            message: format!(
                "line {}: mapping key {key:?} already defined",
                location.line()
            ),
        },
        other => Error::InvalidYaml {
            message: other.to_string(),
        },
    }
}

/// Validates the reviewers of the policy file and returns them by id.
fn parse_reviewers(
    raw: &BTreeMap<String, FileReviewer>,
) -> Result<BTreeMap<String, Reviewer>, Error> {
    if raw.is_empty() {
        return Err(Error::NoReviewers);
    }
    let mut reviewers = BTreeMap::new();
    let mut prefixes: BTreeMap<String, String> = BTreeMap::new();
    for (id, value) in raw {
        if !is_identifier(id) || id == VERIFIER_REVIEWER_ID {
            return Err(Error::ReviewerId { id: id.clone() });
        }
        let protocol = find_value(
            &[Protocol::ClaudeStructured, Protocol::CodexEnvelope],
            &value.protocol,
            Protocol::as_str,
        );
        let Some(protocol) = protocol else {
            return Err(Error::ReviewerProtocol { id: id.clone() });
        };
        if !is_profile(&value.agent_profile) {
            return Err(Error::ReviewerProfile { id: id.clone() });
        }
        if !valid_text(&value.label, MAX_REVIEWER_LABEL_CHARS) {
            return Err(Error::ReviewerLabel {
                id: id.clone(),
                max: MAX_REVIEWER_LABEL_CHARS,
            });
        }
        if !is_finding_prefix(&value.finding_prefix) {
            return Err(Error::ReviewerPrefix { id: id.clone() });
        }
        if let Some(first) = prefixes.get(&value.finding_prefix) {
            return Err(Error::SharedPrefix {
                first: first.clone(),
                second: id.clone(),
                prefix: value.finding_prefix.clone(),
            });
        }
        prefixes.insert(value.finding_prefix.clone(), id.clone());
        reviewers.insert(
            id.clone(),
            Reviewer {
                id: id.clone(),
                label: value.label.clone(),
                protocol,
                agent_profile: value.agent_profile.clone(),
                finding_prefix: value.finding_prefix.clone(),
            },
        );
    }
    Ok(reviewers)
}

/// Validates the pipelines, loads their prompts and returns them by name.
fn parse_pipelines(
    raw: &BTreeMap<String, FilePipeline>,
    reviewers: &BTreeMap<String, Reviewer>,
    read_prompt: ReadPrompt<'_>,
) -> Result<BTreeMap<String, Pipeline>, Error> {
    if raw.is_empty() {
        return Err(Error::NoPipelines);
    }
    let mut pipelines = BTreeMap::new();
    for (name, value) in raw {
        if !is_identifier(name) {
            return Err(Error::PipelineName { name: name.clone() });
        }
        let selected = select_reviewers(name, "reviewers", &value.reviewers, reviewers)?;
        let incremental = select_reviewers(
            name,
            "incremental_reviewers",
            &value.incremental_reviewers,
            reviewers,
        )?;
        for id in &incremental {
            if !selected.iter().any(|reviewer| reviewer.id == id.id) {
                return Err(Error::IncrementalReviewer {
                    pipeline: name.clone(),
                    id: id.id.clone(),
                });
            }
        }
        let Some(first) = selected.first() else {
            return Err(Error::ReviewerCount {
                pipeline: name.clone(),
                field: "reviewers",
                max: MAX_REVIEWERS_PER_PR,
            });
        };
        let first_protocol = first.protocol;
        if selected
            .iter()
            .any(|reviewer| reviewer.protocol != first_protocol)
        {
            // The prompt is written for one request and result contract.
            return Err(Error::MixedProtocols {
                pipeline: name.clone(),
            });
        }
        let checkout = find_value(
            &[Checkout::Base, Checkout::Head],
            &value.checkout,
            Checkout::as_str,
        );
        let Some(checkout) = checkout else {
            return Err(Error::PipelineCheckout {
                pipeline: name.clone(),
            });
        };
        if value.project_sources.is_empty() {
            return Err(Error::NoProjectSources {
                pipeline: name.clone(),
            });
        }
        let mut sources = Vec::new();
        for source in &value.project_sources {
            if !is_source(source) || sources.contains(source) {
                return Err(Error::ProjectSource {
                    pipeline: name.clone(),
                    name: source.clone(),
                });
            }
            sources.push(source.clone());
        }
        let prompt = load_prompt(name, &value.prompt, read_prompt)?;
        let verifier = parse_verifier(name, value.verifier.as_ref(), reviewers, read_prompt)?;
        pipelines.insert(
            name.clone(),
            Pipeline {
                name: name.clone(),
                protocol: first_protocol,
                reviewers: selected,
                incremental_reviewers: incremental,
                prompt,
                checkout,
                project_sources: sources,
                verifier,
            },
        );
    }
    Ok(pipelines)
}

/// Returns the reviewers a pipeline field names, in the order the field lists them.
fn select_reviewers(
    pipeline: &str,
    field: &'static str,
    ids: &[String],
    reviewers: &BTreeMap<String, Reviewer>,
) -> Result<Vec<Reviewer>, Error> {
    if ids.is_empty() || ids.len() > MAX_REVIEWERS_PER_PR {
        return Err(Error::ReviewerCount {
            pipeline: pipeline.to_owned(),
            field,
            max: MAX_REVIEWERS_PER_PR,
        });
    }
    let mut selected = Vec::new();
    for id in ids {
        let Some(reviewer) = reviewers.get(id) else {
            return Err(Error::ReviewerUnknown {
                pipeline: pipeline.to_owned(),
                field,
                id: id.clone(),
            });
        };
        if selected
            .iter()
            .any(|existing: &Reviewer| &existing.id == id)
        {
            return Err(Error::ReviewerUnknown {
                pipeline: pipeline.to_owned(),
                field,
                id: id.clone(),
            });
        }
        selected.push(reviewer.clone());
    }
    Ok(selected)
}

/// Validates the verifier section of a pipeline and loads its prompt.
fn parse_verifier(
    pipeline: &str,
    raw: Option<&FileVerifier>,
    reviewers: &BTreeMap<String, Reviewer>,
    read_prompt: ReadPrompt<'_>,
) -> Result<Option<Verifier>, Error> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let Some(reviewer) = reviewers.get(&raw.reviewer) else {
        return Err(Error::VerifierReviewer {
            pipeline: pipeline.to_owned(),
            reviewer: raw.reviewer.clone(),
        });
    };
    if reviewer.protocol != Protocol::ClaudeStructured {
        // Only the structured protocol enforces the verdict schema.
        return Err(Error::VerifierProtocol {
            pipeline: pipeline.to_owned(),
        });
    }
    let prompt = load_prompt(pipeline, &raw.prompt, read_prompt)?;
    if raw.escalate_severities.is_empty() || raw.escalate_count < 1 {
        return Err(Error::VerifierEscalation {
            pipeline: pipeline.to_owned(),
        });
    }
    let mut seen = Vec::new();
    for severity in &raw.escalate_severities {
        if !BLOCKING_SEVERITIES.contains(&severity.as_str()) || seen.contains(severity) {
            return Err(Error::VerifierSeverities {
                pipeline: pipeline.to_owned(),
            });
        }
        seen.push(severity.clone());
    }
    Ok(Some(Verifier {
        reviewer: reviewer.clone(),
        prompt,
        escalate_severities: raw.escalate_severities.clone(),
        escalate_count: raw.escalate_count,
    }))
}

/// Reads a prompt through read_prompt and returns its text without surrounding white space.
fn load_prompt(
    pipeline: &str,
    relative: &str,
    read_prompt: ReadPrompt<'_>,
) -> Result<String, Error> {
    if !is_relative_clean_path(relative) {
        return Err(Error::PromptPath {
            pipeline: pipeline.to_owned(),
        });
    }
    let bytes = read_prompt(relative).map_err(|source| Error::PromptRead {
        pipeline: pipeline.to_owned(),
        path: relative.to_owned(),
        source,
    })?;
    if bytes.len() > MAX_PROMPT_BYTES {
        return Err(Error::PromptContent {
            pipeline: pipeline.to_owned(),
            path: relative.to_owned(),
            max: MAX_PROMPT_BYTES,
        });
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| Error::PromptContent {
        pipeline: pipeline.to_owned(),
        path: relative.to_owned(),
        max: MAX_PROMPT_BYTES,
    })?;
    if text.contains('\0') || text.trim().is_empty() {
        return Err(Error::PromptContent {
            pipeline: pipeline.to_owned(),
            path: relative.to_owned(),
            max: MAX_PROMPT_BYTES,
        });
    }
    Ok(text.trim().to_owned())
}

/// Reports whether path is relative, uses no backslash and equals its Go path.Clean form.
///
/// Go accepts a path when it is non-empty, equals path.Clean of itself, is not absolute, is not ..
/// and does not start with ../, and holds no backslash. For a non-empty path without a leading
/// slash that is the same as: the path is ., or every slash-separated segment is non-empty and
/// neither . nor .. (path.Clean removes empty and . segments and resolves or keeps .. segments,
/// and a kept leading .. is rejected by the other checks).
fn is_relative_clean_path(path: &str) -> bool {
    if path.is_empty() || path.contains('\\') {
        return false;
    }
    if path.starts_with('/') {
        return false;
    }
    if path == "." {
        return true;
    }
    for segment in path.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return false;
        }
    }
    true
}

/// Parses a rule trigger: review-requested, or label: followed by a valid label.
fn parse_trigger(raw: &str) -> Result<Trigger, Error> {
    if raw == TriggerKind::ReviewRequested.as_str() {
        return Ok(Trigger {
            kind: TriggerKind::ReviewRequested,
            label: String::new(),
        });
    }
    if let Some(rest) = raw.strip_prefix(LABEL_TRIGGER_PREFIX)
        && is_label(rest)
    {
        return Ok(Trigger {
            kind: TriggerKind::Label,
            label: rest.to_owned(),
        });
    }
    Err(Error::RuleTrigger {
        raw: raw.to_owned(),
    })
}

/// Validates the repository rules in file order and resolves their pipelines.
fn parse_rules(
    raw: &[FileRule],
    pipelines: &BTreeMap<String, Pipeline>,
) -> Result<Vec<Rule>, Error> {
    if raw.is_empty() || raw.len() > MAX_RULES {
        return Err(Error::RuleCount { max: MAX_RULES });
    }
    let mut rules = Vec::new();
    for (index, value) in raw.iter().enumerate() {
        let rule = parse_rule(value, &rules, pipelines).map_err(|source| Error::Rule {
            position: format!("repositories[{index}] ({})", value.match_glob),
            source: Box::new(source),
        })?;
        rules.push(rule);
    }
    Ok(rules)
}

/// Validates one repository rule against the rules before it.
fn parse_rule(
    value: &FileRule,
    earlier_rules: &[Rule],
    pipelines: &BTreeMap<String, Pipeline>,
) -> Result<Rule, Error> {
    if !valid_match(&value.match_glob) {
        return Err(Error::RuleMatch);
    }
    for earlier in earlier_rules {
        if matches(&earlier.match_glob, &value.match_glob) {
            // First match wins, so this rule could never apply.
            return Err(Error::RuleUnreachable {
                earlier: earlier.match_glob.clone(),
            });
        }
    }
    let trigger = parse_trigger(&value.trigger)?;
    let Some(pipeline) = pipelines.get(&value.pipeline) else {
        return Err(Error::RuleUnknownPipeline {
            name: value.pipeline.clone(),
        });
    };
    let on_findings = find_value(
        &[OnFindings::RequestChanges, OnFindings::Comment],
        &value.on_findings,
        OnFindings::as_str,
    );
    let Some(on_findings) = on_findings else {
        return Err(Error::RuleOnFindings);
    };
    let on_clean = find_value(
        &[OnClean::Approve, OnClean::Await, OnClean::Comment],
        &value.on_clean,
        OnClean::as_str,
    );
    let Some(on_clean) = on_clean else {
        return Err(Error::RuleOnClean);
    };
    if on_clean == OnClean::Approve
        && !same_reviewers(&pipeline.reviewers, &pipeline.incremental_reviewers)
    {
        // Nobody confirms an automatic approval, so a re-review must not rest on fewer reviewers
        // than the full review.
        return Err(Error::RuleApproveNeedsFullReReview);
    }
    let on_notes = find_value(&[OnNotes::Comment], &value.on_notes, OnNotes::as_str);
    let on_notes = if value.on_notes.is_empty() {
        OnNotes::Unset
    } else if let Some(value) = on_notes {
        value
    } else {
        return Err(Error::RuleOnNotes);
    };
    if on_notes == OnNotes::Comment
        && (on_clean != OnClean::Await || trigger.kind != TriggerKind::ReviewRequested)
    {
        // The author's renewed review request ends the hand-off, and only the user's approval may
        // follow it.
        return Err(Error::RuleOnNotesNeedsAwait);
    }
    let human_review = parse_human_review(value.human_review.as_ref(), on_clean)?;
    Ok(Rule {
        match_glob: value.match_glob.clone(),
        trigger,
        pipeline: pipeline.clone(),
        on_findings,
        on_clean,
        on_notes,
        human_review,
    })
}

/// Returns the reminder hours of notes handed to an author.
///
/// The section is required exactly when a rule hands notes off; without one the hours are zero.
fn parse_notes_handoff(raw: Option<&FileNotesHandoff>, rules: &[Rule]) -> Result<i64, Error> {
    let used = rules.iter().any(|rule| rule.on_notes == OnNotes::Comment);
    match (raw, used) {
        (None, false) => Ok(0),
        (None, true) => Err(Error::NotesHandoffMissing),
        (Some(_), false) => Err(Error::NotesHandoffUnused),
        (Some(value), true) => {
            if !(1..=MAX_APPROVAL_HOURS).contains(&value.reminder_hours) {
                return Err(Error::NotesHandoffHours {
                    max: MAX_APPROVAL_HOURS,
                });
            }
            Ok(value.reminder_hours)
        }
    }
}

/// Validates the notification level of every known event.
///
/// No other event may appear.
fn parse_notifications(
    raw: &BTreeMap<String, Option<String>>,
) -> Result<BTreeMap<Event, Level>, Error> {
    for key in raw.keys() {
        if !EVENTS.iter().any(|event| event.as_str() == key) {
            return Err(Error::UnknownEvent { event: key.clone() });
        }
    }
    let mut notifications = BTreeMap::new();
    for event in EVENTS {
        let Some(value) = raw.get(event.as_str()) else {
            return Err(Error::EventLevelMissing {
                event: event.as_str(),
            });
        };
        let level = match value.as_deref() {
            Some("action") => Level::Action,
            Some("digest") => Level::Digest,
            Some("log") => Level::Log,
            _ => {
                // A null level is Go's empty level, which no level matches.
                return Err(Error::EventLevel {
                    event: event.as_str(),
                });
            }
        };
        notifications.insert(*event, level);
    }
    Ok(notifications)
}

/// Returns the value among values whose text is raw.
fn find_value<T: Copy>(values: &[T], raw: &str, text: fn(T) -> &'static str) -> Option<T> {
    for value in values {
        if text(*value) == raw {
            return Some(*value);
        }
    }
    None
}

/// Reports whether two reviewer lists name the same reviewers in the same order.
fn same_reviewers(left: &[Reviewer], right: &[Reviewer]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    for (a, b) in left.iter().zip(right.iter()) {
        if a.id != b.id {
            return false;
        }
    }
    true
}

/// Reports whether pattern is owner/repository or owner/* in lowercase.
fn valid_match(pattern: &str) -> bool {
    let Some((owner, name)) = pattern.split_once('/') else {
        return false;
    };
    if !is_owner(owner) {
        return false;
    }
    if name == "*" {
        return true;
    }
    is_repo_name(name)
}

/// Reports whether pattern covers target, where target is a repository or another pattern.
pub(crate) fn matches(pattern: &str, target: &str) -> bool {
    if pattern == target {
        return true;
    }
    let (owner, name) = cut(pattern);
    let (target_owner, _) = cut(target);
    name == "*" && owner == target_owner
}

/// Splits value at its first slash, like Go's strings.Cut.
///
/// Without a slash it returns the whole value and an empty string.
fn cut(value: &str) -> (&str, &str) {
    if let Some((left, right)) = value.split_once('/') {
        (left, right)
    } else {
        (value, "")
    }
}

/// Reports whether value is a single non-blank line of at most maximum characters.
///
/// It must not hold a NUL byte.
fn valid_text(value: &str, maximum: usize) -> bool {
    !value.trim().is_empty()
        && value.chars().count() <= maximum
        && !value.contains(['\0', '\r', '\n'])
}
