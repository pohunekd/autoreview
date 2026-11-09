//! Tests of `init`: the embedded files, the written config and the refusal to overwrite.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use autoreview_engine::{Error, error_code};
use clap::ValueEnum;

use super::{
    Example, INIT_FILE_EXISTS, InitArguments, SetupFile, agent_profiles, render, render_config,
    write,
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
        let policy = fs::read(directory.join("policy.yaml")).expect("the policy is readable");
        assert_eq!(example.policy(), policy.as_slice(), "{}", name(example));
        let mut on_disk = BTreeMap::new();
        if let Ok(entries) = fs::read_dir(directory.join("prompts")) {
            for entry in entries {
                let entry = entry.expect("an entry of prompts/");
                let relative = format!("prompts/{}", entry.file_name().to_string_lossy());
                on_disk.insert(relative, fs::read(entry.path()).expect("a readable prompt"));
            }
        }
        let extra: BTreeMap<String, Vec<u8>> = example
            .extra_prompts()
            .iter()
            .map(|(relative, contents)| ((*relative).to_owned(), contents.to_vec()))
            .collect();
        assert_eq!(extra, on_disk, "{}", name(example));
    }
    for (relative, contents) in super::DEFAULT_PROMPTS {
        let on_disk = fs::read(repository().join(relative)).expect("a default prompt");
        assert_eq!(contents, on_disk.as_slice(), "{relative}");
    }
}
