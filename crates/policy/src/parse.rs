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
use serde_saphyr::granit_parser::{Event as YamlEvent, Parser, ScalarStyle, Tag};

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

/// The only `%YAML` directive version Go's YAML decoder accepts, as major and minor.
const GO_YAML_VERSION: (u32, u32) = (1, 1);

/// Go's error for a `%YAML` directive of another version.
const INCOMPATIBLE_DOCUMENT: &str = "yaml: found incompatible YAML document";

/// Go's error for a merge value that is not a mapping or a sequence of mappings.
const MERGE_NOT_MAPPING: &str = "yaml: map merge requires map or sequence of maps as the value";

/// The YAML merge key.
const MERGE_KEY: &str = "<<";

/// Handle of the YAML core schema tags.
const CORE_TAG_HANDLE: &str = "tag:yaml.org,2002:";

/// Suffix of the core schema merge tag.
const MERGE_TAG_SUFFIX: &str = "merge";

/// Handle of the non-specific tag `!`.
const NON_SPECIFIC_TAG_HANDLE: &str = "!";

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

/// Reads an integer field; null reads as zero and quoted text is refused.
///
/// Both match Go's YAML decoder. Go reads a leading-zero integer such as `030` as YAML 1.1 octal;
/// serde_saphyr reads it as a float, which this reader refuses.
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
    // A root or empty path has no parent. Go's filepath.Dir gives "/" and "." for them, and a
    // prompt path joined onto "/" or "" names the same file.
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
    if let Some(message) = go_yaml_refusal(text) {
        return Err(Error::InvalidYaml {
            message: message.to_owned(),
        });
    }
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

/// What an alias names, as Go's merge check sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NodeKind {
    /// A mapping.
    Mapping,
    /// A sequence or a scalar.
    Other,
}

/// The collection that holds the next node, while walking parser events.
#[derive(Debug)]
enum Frame {
    /// A mapping and the role of its next node.
    Mapping {
        /// Whether the next node is a key.
        key: bool,
        /// Whether the next node is the value of a merge key.
        merge: bool,
    },
    /// A sequence.
    Sequence {
        /// Whether the sequence is the value of a merge key.
        merge: bool,
    },
}

/// Returns Go's error for YAML that Go's decoder refuses and serde_saphyr reads.
///
/// These are a `%YAML` directive other than 1.1 and a merge value other than a mapping, an alias
/// of a mapping or a sequence of those (yaml.v3 `decoder.merge`). Only the first document is
/// walked. A parser error returns `None`, so serde_saphyr reports it: Go parses the whole document
/// before it decodes a merge, while a directive fails before the document content is read.
fn go_yaml_refusal(text: &str) -> Option<&'static str> {
    let mut anchors: BTreeMap<usize, NodeKind> = BTreeMap::new();
    let mut stack: Vec<Frame> = Vec::new();
    let mut merge_refused = false;
    for item in Parser::new_from_str(text) {
        let Ok((event, _)) = item else {
            return None;
        };
        match event {
            YamlEvent::DocumentStart(_, Some(version))
                if (version.major, version.minor) != GO_YAML_VERSION =>
            {
                return Some(INCOMPATIBLE_DOCUMENT);
            }
            YamlEvent::DocumentEnd => break,
            YamlEvent::Scalar(value, style, anchor, tag) => {
                record_anchor(&mut anchors, anchor, NodeKind::Other);
                merge_refused |= !merge_accepts(&stack, NodeKind::Other, false);
                let merge_key = value == MERGE_KEY && is_merge_key_tag(style, tag.as_deref());
                finish_node(&mut stack, merge_key);
            }
            YamlEvent::Alias(anchor) => {
                let kind = if anchors.get(&anchor) == Some(&NodeKind::Mapping) {
                    NodeKind::Mapping
                } else {
                    NodeKind::Other
                };
                merge_refused |= !merge_accepts(&stack, kind, false);
                finish_node(&mut stack, false);
            }
            YamlEvent::MappingStart(_, anchor, _) => {
                record_anchor(&mut anchors, anchor, NodeKind::Mapping);
                merge_refused |= !merge_accepts(&stack, NodeKind::Mapping, false);
                stack.push(Frame::Mapping {
                    key: true,
                    merge: false,
                });
            }
            YamlEvent::SequenceStart(_, anchor, _) => {
                record_anchor(&mut anchors, anchor, NodeKind::Other);
                merge_refused |= !merge_accepts(&stack, NodeKind::Other, true);
                let merge = matches!(
                    stack.last(),
                    Some(Frame::Mapping {
                        key: false,
                        merge: true
                    })
                );
                stack.push(Frame::Sequence { merge });
            }
            YamlEvent::MappingEnd | YamlEvent::SequenceEnd => {
                stack.pop();
                finish_node(&mut stack, false);
            }
            _ => {}
        }
    }
    merge_refused.then_some(MERGE_NOT_MAPPING)
}

/// Records the kind of an anchored node; anchor 0 means the node has no anchor.
fn record_anchor(anchors: &mut BTreeMap<usize, NodeKind>, anchor: usize, kind: NodeKind) {
    if anchor != 0 {
        anchors.insert(anchor, kind);
    }
}

/// Reports whether Go's merge accepts a node of kind at the position the stack names.
///
/// A merge value may be a mapping or a sequence; an entry of that sequence must be a mapping.
/// Nodes outside a merge value are always accepted.
fn merge_accepts(stack: &[Frame], kind: NodeKind, sequence: bool) -> bool {
    match stack.last() {
        Some(Frame::Mapping {
            key: false,
            merge: true,
        }) => kind == NodeKind::Mapping || sequence,
        Some(Frame::Sequence { merge: true }) => kind == NodeKind::Mapping,
        _ => true,
    }
}

/// Moves the parent mapping from a finished key to its value, or from a value to the next key.
fn finish_node(stack: &mut [Frame], merge_key: bool) {
    if let Some(Frame::Mapping { key, merge }) = stack.last_mut() {
        *merge = *key && merge_key;
        *key = !*key;
    }
}

/// Reports whether a `<<` scalar is a merge key for Go: plain without a tag, `!` or `!!merge`.
fn is_merge_key_tag(style: ScalarStyle, tag: Option<&Tag>) -> bool {
    match tag {
        None => style == ScalarStyle::Plain,
        Some(tag) => {
            (tag.handle() == CORE_TAG_HANDLE && tag.suffix() == MERGE_TAG_SUFFIX)
                || (tag.handle() == NON_SPECIFIC_TAG_HANDLE && tag.suffix().is_empty())
        }
    }
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
pub(crate) fn cut(value: &str) -> (&str, &str) {
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

#[cfg(test)]
mod tests {
    use std::io;
    use std::time::Duration;

    use super::{load, parse};
    use crate::Error;
    use crate::types::{MAX_PROMPT_BYTES, OnClean, OnNotes};

    const VALID_POLICY: &str = "version: 1
reviewers:
  opus: {protocol: claude-structured, agent_profile: claude-profile, label: Opus, finding_prefix: O}
  codex: {protocol: codex-envelope, agent_profile: codex-profile, label: Codex, finding_prefix: X}
pipelines:
  main: {reviewers: [opus], incremental_reviewers: [opus], prompt: prompts/main.md, checkout: base, verifier: {reviewer: opus, prompt: prompts/verifier.md, escalate_severities: [critical], escalate_count: 2}, project_sources: [manual]}
  side: {reviewers: [codex], incremental_reviewers: [codex], prompt: prompts/side.md, checkout: head, project_sources: [manual, auto]}
repositories:
  - {match: acme/special, trigger: review-requested, pipeline: main, on_findings: request_changes, on_clean: await}
  - {match: acme/*, trigger: review-requested, pipeline: main, on_findings: request_changes, on_clean: approve}
  - {match: solo/*, trigger: \"label:ai:review\", pipeline: side, on_findings: comment, on_clean: comment}
unmatched: skip
approval: {timeout_hours: 24}
notifications:
  approval_requested: action
  findings_on_own_pr: action
  review_stopped: action
  cleanup_manual: action
  command_failed: action
  approved: digest
  changes_requested: digest
  commented: digest
  approval_expired: digest
  started: log
  retry_scheduled: log
  superseded: log
  handoff_reminder: action
";

    fn prompts(name: &str) -> io::Result<Vec<u8>> {
        match name {
            "prompts/main.md" | "prompts/side.md" | "prompts/verifier.md" => {
                Ok(b"Review the pull request.\n".to_vec())
            }
            _ => Err(io::Error::from(io::ErrorKind::NotFound)),
        }
    }

    /// Sets on_notes comment on the acme/special rule.
    fn with_handoff(document: &str, hours: &str) -> String {
        let document =
            document.replacen("on_clean: await}", "on_clean: await, on_notes: comment}", 1);
        document.replacen(
            "approval: {timeout_hours: 24}\n",
            &format!(
                "approval: {{timeout_hours: 24}}\nnotes_handoff: {{reminder_hours: {hours}}}\n"
            ),
            1,
        )
    }

    /// Sets human_review on the approving acme/* rule.
    fn with_human_review(document: &str, value: &str) -> String {
        document.replacen(
            "on_findings: request_changes, on_clean: approve}",
            &format!("on_findings: request_changes, on_clean: approve, human_review: {value}}}"),
            1,
        )
    }

    /// Reports whether a cause in the chain of error is an I/O not-found error.
    fn is_not_found(error: &Error) -> bool {
        let mut current: Option<&(dyn std::error::Error + 'static)> = Some(error);
        while let Some(cause) = current {
            if let Some(io_error) = cause.downcast_ref::<io::Error>()
                && io_error.kind() == io::ErrorKind::NotFound
            {
                return true;
            }
            current = cause.source();
        }
        false
    }

    #[test]
    fn valid_policy_parses() {
        let parsed = parse(VALID_POLICY.as_bytes(), prompts).expect("the valid policy parses");
        let special = parsed.resolve("acme/special").map(|rule| rule.on_clean);
        assert_eq!(
            special,
            Some(OnClean::Await),
            "exact rule must win over the later owner rule"
        );
        let other = parsed.resolve("acme/other").map(|rule| rule.on_clean);
        assert_eq!(
            other,
            Some(OnClean::Approve),
            "owner rule must cover other repositories"
        );
        let again = parse(VALID_POLICY.as_bytes(), prompts).expect("the valid policy parses");
        assert!(
            parsed.digest() == again.digest() && parsed.digest().len() == 64,
            "the policy digest must be stable for identical input"
        );
    }

    #[test]
    fn policy_digest_changes_with_the_verifier() {
        let first = parse(VALID_POLICY.as_bytes(), prompts).expect("the valid policy parses");
        let document = VALID_POLICY.replacen("escalate_count: 2", "escalate_count: 3", 1);
        let second = parse(document.as_bytes(), prompts).expect("the verifier policy parses");
        assert_ne!(
            first.digest(),
            second.digest(),
            "a verifier change must change the policy digest"
        );
    }

    #[test]
    fn on_notes_comment_parses_and_changes_the_digest() {
        let first = parse(VALID_POLICY.as_bytes(), prompts).expect("the valid policy parses");
        let second = parse(with_handoff(VALID_POLICY, "24").as_bytes(), prompts)
            .expect("the handoff policy parses");
        assert!(
            first.handoff_reminder() == Duration::ZERO
                && second.handoff_reminder() == Duration::from_hours(24),
            "handoff reminder = {:?} and {:?}",
            first.handoff_reminder(),
            second.handoff_reminder()
        );
        let third = parse(with_handoff(VALID_POLICY, "48").as_bytes(), prompts);
        assert!(
            third.is_ok_and(|third| third.digest() != second.digest()),
            "a reminder change must change the policy digest"
        );
        let special = second.resolve("acme/special").map(|rule| rule.on_notes);
        assert_eq!(special, Some(OnNotes::Comment), "on_notes");
        let other = second.resolve("acme/other").map(|rule| rule.on_notes);
        assert_eq!(
            other,
            Some(OnNotes::Unset),
            "a rule without on_notes must follow its clean outcome"
        );
        assert_ne!(
            first.digest(),
            second.digest(),
            "an on_notes change must change the policy digest"
        );
    }

    #[test]
    fn human_review_parses_and_changes_the_digest() {
        let first = parse(VALID_POLICY.as_bytes(), prompts).expect("the valid policy parses");
        let document = with_human_review(
            VALID_POLICY,
            r#"{labels: [needs-human], deleted_tests: ["tests/**"]}"#,
        );
        let second = parse(document.as_bytes(), prompts).expect("the human review policy parses");
        let human = second
            .resolve("acme/other")
            .and_then(|rule| rule.human_review.as_ref());
        assert!(
            human.is_some_and(|human| human.labels.join(",") == "needs-human"
                && human.deleted_tests.join(",") == "tests/**"),
            "human_review = {human:?}"
        );
        let special = second
            .resolve("acme/special")
            .and_then(|rule| rule.human_review.as_ref());
        assert!(
            special.is_none(),
            "a rule without human_review must not get one"
        );
        assert_ne!(
            first.digest(),
            second.digest(),
            "a human_review change must change the policy digest"
        );
    }

    #[test]
    fn policy_digest_changes_with_the_prompt() {
        let first = parse(VALID_POLICY.as_bytes(), prompts).expect("the valid policy parses");
        let second = parse(VALID_POLICY.as_bytes(), |_| {
            Ok(b"Different instructions.".to_vec())
        })
        .expect("the policy with other prompts parses");
        assert_ne!(
            first.digest(),
            second.digest(),
            "a prompt change must change the policy digest"
        );
    }

    #[test]
    fn invalid_policy_is_rejected() {
        let valid = VALID_POLICY;
        let weaker_re_review = valid
            .replacen(
                "  main: {reviewers: [opus], incremental_reviewers: [opus],",
                "  main: {reviewers: [opus, extra], incremental_reviewers: [opus],",
                1,
            )
            .replacen(
                "  codex: {protocol: codex-envelope,",
                "  extra: {protocol: claude-structured, agent_profile: extra-profile, label: Extra, finding_prefix: E}\n  codex: {protocol: codex-envelope,",
                1,
            );
        let cases: Vec<(&str, String, &str)> = vec![
            ("unknown field", valid.replacen("unmatched: skip", "unmatched: skip\nauto_approve: true", 1), "field auto_approve not found"),
            ("version", valid.replacen("version: 1", "version: 2", 1), "version must be 1"),
            ("unmatched", valid.replacen("unmatched: skip", "unmatched: review", 1), "unmatched must be skip"),
            ("shadowed rule", valid.replacen("  - {match: acme/special,", "  - {match: acme/*, trigger: review-requested, pipeline: main, on_findings: comment, on_clean: comment}\n  - {match: acme/special,", 1), "unreachable"),
            ("duplicate rule", valid.replacen("  - {match: solo/*,", "  - {match: acme/special, trigger: review-requested, pipeline: main, on_findings: comment, on_clean: comment}\n  - {match: solo/*,", 1), "unreachable"),
            ("uppercase match", valid.replacen("match: acme/special", "match: Acme/special", 1), "lowercase"),
            ("bad trigger", valid.replacen("trigger: \"label:ai:review\"", "trigger: assigned", 1), "trigger"),
            ("unknown pipeline", valid.replacen("pipeline: side", "pipeline: missing", 1), "unknown pipeline"),
            ("bad on_clean", valid.replacen("on_clean: await", "on_clean: merge", 1), "on_clean"),
            ("on_notes without notes_handoff", valid.replacen("on_clean: await}", "on_clean: await, on_notes: comment}", 1), "needs notes_handoff.reminder_hours"),
            ("notes_handoff without on_notes", valid.replacen("approval: {timeout_hours: 24}\n", "approval: {timeout_hours: 24}\nnotes_handoff: {reminder_hours: 24}\n", 1), "no rule sets on_notes"),
            ("notes_handoff reminder out of range", with_handoff(valid, "0"), "reminder_hours must be between"),
            ("human_review on an awaiting rule", valid.replacen("on_clean: await}", "on_clean: await, human_review: {labels: [needs-human]}}", 1), "human_review needs on_clean approve"),
            ("empty human_review", with_human_review(valid, "{}"), "must set labels or deleted_tests"),
            ("invalid human_review label", with_human_review(valid, "{labels: [\"needs human\"]}"), "human_review labels"),
            ("duplicate human_review label", with_human_review(valid, "{labels: [needs-human, Needs-Human]}"), "human_review labels"),
            ("absolute deleted_tests glob", with_human_review(valid, "{deleted_tests: [/tests/**]}"), "human_review deleted_tests"),
            ("partial double star", with_human_review(valid, "{deleted_tests: [tests/**.php]}"), "human_review deleted_tests"),
            ("parent segment", with_human_review(valid, "{deleted_tests: [../tests/*]}"), "human_review deleted_tests"),
            ("broken class", with_human_review(valid, "{deleted_tests: [\"tests/[a\"]}"), "human_review deleted_tests"),
            ("bad on_notes", valid.replacen("on_clean: await}", "on_clean: await, on_notes: approve}", 1), "on_notes must be comment"),
            ("on_notes without a manual approval", valid.replacen("on_clean: approve}", "on_clean: approve, on_notes: comment}", 1), "on_notes comment needs on_clean await"),
            ("on_notes on a label trigger", valid.replacen("on_clean: comment}", "on_clean: await, on_notes: comment}", 1), "on_notes comment needs on_clean await"),
            ("bad on_findings", valid.replacen("on_findings: comment", "on_findings: ignore", 1), "on_findings"),
            ("mixed protocols", valid.replacen("reviewers: [opus], incremental", "reviewers: [opus, codex], incremental", 1), "same protocol"),
            ("incremental outside reviewers", valid.replacen("incremental_reviewers: [opus]", "incremental_reviewers: [codex]", 1), "is not one of its reviewers"),
            ("unknown reviewer", valid.replacen("reviewers: [opus], incremental", "reviewers: [gpt], incremental", 1), "unknown or repeated reviewer"),
            ("shared prefix", valid.replacen("finding_prefix: X", "finding_prefix: O", 1), "share finding_prefix"),
            ("bad protocol", valid.replacen("protocol: codex-envelope", "protocol: plain", 1), "protocol must be"),
            ("bad checkout", valid.replacen("checkout: head", "checkout: merge", 1), "checkout must be"),
            ("prompt escapes directory", valid.replacen("prompt: prompts/side.md", "prompt: ../side.md", 1), "inside the policy directory"),
            ("missing prompt", valid.replacen("prompt: prompts/side.md", "prompt: prompts/none.md", 1), "prompts/none.md"),
            ("missing event", valid.replacen("  superseded: log\n", "", 1), "event superseded needs a level"),
            ("unknown event", valid.replacen("  superseded: log\n", "  superseded: log\n  merged: log\n", 1), "unknown event"),
            ("bad level", valid.replacen("superseded: log", "superseded: telegram", 1), "level must be"),
            ("approval timeout", valid.replacen("timeout_hours: 24", "timeout_hours: 0", 1), "timeout_hours"),
            ("no source", valid.replacen("project_sources: [manual]}", "project_sources: []}", 1), "project_sources"),
            ("second document", format!("{valid}---\nversion: 1\n"), "exactly one YAML document"),
            ("duplicate key", valid.replacen("unmatched: skip", "unmatched: skip\nunmatched: skip", 1), "mapping key \"unmatched\" already defined"),
            ("verifier of another protocol", valid.replacen("verifier: {reviewer: opus,", "verifier: {reviewer: codex,", 1), "verifier must use protocol"),
            ("unknown verifier", valid.replacen("verifier: {reviewer: opus,", "verifier: {reviewer: gpt,", 1), "verifier names unknown reviewer"),
            ("verifier escalating a note", valid.replacen("escalate_severities: [critical]", "escalate_severities: [medium]", 1), "critical or high once each"),
            ("verifier without escalation", valid.replacen("escalate_count: 2", "escalate_count: 0", 1), "escalate_count of at least 1"),
            ("missing verifier prompt", valid.replacen("prompt: prompts/verifier.md", "prompt: prompts/none.md", 1), "prompts/none.md"),
            ("reserved reviewer id", valid.replacen("  codex: {protocol", "  verifier: {protocol", 1), "invalid or reserved"),
            ("ready draft author with the app prefix", format!("{valid}ready_draft_authors: [app/devin-ai-integration]\n"), "ready_draft_authors"),
            ("repeated ready draft author", format!("{valid}ready_draft_authors: [devin, devin]\n"), "ready_draft_authors"),
            ("weaker re-review before automatic approval", weaker_re_review, "incremental_reviewers equal to reviewers"),
        ];
        for (name, document, want) in cases {
            assert_ne!(
                document, valid,
                "{name}: mutation did not change the policy"
            );
            match parse(document.as_bytes(), prompts) {
                Ok(_) => panic!("{name}: parse succeeded, want an error mentioning {want:?}"),
                Err(error) => assert!(
                    error.to_string().contains(want),
                    "{name}: error = {error}, want it to mention {want:?}"
                ),
            }
        }
    }

    #[test]
    fn load_rejects_oversized_prompt() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let root = directory.path();
        std::fs::create_dir_all(root.join("prompts")).expect("prompts directory");
        std::fs::write(
            root.join("prompts").join("main.md"),
            "a".repeat(MAX_PROMPT_BYTES + 1),
        )
        .expect("main prompt");
        std::fs::write(root.join("prompts").join("side.md"), "Review.").expect("side prompt");
        std::fs::write(root.join("policy.yaml"), VALID_POLICY).expect("policy file");
        match load(&root.join("policy.yaml")) {
            Ok(_) => panic!("an oversized prompt must be rejected"),
            Err(error) => assert!(error.to_string().contains("exceeds"), "error = {error}"),
        }
        match load(&root.join("missing.yaml")) {
            Ok(_) => panic!("a missing policy file must be rejected"),
            Err(error) => assert!(is_not_found(&error), "missing policy error = {error}"),
        }
    }
}
