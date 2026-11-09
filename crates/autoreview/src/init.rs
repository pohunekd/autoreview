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

/// Renders `config.yaml`: the reference file with the policy path, the login and the host.
///
/// Each value is written as a JSON string, which YAML reads as a double-quoted scalar.
///
/// # Errors
///
/// Returns `workflow_failed` when the template lacks a line that `init` replaces.
pub(crate) fn render_config(directory: &str, login: &str, host: &str) -> Result<String, Error> {
    let policy_path = format!("{directory}/{POLICY_NAME}");
    let policy_line = format!("policy_file: {}\n", quoted(&policy_path)?);
    let mut text = replace_once(CONFIG_TEMPLATE, POLICY_LINE, &policy_line)?;
    let login_line = format!("      login: {}\n", quoted(login)?);
    text = replace_once(&text, LOGIN_LINE, &login_line)?;
    let hosts_line = format!("  hosts: [{}]\n", quoted(host)?);
    replace_once(&text, HOSTS_LINE, &hosts_line)
}

/// Returns `value` as a JSON string.
fn quoted(value: &str) -> Result<String, Error> {
    go_json(value).map_err(failed)
}

/// Replaces the one occurrence of `old` in `text` with `new`.
///
/// # Errors
///
/// Returns `workflow_failed` when `old` does not occur exactly once.
fn replace_once(text: &str, old: &str, new: &str) -> Result<String, Error> {
    if text.matches(old).count() != 1 {
        return Err(failed(format!(
            "the config template must hold {old:?} exactly once"
        )));
    }
    Ok(text.replacen(old, new, 1))
}

/// Loads the rendered policy with its prompts and returns the agent profiles it needs.
///
/// # Errors
///
/// Returns `configuration_invalid` naming `policy_file` when the policy loader refuses it.
pub(crate) fn agent_profiles(files: &[SetupFile]) -> Result<Vec<String>, Error> {
    let read = |relative: &str| {
        files
            .iter()
            .find(|file| file.relative == relative)
            .map(|file| file.contents.clone())
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
    };
    let policy = read(POLICY_NAME).map_err(failed)?;
    let loaded = autoreview_policy::parse(&policy, read).map_err(|error| {
        Error::config(CONFIGURATION_INVALID, "policy_file", Some(Box::new(error)))
    })?;
    Ok(loaded.agent_profiles())
}

/// Returns the configuration error of a flag value that `init` refuses.
fn invalid_flag(flag: &str, reason: &'static str) -> Error {
    Error::config(CONFIGURATION_INVALID, flag, Some(reason.into()))
}

/// Returns the error that names `path`, which is in the way of `init`.
fn in_the_way(path: &Path) -> Error {
    Error::config(INIT_FILE_EXISTS, path.to_string_lossy(), None)
}

/// Writes `files` into `directory`, creating it and its subdirectories when they are absent.
///
/// Every target is checked before anything is created, and a file is opened only when it
/// does not exist, so an existing file is never changed. When a write fails, the files and
/// the listed directories this call created are removed again; a missing parent of
/// `directory` that `create_dir_all` made stays.
///
/// # Errors
///
/// Returns `init_file_exists` naming the first file, or the first directory that is not a
/// directory, in the way, and `workflow_failed` when a check, a directory or a file fails.
pub(crate) fn write(directory: &Path, files: &[SetupFile]) -> Result<Vec<String>, Error> {
    let targets: Vec<PathBuf> = files
        .iter()
        .map(|file| directory.join(file.relative))
        .collect();
    let mut directories: Vec<PathBuf> = vec![directory.to_path_buf()];
    for target in &targets {
        if let Some(parent) = target.parent()
            && !directories.iter().any(|known| known == parent)
        {
            directories.push(parent.to_path_buf());
        }
    }
    let mut missing = Vec::new();
    for path in &directories {
        match fs::metadata(path) {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => return Err(in_the_way(path)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => missing.push(path.clone()),
            Err(error) => return Err(failed(error)),
        }
    }
    for path in &targets {
        match fs::symlink_metadata(path) {
            Ok(_) => return Err(in_the_way(path)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(failed(error)),
        }
    }
    let mut created = Created::default();
    let result = create_all(&missing, files, &targets, &mut created);
    if result.is_err() {
        created.remove();
    }
    result
}

/// Creates the missing directories, then each file, and records what it created.
fn create_all(
    missing: &[PathBuf],
    files: &[SetupFile],
    targets: &[PathBuf],
    created: &mut Created,
) -> Result<Vec<String>, Error> {
    for path in missing {
        fs::create_dir_all(path).map_err(failed)?;
        created.directories.push(path.clone());
    }
    let mut written = Vec::with_capacity(files.len());
    for (file, path) in files.iter().zip(targets) {
        let mut handle = match OpenOptions::new().write(true).create_new(true).open(path) {
            Ok(handle) => handle,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                return Err(in_the_way(path));
            }
            Err(error) => return Err(failed(error)),
        };
        created.files.push(path.clone());
        handle
            .write_all(&file.contents)
            .and_then(|()| handle.flush())
            .map_err(failed)?;
        written.push(path.to_string_lossy().into_owned());
    }
    Ok(written)
}

/// The files and directories that one `init` call created.
#[derive(Debug, Default)]
struct Created {
    /// Directories, in the order they were created.
    directories: Vec<PathBuf>,
    /// Files, in the order they were created.
    files: Vec<PathBuf>,
}

impl Created {
    /// Removes the files, then the directories, newest first; a failure is only logged.
    fn remove(&self) {
        for path in self.files.iter().rev() {
            if let Err(error) = fs::remove_file(path) {
                tracing::warn!(path = %path.display(), error = %error, "init left a file");
            }
        }
        for path in self.directories.iter().rev() {
            if let Err(error) = fs::remove_dir(path) {
                tracing::warn!(path = %path.display(), error = %error, "init left a directory");
            }
        }
    }
}
