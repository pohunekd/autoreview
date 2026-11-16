//! A log file that rotates by size and hands what it cannot write to a fallback sink.

// Rust guideline compliant 2026-06-26

#![cfg_attr(
    not(test),
    expect(dead_code, reason = "attached by the logging wiring")
)]

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::json;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

/// Mode of a created log file: read and write for the owner only.
const FILE_MODE: u32 = 0o600;

/// Time before a failed rotation is tried again, and between two error lines of the writer.
const RETRY_INTERVAL: Duration = Duration::from_secs(60);

/// Target of the error lines that the writer writes itself.
const ERROR_TARGET: &str = "autoreview::logging";

/// A log file that rotates before a line would pass its size limit.
///
/// Rotated files are `<file>.1` (newest) to `<file>.<kept_files>` (oldest). A line is never
/// split, so a line longer than the limit fills a fresh file alone. Each call of
/// [`Write::write`] takes one whole line.
pub(super) struct RotatingFile {
    /// Path of the current file.
    path: PathBuf,
    /// Size from which the next line goes to a fresh file.
    max_bytes: u64,
    /// Rotated files kept besides the current one.
    kept_files: usize,
    /// The current file, open for appending.
    file: File,
    /// Bytes in the current file.
    written: u64,
    /// The current file is renamed away and its replacement is not open yet.
    renamed: bool,
    /// Instant before which a failed rotation is not tried again.
    retry_at: Option<Instant>,
    /// Instant before which an error line is only counted.
    report_at: Option<Instant>,
    /// Error lines counted since the last one that was written.
    repeats: u64,
    /// Receives every line the file could not take, and the error lines of the writer.
    fallback: Box<dyn Write + Send>,
}

impl RotatingFile {
    /// Opens `path` for appending and starts the size count at its length.
    ///
    /// A created file gets mode 0600; an existing file keeps its mode.
    ///
    /// # Errors
    ///
    /// Returns the error of opening the file or reading its length.
    pub(super) fn open(
        path: PathBuf,
        max_bytes: u64,
        kept_files: usize,
        fallback: Box<dyn Write + Send>,
    ) -> io::Result<Self> {
        let file = open_append(&path)?;
        let written = file.metadata()?.len();
        Ok(Self {
            path,
            max_bytes,
            kept_files,
            file,
            written,
            renamed: false,
            retry_at: None,
            report_at: None,
            repeats: 0,
            fallback,
        })
    }

    /// Appends `line`, rotating first when it would pass the limit.
    fn append(&mut self, line: &[u8]) {
        let length = u64::try_from(line.len()).unwrap_or(u64::MAX);
        if self.written > 0 && self.written.saturating_add(length) > self.max_bytes {
            self.rotate();
        }
        if let Err(error) = self.file.write_all(line) {
            // A partial write leaves a cut line behind, so the count follows the file.
            self.written = self
                .file
                .metadata()
                .map_or(self.written, |metadata| metadata.len());
            self.send(line);
            self.report("log write failed", &error);
        } else {
            self.written = self.written.saturating_add(length);
        }
    }

    /// Moves the current file to `.1` and opens a fresh one, unless a failure is backing off.
    ///
    /// After a failure the current file keeps taking lines, so it may pass the limit.
    fn rotate(&mut self) {
        let now = Instant::now();
        if self.retry_at.is_some_and(|at| now < at) {
            return;
        }
        match self.rename_and_open() {
            Ok(file) => {
                self.file = file;
                self.written = 0;
                self.retry_at = None;
            }
            Err(error) => {
                self.retry_at = Some(now + RETRY_INTERVAL);
                self.report("log rotation failed", &error);
            }
        }
    }

    /// Shifts `.N-1` to `.N` down to the current file to `.1`, then opens the new current file.
    ///
    /// The oldest file is replaced by the rename. When only the open fails, the next call opens
    /// without renaming again.
    fn rename_and_open(&mut self) -> io::Result<File> {
        if !self.renamed {
            for index in (1..self.kept_files).rev() {
                rename_if_present(&self.numbered(index), &self.numbered(index + 1))?;
            }
            rename_if_present(&self.path, &self.numbered(1))?;
            self.renamed = true;
        }
        let file = open_append(&self.path)?;
        self.renamed = false;
        Ok(file)
    }

    /// Returns the path of the rotated file `index`.
    fn numbered(&self, index: usize) -> PathBuf {
        let mut name = self.path.clone().into_os_string();
        name.push(format!(".{index}"));
        PathBuf::from(name)
    }

    /// Writes `line` to the fallback sink.
    fn send(&mut self, line: &[u8]) {
        // The fallback is the last sink, so a failure here has nowhere to go.
        let _ = self.fallback.write_all(line);
    }

    /// Writes one error line to the fallback, at most once per [`RETRY_INTERVAL`].
    ///
    /// The line counts the error lines withheld since the previous one.
    fn report(&mut self, message: &str, error: &io::Error) {
        let now = Instant::now();
        if self.report_at.is_some_and(|at| now < at) {
            self.repeats = self.repeats.saturating_add(1);
            return;
        }
        let timestamp = OffsetDateTime::now_utc().format(&Rfc3339).ok();
        let line = json!({
            "timestamp": timestamp,
            "level": "ERROR",
            "fields": {
                "message": message,
                "error": error.to_string(),
                "path": self.path.display().to_string(),
                "repeats": self.repeats,
            },
            "target": ERROR_TARGET,
        });
        self.report_at = Some(now + RETRY_INTERVAL);
        self.repeats = 0;
        self.send(format!("{line}\n").as_bytes());
    }
}

impl Write for RotatingFile {
    /// Appends one whole line; a failure goes to the fallback and is not returned.
    fn write(&mut self, line: &[u8]) -> io::Result<usize> {
        self.append(line);
        Ok(line.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Opens `path` for appending and creates it with mode 0600 when it is missing.
fn open_append(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .mode(FILE_MODE)
        .open(path)
}

/// Renames `from` to `to`, replacing `to`; a missing `from` is not an error.
fn rename_if_present(from: &Path, to: &Path) -> io::Result<()> {
    match std::fs::rename(from, to) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    /// A fallback sink that the test can read.
    #[derive(Clone, Default)]
    struct Sink(Arc<Mutex<Vec<u8>>>);

    impl Sink {
        fn text(&self) -> String {
            String::from_utf8(self.0.lock().expect("the sink lock").clone()).expect("UTF-8")
        }
    }

    impl Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0
                .lock()
                .expect("the sink lock")
                .extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// A line of 31 bytes: three fit under a limit of 100.
    fn line(number: usize) -> String {
        format!("{{\"n\":{number:04}}}{}\n", "x".repeat(20))
    }

    fn lines(numbers: std::ops::Range<usize>) -> String {
        numbers.map(line).collect()
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).expect("the file exists")
    }

    #[test]
    fn rotation_keeps_the_newest_files_and_a_failure_drops_no_line() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let path = dir.path().join("autoreview.log");
        let numbered = |index: usize| dir.path().join(format!("autoreview.log.{index}"));
        let sink = Sink::default();
        let mut file =
            RotatingFile::open(path.clone(), 100, 3, Box::new(sink.clone())).expect("opens");
        for number in 0..30 {
            file.write_all(line(number).as_bytes()).expect("a line");
        }
        assert_eq!(
            read(&path),
            lines(27..30),
            "the newest lines are in the current file"
        );
        assert_eq!(read(&numbered(1)), lines(24..27));
        assert_eq!(read(&numbered(2)), lines(21..24));
        assert_eq!(read(&numbered(3)), lines(18..21));
        assert!(!numbered(4).exists(), "no file above kept_files");
        let long = "y".repeat(250) + "\n";
        file.write_all(long.as_bytes()).expect("a long line");
        assert_eq!(
            read(&path),
            long,
            "a line over the limit fills a fresh file"
        );
        file.write_all(line(30).as_bytes()).expect("a line");
        assert_eq!(read(&numbered(1)), long, "the long line is never split");
        assert_eq!(read(&path), line(30));
        assert_eq!(sink.text(), "", "nothing reached the fallback");

        // A directory in the place of `.2` makes the second rotation fail.
        let failing = tempfile::tempdir().expect("a temporary directory");
        let path = failing.path().join("autoreview.log");
        let blocked = failing.path().join("autoreview.log.2");
        std::fs::create_dir(&blocked).expect("a directory in the way");
        std::fs::write(blocked.join("keep"), "x").expect("a file in it");
        let sink = Sink::default();
        let mut file =
            RotatingFile::open(path.clone(), 100, 2, Box::new(sink.clone())).expect("opens");
        for number in 0..10 {
            file.write_all(line(number).as_bytes()).expect("a line");
        }
        assert_eq!(read(&path), lines(3..10), "a failed rotation drops no line");
        assert!(
            sink.text().contains("log rotation failed"),
            "the failure is reported"
        );
    }
}
