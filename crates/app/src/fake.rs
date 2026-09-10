//! Development stand-in for `chargecapd`.
//!
//! Serves the [`proto`] contract from an in-memory [`proto::Status`]: `status`
//! reads it, `set_limit` and the other commands change it. It never touches
//! the SMC, so it needs no root as long as the socket path is writable.
//!
//! The `fake-daemon` binary is a thin wrapper around this module; the
//! integration tests drive it in a thread.

use std::io::{BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use proto::{
    ChargeControlMode, MagsafeLedMode, Request, Response, Status, DEFAULT_GAP, DEFAULT_UPPER,
};

/// Binds the socket, replacing a stale one left by an earlier run.
pub fn listen(path: &Path) -> Result<UnixListener> {
    if path.exists() {
        std::fs::remove_file(path)
            .with_context(|| format!("cannot remove the stale socket {}", path.display()))?;
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).ok();
    }
    UnixListener::bind(path).with_context(|| format!("cannot bind {}", path.display()))
}

/// Accepts connections forever, one request per connection.
pub fn serve(listener: UnixListener, status: Arc<Mutex<Status>>) {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                if let Err(err) = handle(stream, &status) {
                    eprintln!("fake-daemon: {err}");
                }
            }
            Err(err) => eprintln!("fake-daemon: accept failed: {err}"),
        }
    }
}

fn handle(stream: UnixStream, status: &Mutex<Status>) -> Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let request: Request = proto::read_line_json(&mut reader)?;
    eprintln!("fake-daemon: {}", serde_json::to_string(&request)?);
    let response = {
        let mut status = status.lock().expect("status lock");
        apply(&mut status, &request)
    };
    let mut writer = stream;
    proto::write_response(&mut writer, &response)?;
    writer.flush()?;
    Ok(())
}

/// Applies `request` to `status` and returns the reply.
pub fn apply(status: &mut Status, request: &Request) -> Response {
    match request {
        Request::Status => {}
        Request::SetLimit { upper, lower } => match proto::validate_limits(*upper, *lower) {
            Ok((upper, lower)) => {
                status.upper = upper;
                status.lower = lower;
                status.charging_allowed = status.battery_percent < upper;
            }
            Err(message) => return Response::err(message),
        },
        Request::SetAdapter { enabled } => status.adapter_enabled = *enabled,
        Request::SetMagsafeLed { mode } => status.magsafe_led = *mode,
        Request::TopUp => status.top_up_active = true,
        Request::CancelTopUp => status.top_up_active = false,
    }
    Response::ok(status.clone())
}

/// The status the fake daemon starts from.
pub fn canned_status() -> Status {
    Status {
        version: proto::VERSION.to_string(),
        mode: ChargeControlMode::Legacy,
        battery_percent: 78,
        plugged_in: true,
        charging_allowed: false,
        adapter_enabled: true,
        upper: DEFAULT_UPPER,
        lower: DEFAULT_UPPER - DEFAULT_GAP,
        magsafe_led: MagsafeLedMode::System,
        top_up_active: false,
        last_error: None,
    }
}
