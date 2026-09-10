//! Line logging to stderr and to [`proto::LOG_PATH`].
//!
//! Lines look like `[2026-09-10T03:04:05Z] INFO started`. The file handle is
//! optional: when the daemon cannot open the log file, for example in an
//! unprivileged dev run, it still logs to stderr.
//!
//! WARNING: launchd points the daemon's stderr at the same log file (see
//! `StandardErrorPath` in [`crate::install::plist`]). Writing to both would
//! record every line twice, so [`init`] compares the two and drops the
//! stderr copy when they are one file.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::mem::ManuallyDrop;
use std::os::fd::{FromRawFd, RawFd};
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

static LOG_FILE: OnceLock<Option<Mutex<File>>> = OnceLock::new();

/// True when stderr already points at the log file, so the stderr copy of
/// every line is dropped.
static STDERR_IS_LOG: AtomicBool = AtomicBool::new(false);

/// Opens `path` for appending and sends later log lines to it as well.
///
/// A failure is reported once on stderr and then ignored.
pub fn init(path: &Path) {
    LOG_FILE.get_or_init(|| match open_append(path) {
        Ok(file) => Some(Mutex::new(file)),
        Err(err) => {
            eprintln!("chargecapd: cannot open log file {}: {err}", path.display());
            None
        }
    });
    STDERR_IS_LOG.store(same_file(libc::STDERR_FILENO, path), Ordering::SeqCst);
}

/// Returns true when `fd` and `path` are the same file.
///
/// Compares the device and inode numbers. A missing path or an unusable
/// descriptor gives false, so the caller keeps both copies of the line.
fn same_file(fd: RawFd, path: &Path) -> bool {
    // SAFETY: the wrapper is never dropped, so `fd` is never closed here.
    let file = ManuallyDrop::new(unsafe { File::from_raw_fd(fd) });
    let (Ok(from_fd), Ok(from_path)) = (file.metadata(), fs::metadata(path)) else {
        return false;
    };
    from_fd.dev() == from_path.dev() && from_fd.ino() == from_path.ino()
}

/// Logs one line at level `INFO`.
pub fn info(message: impl AsRef<str>) {
    write_line("INFO", message.as_ref());
}

/// Logs one line at level `WARN`.
pub fn warn(message: impl AsRef<str>) {
    write_line("WARN", message.as_ref());
}

/// Logs one line at level `ERROR`.
pub fn error(message: impl AsRef<str>) {
    write_line("ERROR", message.as_ref());
}

fn open_append(path: &Path) -> std::io::Result<File> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    OpenOptions::new().create(true).append(true).open(path)
}

fn write_line(level: &str, message: &str) {
    let line = format!("[{}] {level} {message}", timestamp(SystemTime::now()));
    let mut written = false;
    if let Some(Some(file)) = LOG_FILE.get() {
        if let Ok(mut file) = file.lock() {
            written = writeln!(file, "{line}").is_ok();
        }
    }
    // Drop the stderr copy only when it would land in the same log file.
    if !written || !STDERR_IS_LOG.load(Ordering::SeqCst) {
        eprintln!("{line}");
    }
}

/// Formats `time` as `YYYY-MM-DDTHH:MM:SSZ` in UTC.
pub fn timestamp(time: SystemTime) -> String {
    let secs = time
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86_400);
    let time_of_day = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let (hour, minute, second) = (
        time_of_day / 3600,
        (time_of_day % 3600) / 60,
        time_of_day % 60,
    );
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Converts days since 1970-01-01 to a civil date. Howard Hinnant's
/// `civil_from_days`, which is exact for every date this daemon can see.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;
    use std::time::Duration;

    fn at(secs: u64) -> String {
        timestamp(UNIX_EPOCH + Duration::from_secs(secs))
    }

    /// Regression: launchd points stderr at the log file, so a daemon that
    /// wrote to both recorded every line twice.
    #[test]
    fn same_file_spots_a_descriptor_that_points_at_the_log() {
        let dir = std::env::temp_dir().join(format!(
            "chargecap-log-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let log = dir.join("daemon.log");
        let other = dir.join("other.log");

        let handle = open_append(&log).unwrap();
        assert!(same_file(handle.as_raw_fd(), &log));
        assert!(!same_file(handle.as_raw_fd(), &other));
        assert!(!same_file(handle.as_raw_fd(), &dir.join("missing.log")));

        // A second descriptor on the same path is still the same file.
        let again = open_append(&log).unwrap();
        assert!(same_file(again.as_raw_fd(), &log));

        drop(handle);
        drop(again);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn timestamps_match_the_log_format() {
        assert_eq!(at(0), "1970-01-01T00:00:00Z");
        assert_eq!(at(1_757_473_445), "2025-09-10T03:04:05Z");
        // 2024-02-29: a leap day.
        assert_eq!(at(1_709_208_000), "2024-02-29T12:00:00Z");
        assert_eq!(at(1_789_009_200), "2026-09-10T03:00:00Z");
    }
}
