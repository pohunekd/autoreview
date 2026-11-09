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

/// The arguments of `init`; every one is required.
#[derive(Debug, Args)]
pub(crate) struct InitArguments {
    /// Absolute directory that receives config.yaml, policy.yaml and prompts/.
    #[arg(long, value_name = "DIR")]
    pub(crate) dir: PathBuf,
    /// GitHub login of the one credential, in lowercase.
    #[arg(long, value_name = "LOGIN")]
    pub(crate) login: String,
    /// Name of the pohunek host that runs the reviewers.
    #[arg(long, value_name = "HOST")]
    pub(crate) pohunek_host: String,
    /// The example whose policy and prompts are written.
    #[arg(long, value_enum, value_name = "NAME")]
    pub(crate) example: Example,
}

/// What `init` prints after it wrote every file.
#[derive(Debug, Serialize)]
pub(crate) struct InitResult {
    /// The directory, as given.
    directory: String,
    /// The absolute paths of the written files, in the order they were written.
    files: Vec<String>,
    /// The pohunek agent profiles that the policy's reviewers run, as `validate` prints them.
    agent_profiles: Vec<String>,
}

/// One file that `init` writes, relative to the directory.
#[derive(Debug)]
pub(crate) struct SetupFile {
    /// Path relative to the directory, for example `prompts/generic.md`.
    pub(crate) relative: &'static str,
    /// The bytes of the file.
    pub(crate) contents: Vec<u8>,
}

/// Writes the configuration directory and returns what it wrote.
///
/// # Errors
///
/// Returns `command_invalid` when `--config` is given, `configuration_invalid` naming a flag
/// or `policy_file`, `init_file_exists` naming the first path in the way, and
/// `workflow_failed` when a file cannot be written.
pub(crate) fn run(
    config_path: Option<&Path>,
    arguments: &InitArguments,
) -> Result<InitResult, Error> {
    if config_path.is_some() {
        return Err(command_invalid(Some(
            "--config is not an option of init; init writes into --dir".into(),
        )));
    }
    let directory = directory(&arguments.dir)?;
    let files = render(arguments, directory)?;
    let agent_profiles = agent_profiles(&files)?;
    let written = write(Path::new(directory), &files)?;
    Ok(InitResult {
        directory: directory.to_owned(),
        files: written,
        agent_profiles,
    })
}

/// Returns `dir` as text when it is a clean absolute UTF-8 path.
///
/// # Errors
///
/// Returns `configuration_invalid` naming `--dir`.
fn directory(dir: &Path) -> Result<&str, Error> {
    match dir.to_str() {
        Some(text) if clean_absolute(text) => Ok(text),
        _ => Err(invalid_flag("--dir", "must be a clean absolute UTF-8 path")),
    }
}

/// Renders every file of the example in the order `init` writes them.
///
/// # Errors
///
/// Returns `configuration_invalid` naming `--login` or `--pohunek-host`, and the errors of
/// [`render_config`].
pub(crate) fn render(arguments: &InitArguments, directory: &str) -> Result<Vec<SetupFile>, Error> {
    if !github_login_valid(&arguments.login) {
        return Err(invalid_flag("--login", "is not a lowercase GitHub login"));
    }
    if !identifier_valid(&arguments.pohunek_host) {
        return Err(invalid_flag(
            "--pohunek-host",
            "must match [A-Za-z0-9][A-Za-z0-9._-]*",
        ));
    }
    let config = render_config(directory, &arguments.login, &arguments.pohunek_host)?;
    let mut files = vec![
        SetupFile {
            relative: CONFIG_NAME,
            contents: config.into_bytes(),
        },
        SetupFile {
            relative: POLICY_NAME,
            contents: arguments.example.policy().to_vec(),
        },
    ];
    for (relative, contents) in DEFAULT_PROMPTS
        .iter()
        .chain(arguments.example.extra_prompts())
    {
        files.push(SetupFile {
            relative,
            contents: contents.to_vec(),
        });
    }
    Ok(files)
}
