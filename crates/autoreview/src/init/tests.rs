//! Tests of `init`: the written config loads, and the login follows the user login grammar.

use std::fs;
use std::path::Path;

use autoreview_engine::Error;
use autoreview_engine::config::{BackendKind, WebhookFormat as LoadedFormat};
use clap::ValueEnum;

use super::{
    Example, InitArguments, Layout, Notifier, WebhookFormat, agent_profiles, render, write,
};
use crate::config_file::{self, RUNTIME_DIR_OPTIONAL};

/// The login that the tests pass to `init`.
const LOGIN: &str = "review-bot-2";

/// The pohunek host that the tests pass to `init`.
const HOST: &str = "review-host-2";

/// Each `--notifier` (`None` leaves the flag out) with the backend name and type it writes.
const NOTIFIERS: [(Option<Notifier>, &str, &str); 4] = [
    (None, "matrix", "matrix"),
    (Some(Notifier::Matrix), "matrix", "matrix"),
    (Some(Notifier::None), "quiet", "none"),
    (Some(Notifier::Webhook), "webhook", "webhook"),
];

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

/// Sets `notifier`, with the `json` format that `--notifier webhook` requires.
fn set_notifier(init: &mut InitArguments, notifier: Option<Notifier>) {
    init.layout.notifier = notifier;
    init.layout.webhook_format =
        (notifier == Some(Notifier::Webhook)).then_some(WebhookFormat::Json);
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
    for (example, (notifier, backend, kind)) in Example::value_variants()
        .iter()
        .copied()
        .flat_map(|example| NOTIFIERS.map(|notifier| (example, notifier)))
    {
        let case = format!("{} {notifier:?}", name(example));
        // A space and a quote check that the policy path is quoted in the YAML.
        let directory = root.path().join(format!("{case} \"setup\""));
        let text = directory.to_str().expect("a UTF-8 temporary path");
        let mut init = arguments(&directory, example);
        set_notifier(&mut init, notifier);
        let files = render(&init, text).expect("the files render");
        if notifier == Some(Notifier::Matrix) {
            // `--notifier matrix` writes the reference file, as leaving the flag out does.
            let reference = render(&arguments(&directory, example), text).expect("it renders");
            assert_eq!(files[0].contents, reference[0].contents, "{case}");
        }
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
        .unwrap_or_else(|error| panic!("{case}: {error}"));
        let policy = loaded.policy.as_ref().expect("the config names a policy");
        assert_eq!(policy.agent_profiles(), profiles, "{case}");
        assert!(!profiles.is_empty(), "{case}");
        let notify = &loaded.notify;
        let backends: Vec<(&str, &str)> = notify
            .backends
            .iter()
            .map(|entry| {
                let entry_kind = match entry.kind {
                    BackendKind::None => "none",
                    BackendKind::Matrix(_) => "matrix",
                    BackendKind::Webhook(_) => "webhook",
                };
                (entry.name.as_str(), entry_kind)
            })
            .collect();
        assert_eq!(backends, [(backend, kind)], "{case}");
        for route in [&notify.action, &notify.digest, &notify.log, &notify.queue] {
            assert_eq!(route, &[backend], "{case}");
        }
        let served = loaded.credentials.single().map(|only| only.login.as_str());
        assert_eq!(served, Some(LOGIN));
        assert_eq!(loaded.pohunek_hosts, [HOST]);
        // Without `--pohunek-runtime-dir`, macOS leaves the Linux reference value out (#513).
        let runtime = (!RUNTIME_DIR_OPTIONAL).then_some("/run/autoreview");
        assert_eq!(
            loaded.pohunek_runtime.as_deref(),
            runtime,
            "{}",
            name(example)
        );
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

#[test]
fn init_matrix_flags_need_the_matrix_notifier() {
    let directory = Path::new("/srv/autoreview");
    let text = "/srv/autoreview";
    for (flag, value) in [
        ("--matrix-homeserver-url", "https://chat.example.net"),
        ("--matrix-room", "!other:example.net"),
        ("--matrix-mention-user", "@other:example.net"),
    ] {
        for notifier in NOTIFIERS.map(|(notifier, ..)| notifier) {
            let mut init = arguments(directory, Example::SingleRepoComment);
            set_notifier(&mut init, notifier);
            let given = Some(value.to_owned());
            match flag {
                "--matrix-homeserver-url" => init.layout.matrix_homeserver_url = given,
                "--matrix-room" => init.layout.matrix_room = given,
                _ => init.layout.matrix_mention_user = given,
            }
            let case = format!("{flag} {notifier:?}");
            match notifier {
                None | Some(Notifier::Matrix) => {
                    let files = render(&init, text).expect("a Matrix flag renders");
                    let config = String::from_utf8(files[0].contents.clone()).expect("UTF-8");
                    assert!(config.contains(&format!(": \"{value}\"\n")), "{case}");
                }
                Some(Notifier::None | Notifier::Webhook) => {
                    let error = render(&init, text).expect_err("the Matrix flag is refused");
                    assert_eq!(
                        config_error(&error),
                        ("configuration_invalid", flag),
                        "{case}"
                    );
                }
            }
        }
    }
}

#[test]
fn init_webhook_format_is_required_by_the_webhook_notifier_only() {
    let root = tempfile::tempdir().expect("a temporary directory");
    for (format, loaded) in [
        (WebhookFormat::Json, LoadedFormat::Json),
        (WebhookFormat::Slack, LoadedFormat::Slack),
        (WebhookFormat::Discord, LoadedFormat::Discord),
    ] {
        let directory = root.path().join(format.name());
        let text = directory.to_str().expect("a UTF-8 temporary path");
        let mut init = arguments(&directory, Example::SingleRepoComment);
        init.layout.notifier = Some(Notifier::Webhook);
        init.layout.webhook_format = Some(format);
        let files = render(&init, text).expect("a webhook format renders");
        write(&directory, &files, false).expect("init writes into a new directory");
        let config = fs::read_to_string(directory.join("config.yaml")).expect("config.yaml");
        let config = config_file::parse(&config, config_file::tests::secret, |path| {
            autoreview_policy::load(path).map_err(|error| Box::new(error) as _)
        })
        .unwrap_or_else(|error| panic!("{format:?}: {error}"));
        let written: Vec<Option<LoadedFormat>> = config
            .notify
            .backends
            .iter()
            .map(|backend| match &backend.kind {
                BackendKind::Webhook(webhook) => Some(webhook.format),
                BackendKind::None | BackendKind::Matrix(_) => None,
            })
            .collect();
        assert_eq!(written, [Some(loaded)], "{format:?}");
        for notifier in [None, Some(Notifier::Matrix), Some(Notifier::None)] {
            init.layout.notifier = notifier;
            let error = render(&init, text).expect_err("the format is refused");
            let named = config_error(&error);
            assert_eq!(
                named,
                ("configuration_invalid", "--webhook-format"),
                "{notifier:?}"
            );
        }
    }
    let mut init = arguments(root.path(), Example::SingleRepoComment);
    init.layout.notifier = Some(Notifier::Webhook);
    let text = root.path().to_str().expect("a UTF-8 temporary path");
    let error = render(&init, text).expect_err("the webhook notifier needs a format");
    assert_eq!(
        config_error(&error),
        ("configuration_invalid", "--webhook-format")
    );
}
