//! Unix-socket client for `chargecapd`.
//!
//! One request per connection: connect, write one [`proto::Request`], read one
//! [`proto::Response`], close. The socket path comes from `CHARGECAP_SOCKET`
//! when it is set, so a fake daemon can stand in during development.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use proto::{Request, Response, Status};

/// Environment variable that overrides [`proto::SOCKET_PATH`].
pub const SOCKET_ENV: &str = "CHARGECAP_SOCKET";

/// How long the app waits for the daemon to answer.
const TIMEOUT: Duration = Duration::from_secs(5);

/// Returns the socket to talk to.
pub fn socket_path() -> PathBuf {
    match std::env::var_os(SOCKET_ENV) {
        Some(value) if !value.is_empty() => PathBuf::from(value),
        _ => PathBuf::from(proto::SOCKET_PATH),
    }
}

/// Asks the daemon for its current status.
pub fn status(path: &Path) -> Result<Status> {
    send(path, &Request::Status)
}

/// Sets the upper limit and returns the status that follows.
///
/// `lower` is left to the daemon, which fills it in with
/// [`proto::validate_limits`].
pub fn set_limit(path: &Path, upper: u8) -> Result<Status> {
    send(path, &Request::SetLimit { upper, lower: None })
}

/// Sends one request and returns the status from the reply.
pub fn send(path: &Path, request: &Request) -> Result<Status> {
    let stream = UnixStream::connect(path)
        .with_context(|| format!("cannot reach the daemon at {}", path.display()))?;
    stream.set_read_timeout(Some(TIMEOUT))?;
    stream.set_write_timeout(Some(TIMEOUT))?;

    let mut writer = stream.try_clone()?;
    proto::write_request(&mut writer, request)?;
    writer.flush()?;

    let mut line = String::new();
    BufReader::new(stream)
        .read_line(&mut line)
        .context("the daemon closed the connection without answering")?;
    if line.trim().is_empty() {
        bail!("the daemon sent an empty reply");
    }

    let response: Response = serde_json::from_str(line.trim_end())
        .with_context(|| format!("cannot decode the reply: {}", line.trim_end()))?;
    match (response.ok, response.status, response.error) {
        (true, Some(status), _) => Ok(status),
        (_, _, Some(error)) => bail!(error),
        _ => bail!("the daemon sent a reply with no status and no error"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_path_falls_back_to_the_proto_default() {
        // The test binary keeps the variable unset, so this is the default.
        if std::env::var_os(SOCKET_ENV).is_none() {
            assert_eq!(socket_path(), PathBuf::from(proto::SOCKET_PATH));
        }
    }

    #[test]
    fn status_reports_a_missing_socket_instead_of_panicking() {
        let error = status(Path::new("/tmp/chargecap-does-not-exist.sock")).unwrap_err();
        assert!(error.to_string().contains("cannot reach the daemon"));
    }
}
