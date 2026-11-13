//! Reads the `--env-file` that holds the values of the `*_env` names of the configuration.
//!
//! The values feed only the secret lookup of the loader; they never enter the process
//! environment, so no child inherits them.

// Rust guideline compliant 2026-11-13

use std::collections::BTreeMap;
use std::fmt;
use std::fs::File;
use std::io::{self, Read};
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use autoreview_engine::Error;
use autoreview_engine::config::SECRET_ENV_PREFIX;
use rustix::fs::{Mode, OFlags};

/// Name of the flag in errors.
pub(crate) const ENV_FILE_FLAG: &str = "--env-file";
/// Error code of a file that cannot be used.
const CONFIGURATION_INVALID: &str = "configuration_invalid";
/// Permission bits that give the group or others any access.
const SHARED_MODE_BITS: u32 = 0o077;
/// Permission bits shown in errors, with the setuid, setgid and sticky bits.
const MODE_BITS: u32 = 0o7777;

/// Secret values of an `--env-file`, by variable name.
///
/// `Debug` shows the names only, so a value never reaches a log.
pub(crate) struct EnvFile {
    /// The value of each variable that the file names.
    values: BTreeMap<String, String>,
}

impl fmt::Debug for EnvFile {
    /// Shows the variable names only.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EnvFile")
            .field("names", &self.values.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl EnvFile {
    /// Reads the file at `path`, which the effective user must own; see [`read_as`].
    ///
    /// # Errors
    ///
    /// Returns the errors of [`read_as`].
    pub(crate) fn read(path: &Path) -> Result<Self, Error> {
        read_as(path, rustix::process::geteuid().as_raw())
    }
}

/// Reads the file at `path` as the user `euid`.
///
/// A symlink is followed. The checks use the metadata of the opened file: a regular file
/// owned by `euid` that gives the group and others no access. Every other line than a blank
/// one or one that starts with `#` is `NAME=value`; the value is the rest of the line.
///
/// # Errors
///
/// Returns `configuration_invalid` naming `--env-file`, with a message that names `path`, for
/// a file that cannot be opened or read, a failed check, or a line without `=`, with a name
/// other than `AUTOREVIEW_[A-Z0-9_]+` or with the name of an earlier line. An error names the
/// line number, never the line.
pub(crate) fn read_as(path: &Path, euid: u32) -> Result<EnvFile, Error> {
    let unreadable = |error: io::Error| refused(path, &format!("cannot be read: {error}"));
    // Without O_NONBLOCK, opening a FIFO would wait for a writer before the type check.
    let flags = OFlags::RDONLY | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let descriptor =
        rustix::fs::open(path, flags, Mode::empty()).map_err(|errno| unreadable(errno.into()))?;
    let mut file = File::from(descriptor);
    let metadata = file.metadata().map_err(unreadable)?;
    if !metadata.file_type().is_file() {
        return Err(refused(path, "is not a regular file"));
    }
    let owner = metadata.uid();
    if owner != euid {
        let reason = format!("is owned by uid {owner}, not by uid {euid} that runs autoreview");
        return Err(refused(path, &reason));
    }
    let mode = metadata.mode() & MODE_BITS;
    if mode & SHARED_MODE_BITS != 0 {
        let reason = format!("has mode {mode:04o}; only its owner may have access (0600)");
        return Err(refused(path, &reason));
    }
    let mut text = String::new();
    file.read_to_string(&mut text).map_err(unreadable)?;
    parse(path, &text).map(|values| EnvFile { values })
}

/// Returns the secret `name`: the file's value when the file names it, else `environment`'s.
pub(crate) fn secret(
    file: Option<&EnvFile>,
    name: &str,
    environment: impl FnOnce(&str) -> Option<String>,
) -> Option<String> {
    match file.and_then(|file| file.values.get(name)) {
        Some(value) => Some(value.clone()),
        None => environment(name),
    }
}

/// Reads the `NAME=value` lines of `text`, the content of the file at `path`.
///
/// # Errors
///
/// Returns the line errors of [`read_as`].
fn parse(path: &Path, text: &str) -> Result<BTreeMap<String, String>, Error> {
    let mut values = BTreeMap::new();
    for (index, line) in text.lines().enumerate() {
        let number = index + 1;
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((name, value)) = line.split_once('=') else {
            return Err(refused(path, &format!("line {number} has no '='")));
        };
        if !name_valid(name) {
            let reason = format!("line {number} names no AUTOREVIEW_ variable of A-Z, 0-9 and _");
            return Err(refused(path, &reason));
        }
        if values.insert(name.to_owned(), value.to_owned()).is_some() {
            let reason = format!("line {number} repeats the name of an earlier line");
            return Err(refused(path, &reason));
        }
    }
    Ok(values)
}

/// Reports whether `name` is `AUTOREVIEW_` followed by at least one of A-Z, 0-9 and `_`.
///
/// The one rule for a name in the file and for the name in every `*_env` key.
pub(crate) fn name_valid(name: &str) -> bool {
    name.strip_prefix(SECRET_ENV_PREFIX).is_some_and(|rest| {
        !rest.is_empty()
            && rest
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
    })
}

/// Returns the `configuration_invalid` error of the file at `path` for `reason`.
fn refused(path: &Path, reason: &str) -> Error {
    let shown = path.display();
    Error::config_with_message(
        CONFIGURATION_INVALID,
        ENV_FILE_FLAG,
        format!("{ENV_FILE_FLAG} {shown} {reason}."),
        Some(format!("{shown} {reason}").into()),
    )
}
