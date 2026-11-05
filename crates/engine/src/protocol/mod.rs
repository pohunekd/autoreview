//! Reviewer requests and the validation of reviewer output.
//!
//! Parsing the Claude and Codex outputs and combining the results live in the parsing module.

// Rust guideline compliant 2026-06-26

pub(crate) mod request;
pub(crate) mod validate;

/// Fixtures of the ported Go tests. They keep only the fields the tests read.
#[cfg(test)]
pub(crate) mod fixtures {
    use autoreview_policy::{Pipeline, Protocol, Reviewer, Rule};

    use crate::model::{PullRequest, ReviewInput, ReviewScope};

    /// Returns a 40-character SHA made of `character` repeated.
    pub(crate) fn sha(character: &str) -> String {
        character.repeat(40)
    }

    /// Claude reviewer of the fixtures.
    pub(crate) fn opus() -> Reviewer {
        Reviewer {
            id: "opus".to_owned(),
            label: "Opus".to_owned(),
            protocol: Protocol::ClaudeStructured,
            agent_profile: "claude-profile".to_owned(),
            finding_prefix: "O".to_owned(),
        }
    }

    /// Codex reviewer of the fixtures.
    pub(crate) fn codex() -> Reviewer {
        Reviewer {
            id: "codex".to_owned(),
            label: "Codex".to_owned(),
            protocol: Protocol::CodexEnvelope,
            agent_profile: "codex-profile".to_owned(),
            finding_prefix: "X".to_owned(),
        }
    }

    /// Pull request `number` of `acme/widget`.
    pub(crate) fn test_pull_request(number: i64) -> PullRequest {
        PullRequest {
            repository: "acme/widget".to_owned(),
            number,
            head_sha: sha("a"),
            review_request_id: 7,
            title: "Add widget".to_owned(),
            url: format!("https://github.com/acme/widget/pull/{number}"),
            author: "colleague".to_owned(),
            head_branch: "feature".to_owned(),
            base_branch: "main".to_owned(),
            base_sha: sha("b"),
            ..PullRequest::default()
        }
    }

    /// Input of `reviewer` on `pr` with a full scope.
    pub(crate) fn test_input(reviewer: &Reviewer, pr: PullRequest) -> ReviewInput {
        ReviewInput {
            pull_request: pr,
            rule: Rule {
                pipeline: Pipeline {
                    name: "main".to_owned(),
                    protocol: reviewer.protocol,
                    reviewers: vec![reviewer.clone()],
                    incremental_reviewers: vec![reviewer.clone()],
                    prompt: "Review the pull request.".to_owned(),
                    ..Pipeline::default()
                },
                ..Rule::default()
            },
            scope: ReviewScope {
                mode: "full".to_owned(),
                ..ReviewScope::default()
            },
            ..ReviewInput::default()
        }
    }
}
