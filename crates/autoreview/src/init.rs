//! The `init` command: writes a new configuration directory from an embedded example.
//!
//! It asks nothing, needs no configuration and never overwrites a file.

// Rust guideline compliant 2026-11-09

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use autoreview_engine::config::identifier_valid;
use autoreview_engine::{Error, go_json};
use clap::{Args, ValueEnum};
use serde::Serialize;

use crate::config_file::{clean_absolute, github_login_valid};
use crate::{CONFIGURATION_INVALID, command_invalid, failed};

/// Error code of a file or directory that is in the way of `init`.
pub(crate) const INIT_FILE_EXISTS: &str = "init_file_exists";

/// The configuration that all examples share: the complete file of the configuration reference.
const CONFIG_TEMPLATE: &str = include_str!("../../../examples/config.yaml");

/// The `policy_file` line of the template.
const POLICY_LINE: &str = "policy_file: /etc/autoreview/policy.yaml\n";

/// The credential `login` line of the template.
const LOGIN_LINE: &str = "      login: review-bot\n";

/// The `pohunek.hosts` line of the template.
const HOSTS_LINE: &str = "  hosts: [review-host]\n";

/// Name of the written configuration file.
const CONFIG_NAME: &str = "config.yaml";

/// Name of the written policy file, which `policy_file` names.
const POLICY_NAME: &str = "policy.yaml";

/// The two default prompts that every example references, as paths relative to the policy.
const DEFAULT_PROMPTS: [(&str, &[u8]); 2] = [
    (
        "prompts/generic.md",
        include_bytes!("../../../prompts/generic.md"),
    ),
    (
        "prompts/verifier.md",
        include_bytes!("../../../prompts/verifier.md"),
    ),
];

/// The prompts that `org-wildcard` adds to the default prompts.
const ORG_WILDCARD_PROMPTS: [(&str, &[u8]); 2] = [
    (
        "prompts/android.md",
        include_bytes!("../../../examples/org-wildcard/prompts/android.md"),
    ),
    (
        "prompts/companion.md",
        include_bytes!("../../../examples/org-wildcard/prompts/companion.md"),
    ),
];

/// The examples that `init` writes, named as their directories under `examples/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum Example {
    /// One reviewer, a verifier and automatic approval.
    AutoApprove,
    /// Drafts of coding agents marked ready, then a human approval.
    CodingAgentDrafts,
    /// Two reviewers, a verifier with escalation and approval by a person.
    HumanApproval,
    /// Notes go to the author first.
    NotesHandoff,
    /// Specific rules before a wildcard, with per-repository prompts.
    OrgWildcard,
    /// A label gate, maintainers only and a base checkout for others.
    PublicRepositories,
    /// A label trigger, one reviewer and comments only.
    SingleRepoComment,
    /// Own pull requests, with trusted and untrusted authors.
    SoloMaintainer,
}

impl Example {
    /// Returns the `policy.yaml` of the example.
    pub(crate) fn policy(self) -> &'static [u8] {
        match self {
            Self::AutoApprove => include_bytes!("../../../examples/auto-approve/policy.yaml"),
            Self::CodingAgentDrafts => {
                include_bytes!("../../../examples/coding-agent-drafts/policy.yaml")
            }
            Self::HumanApproval => include_bytes!("../../../examples/human-approval/policy.yaml"),
            Self::NotesHandoff => include_bytes!("../../../examples/notes-handoff/policy.yaml"),
            Self::OrgWildcard => include_bytes!("../../../examples/org-wildcard/policy.yaml"),
            Self::PublicRepositories => {
                include_bytes!("../../../examples/public-repositories/policy.yaml")
            }
            Self::SingleRepoComment => {
                include_bytes!("../../../examples/single-repo-comment/policy.yaml")
            }
            Self::SoloMaintainer => {
                include_bytes!("../../../examples/solo-maintainer/policy.yaml")
            }
        }
    }

    /// Returns the prompts the example adds, as paths relative to the policy.
    pub(crate) fn extra_prompts(self) -> &'static [(&'static str, &'static [u8])] {
        match self {
            Self::OrgWildcard => &ORG_WILDCARD_PROMPTS,
            _ => &[],
        }
    }
}
