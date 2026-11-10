//! The files of a reviewer profile: the embedded adapter, its path file, the profile text.
//!
//! The profile text matches what `adapters/<agent>/render_profile.py` writes from a base profile
//! with the same pin, the argument `--permission-mode plan` (Claude) and no `[env]` entries; the
//! shared golden files under `adapters/<agent>/tests/` hold that text for both renderers.

use std::path::{Path, PathBuf};

use autoreview_engine::Error;
use sha2::{Digest, Sha256};

use super::files::{Directory, Kind, Planned};
use super::{Agent, Pin, Runtime, Settings};
use crate::failed;

/// The Claude adapter that a Claude profile starts, as the repository holds it.
const CLAUDE_ADAPTER: &[u8] = include_bytes!("../../../../adapters/claude/claude-adapter");

/// The Codex adapter that a Codex profile starts, as the repository holds it.
const CODEX_ADAPTER: &[u8] = include_bytes!("../../../../adapters/codex/codex-adapter");

/// The first line of every profile and manifest this command writes, before the body digest.
pub(super) const MARKER_PREFIX: &str = "# Written by autoreview profiles install; sha256=";

/// The profile keys that pohunek rewrites when it pins or repins a package.
const PIN_KEYS: [&str; 2] = ["package", "digest"];

/// The detection manifest of a Claude profile: no rules, because the adapter renders no prompt.
///
/// It equals `MANIFEST_CONTENT` of `adapters/claude/render_profile.py`.
pub(super) const MANIFEST_BODY: &str = "# An empty rule list intentionally disables detection: \
the wrapper never renders a\n# prompt box, so the initial input must not wait for one.\n\
rules = []\n";

/// The permission mode of every Claude review run: plan mode never edits a file.
const PERMISSION_MODE: [&str; 2] = ["--permission-mode", "plan"];

/// Delay before the Claude adapter's submit key, as `adapters/claude/render_profile.py` sets it.
const CLAUDE_SUBMIT_DELAY_MS: u32 = 0;

/// Delay before the Codex adapter's submit key, as `adapters/codex/render_profile.py` sets it.
const CODEX_SUBMIT_DELAY_MS: u32 = 150;

/// Mode of an installed adapter: only the owner may read, run or change it.
const ADAPTER_MODE: u32 = 0o700;

/// Mode of every other written file: pohunek and the adapters refuse a writable one.
const FILE_MODE: u32 = 0o600;

/// Everything `profiles install` writes for one profile, in the order it is written.
#[derive(Debug)]
pub(super) struct Plan {
    /// The directories to create when they are absent, parents first.
    pub(super) directories: Vec<Directory>,
    /// The files to write; the profile is the last one.
    pub(super) files: Vec<Planned>,
    /// The absolute path of the profile file.
    pub(super) profile: PathBuf,
    /// The absolute path of the adapter that the profile starts.
    pub(super) program: String,
    /// The hex SHA-256 of the adapter.
    pub(super) adapter_sha256: String,
}

/// Returns the plan of profile `name` over `pin`, with the adapter in `adapters`.
///
/// # Errors
///
/// Returns `workflow_failed` when a value cannot be quoted or a path is not UTF-8.
pub(super) fn plan(
    name: &str,
    pin: &Pin,
    settings: &Settings,
    agents: &Path,
    adapters: &Path,
) -> Result<Plan, Error> {
    let agent = settings.runtime.agent();
    let (file_name, adapter) = match agent {
        Agent::Claude => ("claude-adapter", CLAUDE_ADAPTER),
        Agent::Codex => ("codex-adapter", CODEX_ADAPTER),
    };
    let program_path = adapters.join(file_name);
    let program = program_path
        .to_str()
        .ok_or_else(|| failed("the adapter path is not UTF-8"))?
        .to_owned();
    // The adapter reads the agent path from `<its own path>.<agent>-binary`.
    let sidecar = adapters.join(format!("{file_name}.{}-binary", agent.base()));
    let mut directories = vec![
        Directory::new(adapters.to_path_buf(), true),
        Directory::new(agents.to_path_buf(), false),
    ];
    let mut files = vec![
        Planned::new(program_path, adapter.to_vec(), ADAPTER_MODE, Kind::Owned),
        Planned::new(
            sidecar,
            format!("{}\n", settings.agent_binary).into_bytes(),
            FILE_MODE,
            Kind::Owned,
        ),
    ];
    let manifest = (agent == Agent::Claude).then_some(name);
    if manifest.is_some() {
        let manifests = agents.join("manifests");
        directories.push(Directory::new(manifests.clone(), false));
        let text = with_marker(MANIFEST_BODY);
        let path = manifests.join(format!("{name}.toml"));
        files.push(Planned::new(
            path,
            text.into_bytes(),
            FILE_MODE,
            Kind::Marked,
        ));
    }
    let profile = render_profile(pin, settings, &program, manifest)?;
    let path = agents.join(format!("{name}.toml"));
    files.push(Planned::new(
        path.clone(),
        profile.into_bytes(),
        FILE_MODE,
        Kind::Marked,
    ));
    Ok(Plan {
        directories,
        files,
        profile: path,
        program,
        adapter_sha256: hex::encode(Sha256::digest(adapter)),
    })
}

/// Renders the profile over `pin` that starts the adapter `program` with `settings`.
///
/// A Claude profile also names its detection manifest `manifest`. The text is the marker line,
/// then the body.
///
/// # Errors
///
/// Returns `workflow_failed` when a value cannot be quoted.
pub(super) fn render_profile(
    pin: &Pin,
    settings: &Settings,
    program: &str,
    manifest: Option<&str>,
) -> Result<String, Error> {
    let model = settings.model.as_str();
    let effort = settings.effort.as_str();
    let mut arguments = vec![
        settings.agent_binary.clone(),
        settings.maximum_input_bytes.to_string(),
    ];
    let submit_delay_ms = match settings.runtime {
        Runtime::Claude => {
            arguments.extend(PERMISSION_MODE.map(str::to_owned));
            arguments.extend(["--model", model, "--effort", effort].map(str::to_owned));
            CLAUDE_SUBMIT_DELAY_MS
        }
        Runtime::Codex {
            maximum_result_bytes,
            maximum_retention_seconds,
        } => {
            arguments.push(maximum_result_bytes.to_string());
            arguments.push(maximum_retention_seconds.to_string());
            arguments.extend([model, effort].map(str::to_owned));
            CODEX_SUBMIT_DELAY_MS
        }
    };
    let mut body = format!(
        "base = {}\npackage = {}\ndigest = {}\n",
        quoted(settings.runtime.agent().base())?,
        quoted(&pin.package)?,
        quoted(&pin.digest)?
    );
    if let Some(manifest) = manifest {
        body.push_str(&format!("manifest = {}\n", quoted(manifest)?));
    }
    let quoted_arguments = arguments
        .iter()
        .map(|argument| quoted(argument))
        .collect::<Result<Vec<_>, _>>()?;
    body.push_str(&format!(
        "program = {}\nargs = [{}]\n\n[input_rules]\nbracketed_paste = true\n\
submit_delay_ms = {submit_delay_ms}\n\n[resume]\nresumable = false\n\n[env]\n",
        quoted(program)?,
        quoted_arguments.join(", ")
    ));
    Ok(with_marker(&body))
}

/// Returns `body` under the marker line that holds its digest.
fn with_marker(body: &str) -> String {
    format!("{MARKER_PREFIX}{}\n{body}", body_digest(body))
}

/// Returns `value` as a TOML basic string; JSON string escapes are valid TOML escapes.
///
/// The callers pass no control character, which JSON would leave as it is in one case (DEL).
fn quoted(value: &str) -> Result<String, Error> {
    serde_json::to_string(value).map_err(failed)
}

/// Returns the hex SHA-256 of `body` without its top-level `package` and `digest` lines.
///
/// pohunek rewrites exactly those two keys when it pins or repins a package (`plugin profile
/// migrate`, `service upgrade`), so such a rewrite keeps the file ours.
pub(super) fn body_digest(body: &str) -> String {
    let mut hasher = Sha256::new();
    let mut top_level = true;
    for line in body.split_inclusive('\n') {
        if line.trim_start().starts_with('[') {
            top_level = false;
        }
        let pin_line = top_level
            && line
                .split_once('=')
                .is_some_and(|(key, _)| PIN_KEYS.contains(&key.trim()));
        if !pin_line {
            hasher.update(line.as_bytes());
        }
    }
    hex::encode(hasher.finalize())
}

/// Reports whether `text` starts with the marker and the marker's digest matches the body.
pub(super) fn written_by_us(text: &str) -> bool {
    let Some(rest) = text.strip_prefix(MARKER_PREFIX) else {
        return false;
    };
    let Some((digest, body)) = rest.split_once('\n') else {
        return false;
    };
    digest == body_digest(body)
}
