//! Writing a plan: every file is kept, created, rewritten or refused before anything is written.

use std::fs::{self, DirBuilder, OpenOptions, Permissions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use autoreview_engine::{BoxError, Error};

use super::render::{Plan, written_by_us};
use super::{Outcome, PROFILE_FOREIGN, PROFILE_WRITE_FAILED};

/// Mode of a created directory: only the owner may read or change it.
const DIRECTORY_MODE: u32 = 0o700;

/// The permission bits that a written file or directory is compared on.
const PERMISSION_BITS: u32 = 0o777;

/// The bits of a directory that let the group or others change it.
const SHARED_WRITE_BITS: u32 = 0o022;

/// How many temporary names a rewrite tries when a name is taken.
const TEMPORARY_ATTEMPTS: u32 = 16;

/// Counts the temporary names of this process, so that each rewrite gets a new one.
static TEMPORARY_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A directory of the plan, created with mode 0700 when it is absent.
#[derive(Debug)]
pub(super) struct Directory {
    /// The directory path.
    path: PathBuf,
    /// Whether missing parents are created too, unchecked; otherwise the parent must exist.
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

/// The bytes and the mode of a file before a rewrite, to put back when a later step fails.
#[derive(Debug)]
struct Previous {
    /// The bytes the file held.
    contents: Vec<u8>,
    /// The permission bits the file had.
    mode: u32,
}

/// One change of a running install, undone when a later step fails.
#[derive(Debug)]
enum Undo {
    /// A directory that this run created.
    Directory(PathBuf),
    /// A file that this run created.
    Created(PathBuf),
    /// A file that this run rewrote, with what it held before.
    Rewritten(PathBuf, Previous),
}

/// Writes `plan`, keeping or refusing the files that exist, and returns what was done.
///
/// Every file is decided first, so a refused file leaves every other file as it was. A file
/// with the planned bytes and mode is kept. An owned file with other bytes or another mode is
/// rewritten. A marked file is rewritten when it carries a valid marker, and otherwise only
/// with `replace`. A link or a file that is not regular is always refused. The directories are
/// created next, then the files in plan order, so the profile is written last. When a step
/// fails, every file this call rewrote gets its earlier bytes and mode back, and the files and
/// directories it created are removed, newest first.
///
/// # Errors
///
/// Returns `profile_foreign` naming a refused path, and `profile_write_failed` naming the path
/// that cannot be checked, created or written.
pub(super) fn install(plan: &Plan, replace: bool) -> Result<Outcome, Error> {
    let decisions = plan
        .files
        .iter()
        .map(|file| decide(file, replace))
        .collect::<Result<Vec<_>, _>>()?;
    let mut undo = Vec::new();
    let written = write_all(plan, decisions, &mut undo);
    if written.is_err() {
        for change in undo.into_iter().rev() {
            undo_change(change);
        }
    }
    written
}

/// Ensures the directories, then writes each file as decided, recording every change in `undo`.
fn write_all(
    plan: &Plan,
    decisions: Vec<(Step, Option<Previous>)>,
    undo: &mut Vec<Undo>,
) -> Result<Outcome, Error> {
    for directory in &plan.directories {
        ensure_directory(directory, undo)?;
    }
    let mut steps = Vec::with_capacity(decisions.len());
    for (file, (step, previous)) in plan.files.iter().zip(decisions) {
        match step {
            Step::Keep => {}
            Step::Create => {
                create(&file.path, &file.contents, file.mode)?;
                undo.push(Undo::Created(file.path.clone()));
            }
            Step::Rewrite(_) => {
                rewrite(&file.path, &file.contents, file.mode)?;
                if let Some(previous) = previous {
                    undo.push(Undo::Rewritten(file.path.clone(), previous));
                }
            }
        }
        steps.push(step);
    }
    Ok(outcome(&steps))
}

/// Undoes one change of a failed install; a failure is only logged.
fn undo_change(change: Undo) {
    let (path, undone): (PathBuf, Result<(), BoxError>) = match change {
        Undo::Directory(path) => {
            let undone = fs::remove_dir(&path).map_err(BoxError::from);
            (path, undone)
        }
        Undo::Created(path) => {
            let undone = fs::remove_file(&path).map_err(BoxError::from);
            (path, undone)
        }
        Undo::Rewritten(path, previous) => {
            let undone = rewrite(&path, &previous.contents, previous.mode).map_err(BoxError::from);
            (path, undone)
        }
    };
    if let Err(error) = undone {
        tracing::warn!(path = %path.display(), error = %error, "profiles install left a change");
    }
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
///
/// A rewrite also returns the bytes and the mode that the file holds now.
fn decide(file: &Planned, replace: bool) -> Result<(Step, Option<Previous>), Error> {
    let path = file.path.as_path();
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => metadata,
        Ok(_) => return Err(foreign(path, "it is a link or not a regular file")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok((Step::Create, None)),
        Err(error) => return Err(write_failed(path, error)),
    };
    let existing = fs::read(path).map_err(|error| write_failed(path, error))?;
    let mode = metadata.permissions().mode() & PERMISSION_BITS;
    if existing == file.contents && mode == file.mode {
        return Ok((Step::Keep, None));
    }
    let ours = file.kind == Kind::Owned || std::str::from_utf8(&existing).is_ok_and(written_by_us);
    let step = if ours {
        Step::Rewrite(Outcome::Updated)
    } else if replace {
        Step::Rewrite(Outcome::Replaced)
    } else {
        return Err(foreign(path, "pass --replace to overwrite it"));
    };
    let previous = Previous {
        contents: existing,
        mode,
    };
    Ok((step, Some(previous)))
}

/// Creates `directory` with mode 0700 unless it is a real directory that only its owner writes.
///
/// Each created directory, a missing parent of a recursive one too, is recorded in `undo`.
fn ensure_directory(directory: &Directory, undo: &mut Vec<Undo>) -> Result<(), Error> {
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
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut missing = vec![path];
            if directory.recursive {
                for parent in path.ancestors().skip(1) {
                    match fs::symlink_metadata(parent) {
                        Ok(_) => break,
                        Err(error) if error.kind() == io::ErrorKind::NotFound => {
                            missing.push(parent);
                        }
                        Err(error) => return Err(write_failed(parent, error)),
                    }
                }
            }
            for path in missing.into_iter().rev() {
                DirBuilder::new()
                    .mode(DIRECTORY_MODE)
                    .create(path)
                    .map_err(|error| write_failed(path, error))?;
                undo.push(Undo::Directory(path.to_path_buf()));
                fs::set_permissions(path, Permissions::from_mode(DIRECTORY_MODE))
                    .map_err(|error| write_failed(path, error))?;
            }
            Ok(())
        }
        Err(error) => Err(write_failed(path, error)),
    }
}

/// Writes `contents` to the new file `path` with exactly `mode`; a failed write removes it.
fn create(path: &Path, contents: &[u8], mode: u32) -> Result<(), Error> {
    write_new(path, contents, mode).map_err(|error| write_failed(path, error))
}

/// Writes `contents` to the new file `path` with exactly `mode`; a failed write removes it.
///
/// # Errors
///
/// Returns the error of the open, which is `AlreadyExists` when `path` exists, or of the write.
fn write_new(path: &Path, contents: &[u8], mode: u32) -> io::Result<()> {
    let mut handle = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(path)?;
    // The umask may have cleared bits of `mode`; the adapter needs exactly 0700.
    let written = handle
        .set_permissions(Permissions::from_mode(mode))
        .and_then(|()| handle.write_all(contents))
        .and_then(|()| handle.sync_all());
    if let Err(error) = written {
        remove_quietly(path);
        return Err(error);
    }
    Ok(())
}

/// Replaces the file `path` with `contents` and `mode` through a temporary file and a rename.
///
/// The temporary file sits next to `path` under a name of this process and rewrite, so a file
/// left by a killed run never blocks a later one. pohunek reads only `*.toml` files, so it
/// never loads the temporary file.
fn rewrite(path: &Path, contents: &[u8], mode: u32) -> Result<(), Error> {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_default();
    for _attempt in 0..TEMPORARY_ATTEMPTS {
        let count = TEMPORARY_COUNTER.fetch_add(1, Ordering::Relaxed);
        let temporary = path.with_file_name(format!(
            ".{name}.{}.{count}.autoreview-new",
            std::process::id()
        ));
        match write_new(&temporary, contents, mode) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(write_failed(path, error)),
        }
        if let Err(error) = fs::rename(&temporary, path) {
            remove_quietly(&temporary);
            return Err(write_failed(path, error));
        }
        return Ok(());
    }
    Err(write_failed(
        path,
        io::Error::other("every temporary name next to it is taken"),
    ))
}

/// Removes the file `path` that this command created; a failure is only logged.
fn remove_quietly(path: &Path) {
    if let Err(error) = fs::remove_file(path) {
        tracing::warn!(path = %path.display(), error = %error, "profiles install left a file");
    }
}

/// Returns the error that names `path`, a file this command did not write.
fn foreign(path: &Path, reason: &'static str) -> Error {
    let name = path.to_string_lossy();
    let message = format!(
        "{name} was not written by profiles install; move it away, or pass --replace \
for a regular file."
    );
    Error::config_with_message(PROFILE_FOREIGN, name, message, Some(reason.into()))
}

/// Returns the error that names `path`, which cannot be checked, created or written.
fn write_failed(path: &Path, cause: io::Error) -> Error {
    let name = path.to_string_lossy();
    let message = format!(
        "{name} cannot be written; profiles install restored the files it had \
rewritten and removed those it had created, and logged a warning for any it could not."
    );
    Error::config_with_message(PROFILE_WRITE_FAILED, name, message, Some(Box::new(cause)))
}
