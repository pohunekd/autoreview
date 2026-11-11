//! The JSON object that a failed command prints on stdout.
//!
//! Mirrors `cmd/autoreview/command.go:22-47`.

// Rust guideline compliant 2026-06-26

use std::error::Error as StdError;

use autoreview_engine::model::{HEAD_PREFIX_MIN_LEN, SHA_LEN};
use autoreview_engine::{Error, error_code};
use serde::Serialize;

use crate::init::{INIT_FILE_EXISTS, INIT_WRITE_FAILED};
use crate::profiles::{
    POHUNEK_COMMAND_FAILED, PROFILE_FOREIGN, PROFILE_HOST_NOT_LOCAL, PROFILE_WRITE_FAILED,
    RUNTIME_PACKAGE_MISSING,
};

/// Error code of a command with unknown or malformed arguments.
pub(crate) const COMMAND_INVALID: &str = "command_invalid";

/// Error code of a malformed `HEAD_PREFIX` argument.
pub(crate) const HEAD_PREFIX_INVALID: &str = "head_prefix_invalid";

/// Error code of a malformed `HEAD_SHA` argument.
pub(crate) const HEAD_SHA_INVALID: &str = "head_sha_invalid";

/// Error code of a poll that found the discovery lock held.
const DISCOVERY_BUSY: &str = "discovery_busy";

/// Commands and arguments that the message of `command_invalid` lists.
const USAGE: &str = "usage: autoreview [--config PATH] serve | mcp | validate | poll | \
retry|resolve|approve --repository OWNER/REPO NUMBER HEAD | review --repository OWNER/REPO \
NUMBER | reviews [LIMIT] | candidates | awaiting | review-queue | backup-db PATH; \
autoreview init --dir DIR --login LOGIN --pohunek-host HOST --example NAME [--store-path PATH] \
[--gh-bin PATH] [--pohunek-bin PATH] [--pohunek-state-home PATH] [--pohunek-runtime-dir PATH] \
[--matrix-homeserver-url URL] [--matrix-room ROOM] [--matrix-mention-user USER]; \
autoreview profiles install --host HOST --agent claude|codex --name NAME --model MODEL \
--effort EFFORT --agent-binary PATH --maximum-input-bytes BYTES [--maximum-result-bytes BYTES \
--maximum-retention-seconds SECONDS] [--replace]";

/// The JSON a caller reads from a failed command. It carries stable codes, never raw causes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ErrorResponse {
    /// Stable error code.
    error: String,
    /// Message for a human; it names the code or the configuration key.
    message: String,
    /// Configuration key of a configuration error, or empty.
    #[serde(skip_serializing_if = "String::is_empty")]
    configuration: String,
    /// Whether repeating the same command shortly can succeed.
    retryable: bool,
    /// Whether the caller should wake its agent; no command sets it.
    #[serde(rename = "wakeAgent")]
    wake_agent: bool,
}

/// Returns the response for `error`; a configuration error in its chain decides the code.
pub(crate) fn error_response(error: &(dyn StdError + 'static)) -> ErrorResponse {
    let code = error_code(error);
    let mut response = ErrorResponse {
        error: code.to_owned(),
        message: format!("PR review command failed: {code}."),
        configuration: String::new(),
        retryable: false,
        wake_agent: false,
    };
    if code == COMMAND_INVALID {
        response.message = format!("PR review command arguments are invalid. {USAGE}");
    } else if code == DISCOVERY_BUSY {
        response.message =
            "Another discovery pass holds the lock. Repeat the command shortly.".to_owned();
        response.retryable = true;
    } else if code == HEAD_PREFIX_INVALID {
        response.message = format!(
            "HEAD_PREFIX must be {HEAD_PREFIX_MIN_LEN} to {SHA_LEN} lowercase hexadecimal \
characters of the pull request head, as the Review ID of a notification shows."
        );
    } else if code == HEAD_SHA_INVALID {
        response.message = format!(
            "HEAD_SHA must be the full pull request head, {SHA_LEN} lowercase hexadecimal \
characters; autoreview resolve prints it for a head prefix."
        );
    }
    if let Some((config_code, name)) = first_config_error(error) {
        response.error = config_code.to_owned();
        response.configuration = name.to_owned();
        response.message = match config_code {
            INIT_FILE_EXISTS => format!("{name} is in the way; init never overwrites a file."),
            INIT_WRITE_FAILED => {
                format!("{name} cannot be written; init removed what it had created.")
            }
            PROFILE_HOST_NOT_LOCAL => format!(
                "{name} does not name this machine; run profiles install on the review host."
            ),
            PROFILE_FOREIGN => format!(
                "{name} was not written by profiles install; move it away, or pass --replace \
for a regular file."
            ),
            PROFILE_WRITE_FAILED => format!(
                "{name} cannot be written; profiles install restored the files it had \
rewritten and removed those it had created, and logged a warning for any it could not."
            ),
            RUNTIME_PACKAGE_MISSING => format!(
                "No selected official pohunek package serves {name}. From the extracted pohunek \
release, run: pohunek plugin install runtime/packages/{name}.tar.zst --catalog \
runtime/runtime-catalog.json --yes"
            ),
            POHUNEK_COMMAND_FAILED => format!("{name} failed; check the pohunek daemon."),
            _ => format!("Configuration is invalid or missing: {name}."),
        };
    }
    response
}

/// Returns the code and key of the first configuration error in the chain.
fn first_config_error<'a>(error: &'a (dyn StdError + 'static)) -> Option<(&'a str, &'a str)> {
    std::iter::successors(Some(error), |&current| current.source()).find_map(
        |current| match current.downcast_ref::<Error>() {
            Some(Error::Config { code, name, .. }) => Some((code.as_str(), name.as_str())),
            _ => None,
        },
    )
}
