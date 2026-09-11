//! LaunchDaemon install and uninstall.
//!
//! `install` copies the running binary to [`INSTALL_BIN_PATH`], writes the
//! plist and bootstraps it. `uninstall` reverses that and always leaves the
//! machine charging. Both need root and both are idempotent.

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

use crate::config::Config;
use crate::logging;
use crate::state::Daemon;

/// Where the installed daemon binary lives.
pub const INSTALL_BIN_PATH: &str = "/usr/local/libexec/chargecapd";

const BIN_MODE: u32 = 0o755;
const PLIST_MODE: u32 = 0o644;
const DIR_MODE: u32 = 0o755;
/// How long `install` waits for launchd to finish unloading the old daemon
/// and to accept the new one.
const LAUNCHD_TIMEOUT: Duration = Duration::from_secs(15);
/// How long `install` waits between two tries.
const LAUNCHD_POLL: Duration = Duration::from_millis(250);
const ROOT_UID: u32 = 0;
/// The `wheel` group.
const WHEEL_GID: u32 = 0;

/// Returns true when this process runs as root.
pub fn is_root() -> bool {
    // SAFETY: `geteuid` takes no arguments and cannot fail.
    unsafe { libc::geteuid() == 0 }
}

/// Fails unless this process runs as root.
pub fn require_root(command: &str) -> Result<()> {
    if is_root() {
        Ok(())
    } else {
        bail!("{command} needs root; run: sudo chargecapd {command}")
    }
}

/// Installs and bootstraps the LaunchDaemon.
pub fn install() -> Result<()> {
    require_root("install")?;

    let source = std::env::current_exe().context("cannot find the running binary")?;
    let target = Path::new(INSTALL_BIN_PATH);
    install_binary(&source, target)?;
    logging::info(format!("installed binary at {}", target.display()));

    let plist_path = Path::new(proto::DAEMON_PLIST_PATH);
    write_root_file(plist_path, plist(target).as_bytes(), PLIST_MODE)?;
    logging::info(format!("wrote plist at {}", plist_path.display()));

    // Make the log file reachable before launchd redirects into it.
    if let Some(dir) = Path::new(proto::LOG_PATH).parent() {
        create_dir(dir)?;
    }

    // Idempotent: drop any loaded copy before bootstrapping the new one.
    bootout();
    bootstrap(plist_path)?;
    logging::info(format!("bootstrapped {}", proto::DAEMON_LABEL));
    Ok(())
}

/// Bootstraps `plist_path` into the system domain, retrying while launchd
/// refuses.
///
/// WARNING: `launchctl bootout` returns before launchd has finished
/// unloading the daemon, and a `bootstrap` in that window fails with
/// "Bootstrap failed: 5: Input/output error". Retrying until
/// [`LAUNCHD_TIMEOUT`] rides out the unload instead of leaving the machine
/// with no daemon at all.
fn bootstrap(plist_path: &Path) -> Result<()> {
    let plist_path = plist_path.to_path_buf();
    let mut tries = 0;
    let result = retry_until(LAUNCHD_TIMEOUT, LAUNCHD_POLL, || {
        tries += 1;
        let output = Command::new("launchctl")
            .args(["bootstrap", "system"])
            .arg(&plist_path)
            .output()
            .map_err(|err| format!("cannot run launchctl bootstrap: {err}"))?;
        if output.status.success() {
            return Ok(());
        }
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    });
    match result {
        Ok(()) => {
            if tries > 1 {
                logging::info(format!("launchctl bootstrap succeeded on try {tries}"));
            }
            Ok(())
        }
        Err(message) => bail!("launchctl bootstrap failed after {tries} tries: {message}"),
    }
}

/// Runs `attempt` until it succeeds or `timeout` passes, pausing `poll`
/// between tries. `attempt` always runs at least once.
///
/// Returns the last failure message when every try failed.
fn retry_until<F>(timeout: Duration, poll: Duration, mut attempt: F) -> Result<(), String>
where
    F: FnMut() -> Result<(), String>,
{
    let deadline = Instant::now() + timeout;
    loop {
        match attempt() {
            Ok(()) => return Ok(()),
            Err(message) => {
                if Instant::now() + poll >= deadline {
                    return Err(message);
                }
                std::thread::sleep(poll);
            }
        }
    }
}

/// Removes the LaunchDaemon and leaves charging enabled.
pub fn uninstall(socket_path: &Path) -> Result<()> {
    require_root("uninstall")?;

    // WARNING: turn the limit off before the daemon goes away, so a machine
    // with a closed charge gate is never left behind.
    if socket_path.exists() {
        match crate::client::send(
            socket_path,
            &proto::Request::SetLimit {
                upper: proto::MAX_UPPER,
                lower: None,
            },
        ) {
            Ok(_) => logging::info("asked the running daemon to allow charging"),
            Err(err) => logging::warn(format!("cannot reach the daemon: {err}")),
        }
    }

    bootout();

    // The daemon is gone now, so reset the SMC from here as well.
    match smc::IoKitDriver::open() {
        Ok(driver) => {
            let mut daemon = Daemon::new(smc::Smc::new(driver), Config::default(), "/dev/null");
            daemon.reset_charge_control();
        }
        Err(err) => logging::error(format!("cannot open the SMC: {err}")),
    }

    for path in [
        PathBuf::from(proto::DAEMON_PLIST_PATH),
        PathBuf::from(INSTALL_BIN_PATH),
        socket_path.to_path_buf(),
    ] {
        match fs::remove_file(&path) {
            Ok(()) => logging::info(format!("removed {}", path.display())),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => logging::warn(format!("cannot remove {}: {err}", path.display())),
        }
    }
    // The config file stays, so a re-install keeps the user's limit.
    logging::info(format!("kept the config at {}", proto::CONFIG_PATH));
    Ok(())
}

/// Boots the daemon out. A daemon that is not loaded is not an error.
fn bootout() {
    let target = format!("system/{}", proto::DAEMON_LABEL);
    match Command::new("launchctl")
        .args(["bootout", &target])
        .output()
    {
        Ok(output) if output.status.success() => logging::info(format!("booted out {target}")),
        Ok(_) => logging::info(format!("{target} was not loaded")),
        Err(err) => logging::warn(format!("cannot run launchctl bootout: {err}")),
    }
}

/// Copies `source` over `target` through a temp file, so replacing the
/// binary of a running daemon cannot fail with `ETXTBSY`.
fn install_binary(source: &Path, target: &Path) -> Result<()> {
    if let Some(dir) = target.parent() {
        create_dir(dir)?;
    }
    let bytes = fs::read(source)
        .with_context(|| format!("cannot read the binary at {}", source.display()))?;
    write_root_file(target, &bytes, BIN_MODE)
}

/// Writes `bytes` to `path` atomically as root:wheel with mode `mode`.
fn write_root_file(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    let temp = path.with_extension(format!("tmp{}", std::process::id()));
    let write = || -> Result<()> {
        let mut file =
            fs::File::create(&temp).with_context(|| format!("cannot create {}", temp.display()))?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::set_permissions(&temp, fs::Permissions::from_mode(mode))?;
        chown_root(&temp)?;
        fs::rename(&temp, path)
            .with_context(|| format!("cannot move {} to {}", temp.display(), path.display()))?;
        Ok(())
    };
    let result = write();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

fn create_dir(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    fs::set_permissions(dir, fs::Permissions::from_mode(DIR_MODE))?;
    Ok(())
}

fn chown_root(path: &Path) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes())?;
    // SAFETY: `c_path` is a NUL-terminated path that `chown` only reads.
    let rc = unsafe { libc::chown(c_path.as_ptr(), ROOT_UID, WHEEL_GID) };
    if rc != 0 {
        bail!(
            "cannot chown {} to root:wheel: {}",
            path.display(),
            std::io::Error::last_os_error()
        );
    }
    Ok(())
}

/// Renders the LaunchDaemon plist for `binary`.
pub fn plist(binary: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{label}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{binary}</string>
		<string>daemon</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<true/>
	<key>ProcessType</key>
	<string>Interactive</string>
	<key>StandardOutPath</key>
	<string>{log}</string>
	<key>StandardErrorPath</key>
	<string>{log}</string>
</dict>
</plist>
"#,
        label = proto::DAEMON_LABEL,
        binary = binary.display(),
        log = proto::LOG_PATH,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_carries_every_required_key() {
        let text = plist(Path::new(INSTALL_BIN_PATH));
        for needle in [
            proto::DAEMON_LABEL,
            INSTALL_BIN_PATH,
            proto::LOG_PATH,
            "<key>RunAtLoad</key>",
            "<key>KeepAlive</key>",
            "<string>Interactive</string>",
            "<string>daemon</string>",
        ] {
            assert!(text.contains(needle), "plist is missing {needle}");
        }
    }

    /// Regression: `launchctl bootout` returns before launchd has finished
    /// unloading, so the bootstrap that follows failed with
    /// "Bootstrap failed: 5: Input/output error" and left no daemon.
    #[test]
    fn retry_until_rides_out_a_few_failures() {
        let mut calls = 0;
        let result = retry_until(Duration::from_secs(5), Duration::from_millis(1), || {
            calls += 1;
            if calls < 3 {
                Err("Bootstrap failed: 5: Input/output error".to_string())
            } else {
                Ok(())
            }
        });
        assert_eq!(result, Ok(()));
        assert_eq!(calls, 3);
    }

    #[test]
    fn retry_until_runs_once_and_reports_the_last_failure() {
        let mut calls = 0;
        let result = retry_until(Duration::from_millis(30), Duration::from_millis(10), || {
            calls += 1;
            Err(format!("try {calls} failed"))
        });
        assert_eq!(result, Err(format!("try {calls} failed")));
        assert!(calls >= 1, "the attempt must run at least once");

        // A timeout below the poll interval still runs the attempt once.
        let mut once = 0;
        let result = retry_until(Duration::ZERO, Duration::from_secs(60), || {
            once += 1;
            Err("no".to_string())
        });
        assert_eq!(result, Err("no".to_string()));
        assert_eq!(once, 1);
    }

    #[test]
    fn install_and_uninstall_need_root() {
        if is_root() {
            return; // The test suite is not expected to run as root.
        }
        let message = require_root("install").unwrap_err().to_string();
        assert!(message.contains("needs root"));
        assert!(install().is_err());
        assert!(uninstall(Path::new("/nonexistent.sock")).is_err());
    }
}
