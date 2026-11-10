//! Writing a plan: every file is kept, created, rewritten or refused before anything is written.

use std::fs::{self, DirBuilder, OpenOptions, Permissions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use autoreview_engine::Error;

use super::render::{Plan, written_by_us};
use super::{Outcome, PROFILE_FOREIGN, PROFILE_WRITE_FAILED};

/// Mode of a created directory: only the owner may read or change it.
const DIRECTORY_MODE: u32 = 0o700;

/// The permission bits that a written file or directory is compared on.
const PERMISSION_BITS: u32 = 0o777;

/// The bits of a directory that let the group or others change it.
const SHARED_WRITE_BITS: u32 = 0o022;

/// A directory of the plan, created with mode 0700 when it is absent.
#[derive(Debug)]
pub(super) struct Directory {
    /// The directory path.
    path: PathBuf,
    /// Whether missing parents are created too; otherwise the parent must exist.
    recursive: bool,
}

impl Directory {
    /// Returns the directory `path`, with its missing parents created when `recursive`.
    pub(super) fn new(path: PathBuf, recursive: bool) -> Self {
        Self { path, recursive }
    }
}

/// Who may have written an existing file at a planned path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    /// A file in pohunek's agents directory; it is ours only when it carries a valid marker.
    Marked,
    /// A file in the adapters directory of autoreview; only this command writes there.
    Owned,
}

/// One file of the plan: its path, its bytes and its mode.
#[derive(Debug)]
pub(super) struct Planned {
    /// The absolute path of the file.
    pub(super) path: PathBuf,
    /// The bytes the file must hold.
    contents: Vec<u8>,
    /// The permission bits the file must have.
    mode: u32,
    /// Who may have written an existing file there.
    kind: Kind,
}

impl Planned {
    /// Returns the planned file `path` with `contents` and `mode`.
    pub(super) fn new(path: PathBuf, contents: Vec<u8>, mode: u32, kind: Kind) -> Self {
        Self {
            path,
            contents,
            mode,
            kind,
        }
    }
}

/// What happens to one planned file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    /// The file holds the planned bytes and mode; it is kept.
    Keep,
    /// No file exists; it is created.
    Create,
    /// The file is replaced through a temporary file and a rename.
    Rewrite(Outcome),
}

/// Writes `plan`, keeping or refusing the files that exist, and returns what was done.
///
/// Every file is decided first, so a refused file leaves every other file as it was. A file
/// with the planned bytes and mode is kept. An owned file with other bytes or another mode is
/// rewritten. A marked file is rewritten when it carries a valid marker, and otherwise only
/// with `replace`. A link or a file that is not regular is always refused. The directories are
/// created next, then the files in plan order, so the profile is written last.
///
/// # Errors
///
/// Returns `profile_foreign` naming a refused path, and `profile_write_failed` naming the path
/// that cannot be checked, created or written.
pub(super) fn install(plan: &Plan, replace: bool) -> Result<Outcome, Error> {
    let steps = plan
        .files
        .iter()
        .map(|file| decide(file, replace))
        .collect::<Result<Vec<_>, _>>()?;
    for directory in &plan.directories {
        ensure_directory(directory)?;
    }
    for (file, step) in plan.files.iter().zip(&steps) {
        match step {
            Step::Keep => {}
            Step::Create => create(&file.path, &file.contents, file.mode)?,
            Step::Rewrite(_) => rewrite(file)?,
        }
    }
    Ok(outcome(&steps))
}

/// Returns the outcome of the steps, whose last one is the profile's.
///
/// A replaced file wins over a created profile, which wins over any other write.
fn outcome(steps: &[Step]) -> Outcome {
    if steps.contains(&Step::Rewrite(Outcome::Replaced)) {
        Outcome::Replaced
    } else if steps.last() == Some(&Step::Create) {
        Outcome::Created
    } else if steps.iter().any(|step| *step != Step::Keep) {
        Outcome::Updated
    } else {
        Outcome::Unchanged
    }
}

/// Decides what happens to `file` from what is at its path now; nothing is written.
fn decide(file: &Planned, replace: bool) -> Result<Step, Error> {
    let path = file.path.as_path();
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => metadata,
        Ok(_) => return Err(foreign(path, "it is a link or not a regular file")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Step::Create),
        Err(error) => return Err(write_failed(path, error)),
    };
    let existing = fs::read(path).map_err(|error| write_failed(path, error))?;
    let mode = metadata.permissions().mode() & PERMISSION_BITS;
    if existing == file.contents && mode == file.mode {
        return Ok(Step::Keep);
    }
    let ours = file.kind == Kind::Owned || std::str::from_utf8(&existing).is_ok_and(written_by_us);
    if ours {
        Ok(Step::Rewrite(Outcome::Updated))
    } else if replace {
        Ok(Step::Rewrite(Outcome::Replaced))
    } else {
        Err(foreign(path, "pass --replace to overwrite it"))
    }
}

/// Creates `directory` with mode 0700 unless it is a real directory that only its owner writes.
fn ensure_directory(directory: &Directory) -> Result<(), Error> {
    let path = directory.path.as_path();
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => {
            if metadata.permissions().mode() & SHARED_WRITE_BITS == 0 {
                Ok(())
            } else {
                Err(write_failed(
                    path,
                    io::Error::other("it is group- or world-writable"),
                ))
            }
        }
        Ok(_) => Err(write_failed(
            path,
            io::Error::other("it is a link or not a directory"),
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => DirBuilder::new()
            .recursive(directory.recursive)
            .mode(DIRECTORY_MODE)
            .create(path)
            .and_then(|()| fs::set_permissions(path, Permissions::from_mode(DIRECTORY_MODE)))
            .map_err(|error| write_failed(path, error)),
        Err(error) => Err(write_failed(path, error)),
    }
}

/// Writes `contents` to the new file `path` with exactly `mode`; a failed write removes it.
fn create(path: &Path, contents: &[u8], mode: u32) -> Result<(), Error> {
    let mut handle = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(path)
        .map_err(|error| write_failed(path, error))?;
    // The umask may have cleared bits of `mode`; the adapter needs exactly 0700.
    let written = handle
        .set_permissions(Permissions::from_mode(mode))
        .and_then(|()| handle.write_all(contents))
        .and_then(|()| handle.sync_all());
    if let Err(error) = written {
        remove_quietly(path);
        return Err(write_failed(path, error));
    }
    Ok(())
}

/// Replaces `file` through a temporary file next to it and a rename.
///
/// pohunek reads only `*.toml` files, so it never loads the temporary file.
fn rewrite(file: &Planned) -> Result<(), Error> {
    let name = file.path.file_name().map(|name| name.to_string_lossy());
    let temporary = file
        .path
        .with_file_name(format!(".{}.autoreview-new", name.unwrap_or_default()));
    create(&temporary, &file.contents, file.mode)?;
    if let Err(error) = fs::rename(&temporary, &file.path) {
        remove_quietly(&temporary);
        return Err(write_failed(&file.path, error));
    }
    Ok(())
}

/// Removes the file `path` that this command created; a failure is only logged.
fn remove_quietly(path: &Path) {
    if let Err(error) = fs::remove_file(path) {
        tracing::warn!(path = %path.display(), error = %error, "profiles install left a file");
    }
}

/// Returns the error that names `path`, a file this command did not write.
fn foreign(path: &Path, reason: &'static str) -> Error {
    Error::config(PROFILE_FOREIGN, path.to_string_lossy(), Some(reason.into()))
}

/// Returns the error that names `path`, which cannot be checked, created or written.
fn write_failed(path: &Path, cause: io::Error) -> Error {
    Error::config(
        PROFILE_WRITE_FAILED,
        path.to_string_lossy(),
        Some(Box::new(cause)),
    )
}
