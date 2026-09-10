//! Line logging to stderr and to [`proto::LOG_PATH`].
//!
//! Lines look like `[2026-09-10T03:04:05Z] INFO started`. The file handle is
//! optional: when the daemon cannot open the log file, for example in an
//! unprivileged dev run, it still logs to stderr.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

static LOG_FILE: OnceLock<Option<Mutex<File>>> = OnceLock::new();

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
    eprintln!("{line}");
    if let Some(Some(file)) = LOG_FILE.get() {
        if let Ok(mut file) = file.lock() {
            let _ = writeln!(file, "{line}");
        }
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
    use std::time::Duration;

    fn at(secs: u64) -> String {
        timestamp(UNIX_EPOCH + Duration::from_secs(secs))
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
