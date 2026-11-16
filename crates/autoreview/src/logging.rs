//! JSON log lines on stderr, or in a size-limited file once the config has a `log` section, so
//! stdout carries only the command's JSON.

// Rust guideline compliant 2026-06-26

mod rotating_file;

use std::fs::{DirBuilder, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};

use autoreview_engine::BoxError;
use rustix::fs::{FlockOperation, flock};
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::fmt::{self, MakeWriter};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{Layer, Registry, reload};

use self::rotating_file::{FILE_MODE, RotatingFile};
use crate::config_file::LogSettings;

/// Lowest level that is logged: `INFO` and above.
const LOG_LEVEL: LevelFilter = LevelFilter::INFO;

/// Name of the log file of `autoreview serve` in `log.directory`.
pub(crate) const SERVE_LOG_FILE: &str = "autoreview.log";

/// Name of the log file of `autoreview review-queue` in `log.directory`.
pub(crate) const QUEUE_LOG_FILE: &str = "autoreview-queue.log";

/// Name of the log file of `autoreview mcp` in `log.directory`.
pub(crate) const MCP_LOG_FILE: &str = "autoreview-mcp.log";

/// Suffix of the lock file that sits next to a log file.
const LOCK_SUFFIX: &str = ".lock";

/// Mode of a created log directory: the owner only.
const DIRECTORY_MODE: u32 = 0o700;

/// The layer that writes the log lines, boxed so that one reload handle can replace it.
type BoxedLayer = Box<dyn Layer<Registry> + Send + Sync>;

/// Replaces the layer that writes the log lines.
static HANDLE: OnceLock<reload::Handle<BoxedLayer, Registry>> = OnceLock::new();

/// The lock of the log file, held until the process exits.
static LOCK: OnceLock<File> = OnceLock::new();

/// Installs the JSON subscriber that writes every log line to stderr.
///
/// # Errors
///
/// Returns the error of `tracing-subscriber` when a global subscriber is already set.
pub(crate) fn init() -> Result<(), BoxError> {
    let layer: BoxedLayer = fmt::layer().json().with_writer(io::stderr).boxed();
    let (layer, handle) = reload::Layer::new(layer);
    tracing_subscriber::registry()
        .with(layer)
        .with(LOG_LEVEL)
        .try_init()?;
    // `init` installs the one global subscriber, so the handle is set once.
    let _ = HANDLE.set(handle);
    Ok(())
}

/// Moves the log lines from stderr to the rotating file `file_name` in `log.directory`.
///
/// Creates the directory with mode 0700 when it is missing. When another process holds the lock
/// of the file, or the filesystem cannot lock, the lines stay on stderr and one `log file in
/// use` warning says so. Afterwards stderr gets only the lines the file could not take, and the
/// error lines of the file writer.
///
/// # Errors
///
/// Returns the error of creating the directory or opening the file or its lock file.
pub(crate) fn attach(settings: &LogSettings, file_name: &str) -> io::Result<()> {
    DirBuilder::new()
        .recursive(true)
        .mode(DIRECTORY_MODE)
        .create(&settings.directory)?;
    let path = settings.directory.join(file_name);
    let lock = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(FILE_MODE)
        .open(settings.directory.join(format!("{file_name}{LOCK_SUFFIX}")))?;
    if let Err(error) = flock(&lock, FlockOperation::NonBlockingLockExclusive) {
        tracing::warn!(path = %path.display(), error = %error, "log file in use");
        return Ok(());
    }
    let file = RotatingFile::open(
        path,
        settings.max_file_bytes,
        settings.kept_files,
        Box::new(io::stderr()),
    )?;
    let layer: BoxedLayer = fmt::layer()
        .json()
        .with_writer(SharedFile(Mutex::new(file)))
        .boxed();
    let handle = HANDLE
        .get()
        .ok_or_else(|| io::Error::other("the log subscriber is not installed"))?;
    handle.reload(layer).map_err(io::Error::other)?;
    // A second attach in one process would find the lock taken, so the set cannot fail.
    let _ = LOCK.set(lock);
    Ok(())
}

/// The rotating file behind a mutex, as the writer of the file layer.
struct SharedFile(Mutex<RotatingFile>);

/// The rotating file while one log line is written.
struct LockedFile<'a>(MutexGuard<'a, RotatingFile>);

impl<'a> MakeWriter<'a> for SharedFile {
    type Writer = LockedFile<'a>;

    fn make_writer(&'a self) -> LockedFile<'a> {
        // The writer does not panic, so a poisoned lock holds a usable file.
        LockedFile(self.0.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

impl Write for LockedFile<'_> {
    fn write(&mut self, line: &[u8]) -> io::Result<usize> {
        self.0.write(line)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}
