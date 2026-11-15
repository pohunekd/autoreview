//! The pohunek CLI version check of `validate` and `serve`.
//!
//! The engine reads the JSON answers of the pohunek CLI at `pohunek.bin`; the supported range
//! names the releases whose answers it reads as they are.

// Rust guideline compliant 2026-06-26

use std::fmt;

use autoreview_engine::Error;
use autoreview_engine::config::Config;
use tokio_util::sync::CancellationToken;

use super::pohunek_environment;
use crate::process::run;

/// Oldest supported pohunek release; `scripts/setup` keeps the same `POHUNEK_MIN_VERSION`.
///
/// The `host command run` time limits of the configuration follow pohunek 0.41.0.
const SUPPORTED_FROM: Release = Release::new(0, 41, 0);

/// First pohunek release past the supported range: the next major release.
///
/// A minor release that changes the JSON fails each review as `pohunek_response_invalid`.
const SUPPORTED_BEFORE: Release = Release::new(1, 0, 0);

/// Option of the pohunek CLI that prints its version.
const VERSION_OPTION: &str = "--version";

/// Text before the release number in the output of `pohunek --version`.
const VERSION_PREFIX: &str = "pohunek ";

/// Configuration key of the pohunek CLI that the check runs.
const POHUNEK_BIN_KEY: &str = "pohunek.bin";

/// Error code of a version output that cannot be read.
const VERSION_INVALID: &str = "pohunek_version_invalid";

/// Error code of a release outside the supported range.
const VERSION_UNSUPPORTED: &str = "pohunek_version_unsupported";

/// A pohunek release number, `MAJOR.MINOR.PATCH`, ordered part by part.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Release {
    /// Major part.
    major: u64,
    /// Minor part.
    minor: u64,
    /// Patch part.
    patch: u64,
}

impl Release {
    /// Returns the release `major.minor.patch`.
    const fn new(major: u64, minor: u64, patch: u64) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }
}

impl fmt::Display for Release {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Why a version output is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refusal {
    /// The output is not one line `pohunek MAJOR.MINOR.PATCH`.
    Invalid,
    /// The release is outside the supported range.
    Unsupported(Release),
}

/// Returns the release that `output` names when it is in the supported range.
///
/// The output is `pohunek ` and three decimal parts joined by dots, with at most one final line
/// feed. A pre-release or build suffix, other text and a part above `u64::MAX` are invalid.
fn supported_release(output: &str) -> Result<Release, Refusal> {
    let line = output.strip_suffix('\n').unwrap_or(output);
    let number = line.strip_prefix(VERSION_PREFIX).ok_or(Refusal::Invalid)?;
    let mut parts = number.split('.');
    let mut next = || -> Result<u64, Refusal> {
        let part = parts.next().ok_or(Refusal::Invalid)?;
        if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(Refusal::Invalid);
        }
        part.parse::<u64>().map_err(|_| Refusal::Invalid)
    };
    let release = Release::new(next()?, next()?, next()?);
    if parts.next().is_some() {
        return Err(Refusal::Invalid);
    }
    if release < SUPPORTED_FROM || release >= SUPPORTED_BEFORE {
        return Err(Refusal::Unsupported(release));
    }
    Ok(release)
}

/// Returns the version of the pohunek CLI at `pohunek.bin` when the engine supports it.
///
/// One `pohunek --version` run in the pohunek environment, bounded by `pohunek.timeout_seconds`.
///
/// # Errors
///
/// Returns `pohunek_version_invalid` for `pohunek.bin` when the run fails or prints no release
/// number, and `pohunek_version_unsupported` when the release is outside the supported range.
pub(crate) async fn checked_version(
    config: &Config,
    cancel: &CancellationToken,
) -> Result<String, Error> {
    let environment = pohunek_environment(config, std::env::vars_os());
    let arguments = [VERSION_OPTION.to_owned()];
    let output = run(
        cancel,
        &config.pohunek_bin,
        &arguments,
        &[],
        &environment,
        config.pohunek_timeout,
    )
    .await
    .map_err(|error| {
        let message = format!("`{VERSION_OPTION}` of {POHUNEK_BIN_KEY} did not succeed.");
        Error::config_with_message(
            VERSION_INVALID,
            POHUNEK_BIN_KEY,
            message,
            Some(error.into()),
        )
    })?;
    match supported_release(&output) {
        Ok(release) => Ok(release.to_string()),
        Err(Refusal::Invalid) => {
            let message =
                format!("{POHUNEK_BIN_KEY} did not print its version as `{VERSION_PREFIX}X.Y.Z`.");
            Err(Error::config_with_message(
                VERSION_INVALID,
                POHUNEK_BIN_KEY,
                message,
                None,
            ))
        }
        Err(Refusal::Unsupported(release)) => {
            let message = format!(
                "pohunek {release} at {POHUNEK_BIN_KEY} is not supported: autoreview needs \
{SUPPORTED_FROM} or later, before {SUPPORTED_BEFORE}."
            );
            Err(Error::config_with_message(
                VERSION_UNSUPPORTED,
                POHUNEK_BIN_KEY,
                message,
                None,
            ))
        }
    }
}
