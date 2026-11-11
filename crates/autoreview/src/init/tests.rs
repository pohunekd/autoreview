//! Tests of `init`: the embedded files, the written config and the refusal to overwrite.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use autoreview_engine::{Error, error_code};
use clap::ValueEnum;

use super::{
    Example, INIT_FILE_EXISTS, INIT_WRITE_FAILED, InitArguments, Layout, SetupFile, agent_profiles,
    render, render_config, write,
};
use crate::config_file;

/// The configuration reference, whose "Complete file" block `init` writes.
const REFERENCE: &str = include_str!("../../../../docs/configuration.md");

/// The login that the tests pass to `init`.
const LOGIN: &str = "review-bot-2";

/// The pohunek host that the tests pass to `init`.
const HOST: &str = "review-host-2";

/// Returns the repository directory that holds `examples/` and `prompts/`.
fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Returns the arguments of `init` for `example` into `directory`.
fn arguments(directory: &Path, example: Example) -> InitArguments {
    InitArguments {
        dir: directory.to_path_buf(),
        login: LOGIN.to_owned(),
        pohunek_host: HOST.to_owned(),
        example,
        layout: Layout::default(),
    }
}

/// Returns the directory name of `example` under `examples/`.
fn name(example: Example) -> String {
    example
        .to_possible_value()
        .expect("every example has a name")
        .get_name()
        .to_owned()
}

/// Returns the YAML of the "Complete file" section of the configuration reference.
fn reference_file() -> &'static str {
    let (_, section) = REFERENCE
        .split_once("## Complete file")
        .expect("the reference has a Complete file section");
    let (_, block) = section
        .split_once("```yaml\n")
        .expect("the section opens a yaml block");
    let (file, _) = block.split_once("```\n").expect("the yaml block is closed");
    file
}

/// Returns the lines of `text` that are neither blank nor YAML comments.
fn without_comments(text: &str) -> Vec<&str> {
    text.lines()
        .filter(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#'))
        .collect()
}

/// Returns the code and the name of a configuration error.
fn config_error(error: &Error) -> (&str, &str) {
    match error {
        Error::Config { code, name, .. } => (code.as_str(), name.as_str()),
        other => panic!("expected a configuration error, got {other}"),
    }
}

#[test]
fn every_example_and_its_prompts_are_embedded() {
    let examples = repository().join("examples");
    let mut directories: Vec<String> = fs::read_dir(&examples)
        .expect("examples/ is readable")
        .map(|entry| entry.expect("an entry of examples/"))
        .filter(|entry| entry.path().is_dir())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    directories.sort();
    let mut embedded: Vec<String> = Example::value_variants()
        .iter()
        .copied()
        .map(name)
        .collect();
    embedded.sort();
    assert_eq!(embedded, directories);
    for example in Example::value_variants().iter().copied() {
        let directory = examples.join(name(example));
        let mut on_disk = BTreeSet::new();
        if let Ok(entries) = fs::read_dir(directory.join("prompts")) {
            for entry in entries {
                let entry = entry.expect("an entry of prompts/");
                on_disk.insert(format!("prompts/{}", entry.file_name().to_string_lossy()));
            }
        }
        let extra: BTreeSet<String> = example
            .extra_prompts()
            .iter()
            .map(|(relative, _)| (*relative).to_owned())
            .collect();
        assert_eq!(extra, on_disk, "{}", name(example));
    }
}

#[test]
fn written_config_is_the_reference_apart_from_policy_login_and_host() {
    let written = render_config("/srv/autoreview", LOGIN, HOST).expect("the template renders");
    let expected = reference_file()
        .replace(
            "policy_file: /etc/autoreview/policy.yaml\n",
            "policy_file: \"/srv/autoreview/policy.yaml\"\n",
        )
        .replace(
            "      login: review-bot\n",
            "      login: \"review-bot-2\"\n",
        )
        .replace("  hosts: [review-host]\n", "  hosts: [\"review-host-2\"]\n");
    assert_eq!(without_comments(&written), without_comments(&expected));
}

#[test]
fn init_then_validate_loads_every_example() {
    let root = tempfile::tempdir().expect("a temporary directory");
    for example in Example::value_variants().iter().copied() {
        // A space and a quote check that the policy path is quoted in the YAML.
        let directory = root.path().join(format!("{} \"setup\"", name(example)));
        let text = directory.to_str().expect("a UTF-8 temporary path");
        let files = render(&arguments(&directory, example), text).expect("the files render");
        let profiles = agent_profiles(&files).expect("the embedded policy loads");
        let written = write(&directory, &files).expect("init writes into a new directory");
        let relative: Vec<&str> = files.iter().map(|file| file.relative).collect();
        let expected: Vec<String> = relative
            .iter()
            .map(|path| directory.join(path).to_string_lossy().into_owned())
            .collect();
        assert_eq!(written, expected);
        let config = fs::read_to_string(directory.join("config.yaml")).expect("config.yaml");
        let loaded = config_file::parse(&config, config_file::tests::secret, |path| {
            autoreview_policy::load(path).map_err(|error| Box::new(error) as _)
        })
        .unwrap_or_else(|error| panic!("{}: {error}", name(example)));
        let policy = loaded.policy.as_ref().expect("the config names a policy");
        assert_eq!(policy.agent_profiles(), profiles, "{}", name(example));
        assert!(!profiles.is_empty(), "{}", name(example));
        assert_eq!(loaded.github_login, LOGIN);
        assert_eq!(loaded.pohunek_hosts, [HOST]);
    }
}

#[test]
fn init_names_an_existing_file_and_changes_nothing() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let directory = root.path().join("setup");
    let prompts = directory.join("prompts");
    fs::create_dir_all(&prompts).expect("prompts/ is created");
    fs::write(prompts.join("verifier.md"), "kept\n").expect("the existing prompt is written");
    let text = directory.to_str().expect("a UTF-8 temporary path");
    let init = arguments(&directory, Example::SingleRepoComment);
    let files: Vec<SetupFile> = render(&init, text).expect("the files render");
    let error = write(&directory, &files).expect_err("an existing file is refused");
    let path = prompts.join("verifier.md");
    assert_eq!(
        config_error(&error),
        (INIT_FILE_EXISTS, path.to_str().expect("a UTF-8 path"))
    );
    assert_eq!(error_code(&error), INIT_FILE_EXISTS);
    let mut left: Vec<PathBuf> = walk(&directory);
    left.sort();
    assert_eq!(left, [prompts.clone(), path.clone()]);
    assert_eq!(fs::read_to_string(&path).expect("the prompt"), "kept\n");
}

#[test]
fn init_refuses_a_symlinked_prompts_directory() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let directory = root.path().join("setup");
    let elsewhere = root.path().join("elsewhere");
    fs::create_dir_all(&directory).expect("the directory is created");
    fs::create_dir_all(&elsewhere).expect("the link target is created");
    let prompts = directory.join("prompts");
    std::os::unix::fs::symlink(&elsewhere, &prompts).expect("prompts/ is a symlink");
    let text = directory.to_str().expect("a UTF-8 temporary path");
    let init = arguments(&directory, Example::SingleRepoComment);
    let files = render(&init, text).expect("the files render");
    let error = write(&directory, &files).expect_err("a symlinked prompts/ is refused");
    assert_eq!(
        config_error(&error),
        (INIT_FILE_EXISTS, prompts.to_str().expect("a UTF-8 path"))
    );
    assert_eq!(walk(&directory), [prompts]);
    assert!(walk(&elsewhere).is_empty());
}

#[test]
fn a_failed_write_removes_what_init_created() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().expect("a temporary directory");
    let directory = root.path().join("setup");
    let prompts = directory.join("prompts");
    fs::create_dir_all(&prompts).expect("prompts/ is created");
    fs::set_permissions(&prompts, fs::Permissions::from_mode(0o555)).expect("read-only");
    // A process that may write anyway (root) cannot run this scenario.
    let probe = prompts.join("probe");
    if fs::write(&probe, "").is_ok() {
        fs::remove_file(&probe).expect("the probe is removed");
        return;
    }
    let text = directory.to_str().expect("a UTF-8 temporary path");
    let init = arguments(&directory, Example::SingleRepoComment);
    let files = render(&init, text).expect("the files render");
    let error = write(&directory, &files).expect_err("a read-only prompts/ fails the write");
    let generic = prompts.join("generic.md");
    assert_eq!(
        config_error(&error),
        (INIT_WRITE_FAILED, generic.to_str().expect("a UTF-8 path"))
    );
    let left = walk(&directory);
    fs::set_permissions(&prompts, fs::Permissions::from_mode(0o755)).expect("writable again");
    assert_eq!(left, [prompts]);
}

/// Returns every path below `directory`, depth first.
fn walk(directory: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(directory).expect("a readable directory") {
        let path = entry.expect("an entry").path();
        if path.is_dir() && !path.is_symlink() {
            paths.extend(walk(&path));
        }
        paths.push(path);
    }
    paths
}
