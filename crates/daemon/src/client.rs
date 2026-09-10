//! Socket client for the `status` and `limit` sub-commands.
//!
//! The client speaks the same one-line contract as the menu-bar app: connect,
//! write one [`proto::Request`], read one [`proto::Response`], close.

use std::io::{BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use proto::{Request, Response};

/// How long the client waits for the daemon to answer.
const TIMEOUT: Duration = Duration::from_secs(5);

/// Message printed when nothing is listening on the socket.
pub const NOT_RUNNING: &str = "daemon not running; run: sudo chargecapd install";

/// Sends `request` to the daemon at `path` and returns the raw reply line.
pub fn send_raw(path: &Path, request: &Request) -> Result<String> {
    use std::io::BufRead;

    let stream = UnixStream::connect(path).with_context(|| NOT_RUNNING.to_string())?;
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
        anyhow::bail!("the daemon sent an empty reply");
    }
    Ok(line)
}

/// Sends `request` and decodes the reply.
pub fn send(path: &Path, request: &Request) -> Result<Response> {
    let line = send_raw(path, request)?;
    serde_json::from_str(line.trim_end())
        .with_context(|| format!("cannot decode the reply: {}", line.trim_end()))
}

/// Runs one client command and prints the result.
///
/// With `json`, the raw reply line is printed as it came off the socket.
/// Otherwise the status is printed as pretty JSON.
pub fn run(path: &Path, request: &Request, json: bool) -> Result<()> {
    let line = send_raw(path, request)?;
    if json {
        print!("{line}");
        let response: Response = serde_json::from_str(line.trim_end())?;
        if !response.ok {
            anyhow::bail!(response
                .error
                .unwrap_or_else(|| "request failed".to_string()));
        }
        return Ok(());
    }

    let response: Response = serde_json::from_str(line.trim_end())
        .with_context(|| format!("cannot decode the reply: {}", line.trim_end()))?;
    match (response.ok, response.status, response.error) {
        (true, Some(status), _) => {
            println!("{}", serde_json::to_string_pretty(&status)?);
            Ok(())
        }
        (_, _, Some(error)) => anyhow::bail!(error),
        _ => anyhow::bail!("the daemon sent a reply with no status and no error"),
    }
}
