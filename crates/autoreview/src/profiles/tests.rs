//! Tests of `profiles install`: the rendered files, the flags and the decision on existing files.

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use autoreview_engine::{Error, error_code};

use super::files::install;
use super::render::{
    MANIFEST_BODY, MARKER_PREFIX, body_digest, plan, render_profile, written_by_us,
};
use super::{
    Agent, Effort, InstallArguments, Outcome, PROFILE_FOREIGN, PROFILE_WRITE_FAILED, Pin, Runtime,
    Settings, settings,
};
use crate::CONFIGURATION_INVALID;
use crate::error_response::COMMAND_INVALID;

/// The archive digest that the tests and the golden files pin.
const DIGEST: &str = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// A newer archive digest, as `service upgrade` repins to.
const NEWER_DIGEST: &str =
    "sha256:fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";

/// The adapter that a Claude profile starts.
const CLAUDE_ADAPTER: &[u8] = include_bytes!("../../../../adapters/claude/claude-adapter");

/// Returns the pin of the official package of `agent` at `DIGEST`.
fn pin(agent: Agent) -> Pin {
    Pin {
        package: format!("pohunek.runtime.{}", agent.base()),
        digest: DIGEST.to_owned(),
    }
}

/// Returns the settings of the golden Claude profile.
fn claude_settings() -> Settings {
    Settings {
        model: "claude-sonnet-4-5".to_owned(),
        effort: Effort::High,
        agent_binary: "/usr/local/bin/claude".to_owned(),
        maximum_input_bytes: 1_048_576,
        runtime: Runtime::Claude,
    }
}

/// Returns the settings of the golden Codex profile.
fn codex_settings() -> Settings {
    Settings {
        model: "gpt-5-codex".to_owned(),
        effort: Effort::High,
        agent_binary: "/bin/sh".to_owned(),
        maximum_input_bytes: 1_048_576,
        runtime: Runtime::Codex {
            maximum_result_bytes: 1_048_576,
            maximum_retention_seconds: 7200,
        },
    }
}

/// Returns valid Claude arguments whose agent binary is `binary`.
fn claude_arguments(binary: &Path) -> InstallArguments {
    InstallArguments {
        host: "review-host".to_owned(),
        agent: Agent::Claude,
        name: "autoreview-sonnet".to_owned(),
        model: "claude-sonnet-4-5".to_owned(),
        effort: Effort::Max,
        agent_binary: binary.to_path_buf(),
        maximum_input_bytes: 1_048_576,
        maximum_result_bytes: None,
        maximum_retention_seconds: None,
        replace: false,
    }
}

/// Returns the code and the name of a configuration error, or the code of another error.
fn code_and_name(error: &Error) -> (&str, &str) {
    match error {
        Error::Config { name, .. } => (error_code(error), name.as_str()),
        other => (error_code(other), ""),
    }
}

/// Returns the permission bits of `path`.
fn mode(path: &Path) -> u32 {
    fs::metadata(path).expect("metadata").permissions().mode() & 0o777
}

/// Returns the sorted names in `directory`.
fn names(directory: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(directory)
        .expect("directory")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

#[test]
fn rendered_profiles_match_the_golden_files_of_the_adapters() {
    let program = "/home/review/.local/share/autoreview/adapters/autoreview-sonnet/claude-adapter";
    let claude = render_profile(
        &pin(Agent::Claude),
        &claude_settings(),
        program,
        Some("autoreview-sonnet"),
    )
    .expect("claude renders");
    let (marker, body) = claude.split_once('\n').expect("a marker line");
    assert_eq!(marker, format!("{MARKER_PREFIX}{}", body_digest(body)));
    assert_eq!(
        body,
        include_str!("../../../../adapters/claude/tests/installed-profile.toml")
    );
    assert_eq!(
        MANIFEST_BODY,
        include_str!("../../../../adapters/claude/tests/installed-manifest.toml")
    );

    let program = "/home/review/.local/share/autoreview/adapters/autoreview-codex/codex-adapter";
    let codex = render_profile(&pin(Agent::Codex), &codex_settings(), program, None)
        .expect("codex renders");
    let (marker, codex_body) = codex.split_once('\n').expect("a marker line");
    assert_eq!(
        marker,
        format!("{MARKER_PREFIX}{}", body_digest(codex_body))
    );
    assert_eq!(
        codex_body,
        include_str!("../../../../adapters/codex/tests/installed-profile.toml")
    );

    // The digest leaves out the pin lines that pohunek rewrites, and covers every other line.
    assert_eq!(
        body_digest(body),
        body_digest(&body.replace(DIGEST, NEWER_DIGEST))
    );
    assert_ne!(
        body_digest(body),
        body_digest(&body.replace("\"high\"", "\"low\""))
    );
    assert!(written_by_us(&claude) && written_by_us(&codex));
    assert!(!written_by_us(&claude.replace("\"high\"", "\"low\"")));
}

#[test]
fn settings_need_exactly_the_flags_of_the_agent() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let binary = root.path().join("claude");
    fs::write(&binary, "#!/bin/sh\n").expect("binary");
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).expect("chmod");

    let accepted = settings(&claude_arguments(&binary)).expect("claude settings");
    assert_eq!(accepted.runtime, Runtime::Claude);
    assert_eq!(accepted.agent_binary, binary.to_str().expect("utf-8"));

    let mut arguments = claude_arguments(&binary);
    arguments.maximum_retention_seconds = Some(7200);
    let error = settings(&arguments).expect_err("a Codex flag on Claude");
    assert_eq!(code_and_name(&error).0, COMMAND_INVALID);

    let mut arguments = claude_arguments(&binary);
    arguments.agent = Agent::Codex;
    arguments.effort = Effort::High;
    arguments.maximum_result_bytes = Some(1_048_576);
    let error = settings(&arguments).expect_err("Codex without a retention time");
    assert_eq!(code_and_name(&error).0, COMMAND_INVALID);

    arguments.maximum_retention_seconds = Some(7200);
    let codex = settings(&arguments).expect("codex settings");
    assert_eq!(codex.runtime.agent(), Agent::Codex);
    arguments.effort = Effort::Max;
    let error = settings(&arguments).expect_err("Codex has no max effort");
    assert_eq!(code_and_name(&error), (CONFIGURATION_INVALID, "--effort"));

    let mut arguments = claude_arguments(&binary);
    arguments.model = "sonnet 4".to_owned();
    let error = settings(&arguments).expect_err("a model with a space");
    assert_eq!(code_and_name(&error), (CONFIGURATION_INVALID, "--model"));

    let mut arguments = claude_arguments(&binary);
    arguments.agent_binary = PathBuf::from("bin/claude");
    let error = settings(&arguments).expect_err("a relative binary");
    assert_eq!(
        code_and_name(&error),
        (CONFIGURATION_INVALID, "--agent-binary")
    );
}

#[test]
fn install_decides_created_unchanged_updated_foreign_and_replaced() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let config = root.path().join("config");
    fs::create_dir_all(config.join("pohunek")).expect("the pohunek config directory");
    let agents = config.join("pohunek").join("agents");
    let adapters = root.path().join("data/autoreview/adapters");
    let sonnet = adapters.join("autoreview-sonnet");
    let name = "autoreview-sonnet";
    let claude = plan(
        name,
        &pin(Agent::Claude),
        &claude_settings(),
        &agents,
        &sonnet,
    )
    .expect("a plan");
    let profile = agents.join("autoreview-sonnet.toml");
    let manifest = agents.join("manifests").join("autoreview-sonnet.toml");
    let adapter = sonnet.join("claude-adapter");
    let sidecar = sonnet.join("claude-adapter.claude-binary");
    assert_eq!(claude.profile, profile);
    assert_eq!(claude.program, adapter.to_str().expect("utf-8"));
    let rendered = render_profile(
        &pin(Agent::Claude),
        &claude_settings(),
        &claude.program,
        Some(name),
    )
    .expect("renders");

    assert_eq!(install(&claude, false).expect("created"), Outcome::Created);
    assert_eq!(fs::read_to_string(&profile).expect("profile"), rendered);
    assert!(written_by_us(
        &fs::read_to_string(&manifest).expect("manifest")
    ));
    assert_eq!(fs::read(&adapter).expect("adapter"), CLAUDE_ADAPTER);
    assert_eq!(
        fs::read_to_string(&sidecar).expect("sidecar"),
        "/usr/local/bin/claude\n"
    );
    let modes = [&agents, &agents.join("manifests"), &sonnet, &adapter]
        .map(|path| mode(path))
        .into_iter()
        .chain([&profile, &manifest, &sidecar].map(|path| mode(path)))
        .collect::<Vec<_>>();
    assert_eq!(modes, [0o700, 0o700, 0o700, 0o700, 0o600, 0o600, 0o600]);

    assert_eq!(
        install(&claude, false).expect("unchanged"),
        Outcome::Unchanged
    );

    // pohunek repins the package of a profile by rewriting only its digest line.
    fs::write(&profile, rendered.replace(DIGEST, NEWER_DIGEST)).expect("repin");
    assert_eq!(install(&claude, false).expect("repinned"), Outcome::Updated);
    assert_eq!(fs::read_to_string(&profile).expect("profile"), rendered);

    // An older adapter and a widened path file are rewritten by the next run.
    fs::write(&adapter, "#!/bin/sh\n").expect("an older adapter");
    fs::set_permissions(&sidecar, fs::Permissions::from_mode(0o666)).expect("chmod");
    assert_eq!(
        install(&claude, false).expect("older adapter"),
        Outcome::Updated
    );
    assert_eq!(fs::read(&adapter).expect("adapter"), CLAUDE_ADAPTER);
    assert_eq!(mode(&sidecar), 0o600);

    // A failed profile write puts back what the run wrote before it, and removes what it created.
    fs::set_permissions(&sidecar, fs::Permissions::from_mode(0o644)).expect("chmod");
    fs::set_permissions(&agents, fs::Permissions::from_mode(0o500)).expect("read-only");
    let probe = agents.join("probe");
    // A process that may write anyway (root) cannot run this scenario.
    if fs::write(&probe, "").is_ok() {
        fs::remove_file(&probe).expect("the probe is removed");
    } else {
        let mut moved = claude_settings();
        moved.agent_binary = "/opt/claude/bin/claude".to_owned();
        let moved = plan(name, &pin(Agent::Claude), &moved, &agents, &sonnet).expect("a plan");
        let error = install(&moved, false).expect_err("a read-only agents directory");
        assert_eq!(code_and_name(&error).0, PROFILE_WRITE_FAILED);
        assert_eq!(
            fs::read_to_string(&sidecar).expect("sidecar"),
            "/usr/local/bin/claude\n"
        );
        assert_eq!(mode(&sidecar), 0o644);
        assert_eq!(
            names(&sonnet),
            ["claude-adapter", "claude-adapter.claude-binary"]
        );
        assert_eq!(code_and_name(&error).1, profile.to_str().expect("utf-8"));

        let other_adapters = adapters.join("autoreview-other");
        let other = plan(
            "autoreview-other",
            &pin(Agent::Claude),
            &claude_settings(),
            &agents,
            &other_adapters,
        )
        .expect("a plan");
        let error = install(&other, false).expect_err("a read-only agents directory");
        assert_eq!(code_and_name(&error).0, PROFILE_WRITE_FAILED);
        assert!(!other_adapters.exists());
        assert_eq!(names(&agents.join("manifests")), ["autoreview-sonnet.toml"]);
    }
    fs::set_permissions(&agents, fs::Permissions::from_mode(0o700)).expect("writable again");
    install(&claude, false).expect("the path file is repaired");
    assert_eq!(mode(&sidecar), 0o600);

    // A refused profile leaves every file as it was, the adapter too.
    let edited = format!("{rendered}# edited\n");
    fs::write(&profile, &edited).expect("edit");
    fs::write(&adapter, "#!/bin/sh\n").expect("an older adapter");
    let error = install(&claude, false).expect_err("an edited profile");
    assert_eq!(
        code_and_name(&error),
        (PROFILE_FOREIGN, profile.to_str().expect("utf-8"))
    );
    assert_eq!(fs::read_to_string(&profile).expect("kept"), edited);
    assert_eq!(fs::read(&adapter).expect("kept"), b"#!/bin/sh\n");
    assert_eq!(install(&claude, true).expect("replaced"), Outcome::Replaced);
    assert_eq!(fs::read_to_string(&profile).expect("profile"), rendered);
    assert_eq!(fs::read(&adapter).expect("adapter"), CLAUDE_ADAPTER);

    // A manifest without the marker is foreign too.
    fs::write(&manifest, "rules = []\n").expect("a hand-written manifest");
    let error = install(&claude, false).expect_err("a foreign manifest");
    assert_eq!(
        code_and_name(&error),
        (PROFILE_FOREIGN, manifest.to_str().expect("utf-8"))
    );
    assert_eq!(install(&claude, true).expect("replaced"), Outcome::Replaced);

    // A group-writable directory is refused before any file is written.
    let manifests = agents.join("manifests");
    fs::set_permissions(&manifests, fs::Permissions::from_mode(0o770)).expect("chmod");
    fs::write(&profile, "base = \"claude\"\n").expect("a hand-written profile");
    let error = install(&claude, true).expect_err("a shared directory");
    assert_eq!(
        code_and_name(&error),
        (PROFILE_WRITE_FAILED, manifests.to_str().expect("utf-8"))
    );
    assert_eq!(
        fs::read_to_string(&profile).expect("kept"),
        "base = \"claude\"\n"
    );
    fs::set_permissions(&manifests, fs::Permissions::from_mode(0o700)).expect("chmod");
    let error = install(&claude, false).expect_err("no marker");
    assert_eq!(code_and_name(&error).0, PROFILE_FOREIGN);

    // A link is refused even with --replace, and nothing of its plan is written.
    let linked_adapters = adapters.join("linked");
    let linked = plan(
        "linked",
        &pin(Agent::Claude),
        &claude_settings(),
        &agents,
        &linked_adapters,
    )
    .expect("a plan");
    symlink(&profile, agents.join("linked.toml")).expect("symlink");
    let error = install(&linked, true).expect_err("a link is refused");
    assert_eq!(code_and_name(&error).0, PROFILE_FOREIGN);
    assert!(!linked_adapters.exists());
    assert_eq!(
        fs::read_to_string(&profile).expect("target kept"),
        "base = \"claude\"\n"
    );

    // A Codex profile has no manifest; its path file is named after the Codex adapter.
    let codex_adapters = adapters.join("autoreview-codex");
    let codex = plan(
        "autoreview-codex",
        &pin(Agent::Codex),
        &codex_settings(),
        &agents,
        &codex_adapters,
    )
    .expect("a plan");
    assert_eq!(install(&codex, false).expect("created"), Outcome::Created);
    assert_eq!(
        install(&codex, false).expect("unchanged"),
        Outcome::Unchanged
    );

    assert_eq!(
        names(&agents),
        [
            "autoreview-codex.toml",
            "autoreview-sonnet.toml",
            "linked.toml",
            "manifests"
        ],
        "no temporary file left"
    );
    assert_eq!(names(&manifests), ["autoreview-sonnet.toml"]);
    assert_eq!(
        names(&sonnet),
        ["claude-adapter", "claude-adapter.claude-binary"]
    );
    assert_eq!(
        names(&codex_adapters),
        ["codex-adapter", "codex-adapter.codex-binary"]
    );
}
