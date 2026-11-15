//! Tests of `init`: the written config loads, and the login follows the user login grammar.

use std::fs;
use std::path::Path;

use autoreview_engine::Error;
use clap::ValueEnum;

use super::{Example, InitArguments, Layout, agent_profiles, render, write};
use crate::config_file;

/// The login that the tests pass to `init`.
const LOGIN: &str = "review-bot-2";

/// The pohunek host that the tests pass to `init`.
const HOST: &str = "review-host-2";

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

/// Returns the code and the name of a configuration error.
fn config_error(error: &Error) -> (&str, &str) {
    match error {
        Error::Config { code, name, .. } => (code.as_str(), name.as_str()),
        other => panic!("expected a configuration error, got {other}"),
    }
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
        let written = write(&directory, &files, false).expect("init writes into a new directory");
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
        let served = loaded.credentials.single().map(|only| only.login.as_str());
        assert_eq!(served, Some(LOGIN));
        assert_eq!(loaded.pohunek_hosts, [HOST]);
    }
}

#[test]
fn init_login_follows_the_user_login_grammar() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let directory = root.path().join("setup");
    let text = directory.to_str().expect("a UTF-8 temporary path");
    let mut init = arguments(&directory, Example::SingleRepoComment);
    // An Enterprise Managed User login is `handle_shortcode`.
    init.login = "mona-cat_octo".to_owned();
    let files = render(&init, text).expect("a managed user's login renders");
    write(&directory, &files, false).expect("init writes into a new directory");
    let config = fs::read_to_string(directory.join("config.yaml")).expect("config.yaml");
    let loaded = config_file::parse(&config, config_file::tests::secret, |path| {
        autoreview_policy::load(path).map_err(|error| Box::new(error) as _)
    })
    .unwrap_or_else(|error| panic!("{error}"));
    let served = loaded.credentials.single().map(|only| only.login.as_str());
    assert_eq!(served, Some("mona-cat_octo"));
    for login in [
        "",
        "review-bot[bot]",
        "Mona-Cat_octo",
        "mona_cat_octo",
        "a/b",
    ] {
        init.login = login.to_owned();
        let error = render(&init, text).expect_err("the login is refused");
        assert_eq!(
            config_error(&error),
            ("configuration_invalid", "--login"),
            "{login}"
        );
    }
}
