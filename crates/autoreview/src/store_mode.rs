//! The check of `validate` that the store and its directory are private to their owner.

// Rust guideline compliant 2026-11-15

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use autoreview_engine::Error;

use crate::CONFIGURATION_INVALID;

/// The configuration key that names the store.
const STORE_KEY: &str = "store.path";

/// Permission bits that give the group or others any access.
const SHARED_MODE_BITS: u32 = 0o077;

/// Permission bits shown in the message, with the setuid, setgid and sticky bits.
const MODE_BITS: u32 = 0o7777;

/// Suffixes of the database and of the two files that SQLite keeps next to it.
const FILE_SUFFIXES: [&str; 3] = ["", "-wal", "-shm"];

/// Checks that the store directory and the store files that exist grant nothing to others.
///
/// A missing path passes: `Store::open` creates a missing directory 0700 and a missing
/// database 0600, and SQLite gives `-wal` and `-shm` the database's mode. A mode is only
/// reported, never changed.
///
/// # Errors
///
/// Returns `configuration_invalid` naming `store.path` when a mode gives the group or other
/// users access, or when a mode cannot be read.
pub(crate) fn check(store_path: &Path) -> Result<(), Error> {
    let Some(directory) = store_path.parent() else {
        return Ok(());
    };
    let directory_mode = mode_of(directory).map_err(|error| unreadable(directory, error))?;
    let mut entries = vec![(directory.to_path_buf(), directory_mode, "700")];
    for suffix in FILE_SUFFIXES {
        let mut name = store_path.as_os_str().to_owned();
        name.push(suffix);
        let path = PathBuf::from(name);
        let mode = match mode_of(&path) {
            Ok(mode) => mode,
            // Only a caller that cannot search the directory gets this; its mode is checked.
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => None,
            Err(error) => return Err(unreadable(&path, error)),
        };
        entries.push((path, mode, "600"));
    }
    match exposure(&entries) {
        Some(message) => Err(Error::config_with_message(
            CONFIGURATION_INVALID,
            STORE_KEY,
            message,
            None,
        )),
        None => Ok(()),
    }
}

/// Returns the permission bits of `path`, or `None` when it does not exist.
fn mode_of(path: &Path) -> io::Result<Option<u32>> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(Some(metadata.permissions().mode() & MODE_BITS)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Returns the message naming each path whose mode gives the group or others access.
///
/// Each entry is a path, its mode (`None` when it is missing) and the mode of its fix.
fn exposure(entries: &[(PathBuf, Option<u32>, &str)]) -> Option<String> {
    let fixes: Vec<String> = entries
        .iter()
        .filter_map(|(path, mode, fix)| {
            let mode = mode.filter(|mode| mode & SHARED_MODE_BITS != 0)?;
            let shown = path.display();
            Some(format!(
                "{shown} has mode {mode:04o}, run chmod {fix} {shown}"
            ))
        })
        .collect();
    if fixes.is_empty() {
        return None;
    }
    Some(format!(
        "store.path must be private to the service user: {}. validate changes no mode.",
        fixes.join("; ")
    ))
}

/// Returns the `configuration_invalid` error of a path whose mode cannot be read.
fn unreadable(path: &Path, error: io::Error) -> Error {
    let message = format!("store.path: the mode of {} cannot be read.", path.display());
    Error::config_with_message(
        CONFIGURATION_INVALID,
        STORE_KEY,
        message,
        Some(Box::new(error)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A case name, the modes of the directory, database, log and memory, the reported ones.
    type Case = (&'static str, [Option<u32>; 4], &'static [usize]);

    /// The directory must be 0700 and each existing store file 0600 or narrower (#439, D3).
    #[test]
    fn store_modes_wider_than_owner_only_are_reported() {
        let paths = ["/s", "/s/a.db", "/s/a.db-wal", "/s/a.db-shm"];
        let fixes = ["700", "600", "600", "600"];
        let (absent, private) = ([None; 4], Some(0o700));
        let cases: [Case; 10] = [
            ("nothing exists yet", absent, &[]),
            ("private", [private, Some(0o600), None, None], &[]),
            (
                "narrower",
                [Some(0o500), Some(0o400), Some(0o600), Some(0o600)],
                &[],
            ),
            ("setgid only", [Some(0o2700), Some(0o600), None, None], &[]),
            (
                "umask 022",
                [Some(0o755), Some(0o644), Some(0o644), Some(0o644)],
                &[0, 1, 2, 3],
            ),
            ("group may search", [Some(0o710), None, None, None], &[0]),
            ("others may search", [Some(0o701), None, None, None], &[0]),
            ("group may read", [private, Some(0o640), None, None], &[1]),
            (
                "others may write the log",
                [private, Some(0o600), Some(0o602), None],
                &[2],
            ),
            (
                "group may read the memory",
                [private, Some(0o600), None, Some(0o640)],
                &[3],
            ),
        ];
        for (case, modes, reported) in cases {
            let entries: Vec<(PathBuf, Option<u32>, &str)> = (0..4)
                .map(|i| (PathBuf::from(paths[i]), modes[i], fixes[i]))
                .collect();
            let expected = (!reported.is_empty()).then(|| {
                let shown: Vec<String> = reported
                    .iter()
                    .map(|&i| {
                        let mode = modes[i].unwrap_or_default();
                        format!(
                            "{} has mode {mode:04o}, run chmod {} {}",
                            paths[i], fixes[i], paths[i]
                        )
                    })
                    .collect();
                format!(
                    "store.path must be private to the service user: {}. validate changes no \
mode.",
                    shown.join("; ")
                )
            });
            assert_eq!(exposure(&entries), expected, "{case}");
        }
        let entries = [(PathBuf::from("/s"), Some(0o755), "700")];
        assert_eq!(
            exposure(&entries).as_deref(),
            Some(
                "store.path must be private to the service user: /s has mode 0755, run chmod 700 \
/s. validate changes no mode."
            )
        );
    }
}
