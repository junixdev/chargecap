//! The Unix-socket API.
//!
//! One request per connection: the server reads one JSON line, applies it,
//! writes one [`proto::Response`] line and closes. The socket is mode 0666,
//! so the unprivileged menu-bar app can connect to a root daemon.

use std::fs;
use std::io::{self, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use proto::{Request, Response};
use smc::Driver;

use crate::logging;
use crate::state::Daemon;

/// Mode of the socket file. The app runs unprivileged.
const SOCKET_MODE: u32 = 0o666;

/// How long one connection may take before the server drops it.
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(5);

/// Binds the listener at `path`, replacing a stale socket file.
pub fn bind(path: &Path) -> io::Result<UnixListener> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    // A leftover socket file from a killed daemon blocks the bind.
    match fs::remove_file(path) {
        Ok(()) => logging::warn(format!("removed stale socket {}", path.display())),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(err),
    }
    let listener = UnixListener::bind(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(SOCKET_MODE))?;
    Ok(listener)
}

/// Accepts connections until `running` turns false.
///
/// The listener must be non-blocking, so the loop can notice shutdown.
pub fn serve<D: Driver>(listener: &UnixListener, daemon: &Mutex<Daemon<D>>, running: &AtomicBool) {
    while running.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => handle(stream, daemon),
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(err) => logging::error(format!("accept failed: {err}")),
        }
    }
}

/// Serves one connection. Errors are logged, never propagated: a bad client
/// must not stop the daemon.
pub fn handle<D: Driver>(stream: UnixStream, daemon: &Mutex<Daemon<D>>) {
    if let Err(err) = handle_inner(stream, daemon) {
        logging::error(format!("connection failed: {err}"));
    }
}

fn handle_inner<D: Driver>(stream: UnixStream, daemon: &Mutex<Daemon<D>>) -> io::Result<()> {
    stream.set_read_timeout(Some(CONNECTION_TIMEOUT))?;
    stream.set_write_timeout(Some(CONNECTION_TIMEOUT))?;
    // A blocking listener hands out blocking streams, but a non-blocking one
    // hands out non-blocking streams, which the line reader cannot use.
    stream.set_nonblocking(false)?;

    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);

    let response = match proto::read_line_json::<Request>(&mut reader) {
        Ok(request) => {
            let mut daemon = daemon.lock().unwrap_or_else(|err| err.into_inner());
            daemon.apply(request)
        }
        Err(err) => Response::err(format!("bad request: {err}")),
    };

    proto::write_response(&mut writer, &response)?;
    writer.flush()
}

/// The socket of the installed root daemon.
pub fn default_socket_path() -> std::path::PathBuf {
    std::path::PathBuf::from(proto::SOCKET_PATH)
}

/// The socket of an unprivileged dev run.
///
/// `/var/run` needs root, so a dev daemon binds here instead. On macOS
/// `TMPDIR` is a private per-user directory, so the daemon and the app of one
/// user agree on this path and no other user can take the name first.
pub fn dev_socket_path() -> std::path::PathBuf {
    std::env::temp_dir().join("chargecap-dev.sock")
}

/// Returns the socket a client should use.
///
/// The installed daemon wins. The dev socket is the fallback, so a dev run
/// needs no extra flag.
pub fn client_socket_path() -> std::path::PathBuf {
    let installed = default_socket_path();
    if installed.exists() {
        return installed;
    }
    let dev = dev_socket_path();
    if dev.exists() {
        return dev;
    }
    installed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use proto::MagsafeLedMode;
    use smc::{MockDriver, Smc, KEY_ACW, KEY_BUIC, KEY_CH0B, KEY_CH0C, KEY_CH0J};
    use std::path::PathBuf;

    fn socket_path(tag: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("cc-{tag}-{}.sock", std::process::id()));
        let _ = fs::remove_file(&path);
        path
    }

    fn test_daemon(config_path: &Path) -> Daemon<MockDriver> {
        let mock = MockDriver::new();
        mock.seed(KEY_CH0B, &[0x00])
            .seed(KEY_CH0C, &[0x00])
            .seed(KEY_BUIC, &[79])
            .seed(KEY_ACW, &[0x01])
            .seed(KEY_CH0J, &[0x00]);
        Daemon::new(Smc::new(mock), Config::default(), config_path)
    }

    /// Sends one raw line and returns the raw reply line.
    fn round_trip(path: &Path, line: &str) -> String {
        use std::io::BufRead;
        let mut stream = UnixStream::connect(path).expect("connect");
        stream.write_all(line.as_bytes()).unwrap();
        stream.write_all(b"\n").unwrap();
        stream.flush().unwrap();
        let mut reply = String::new();
        BufReader::new(stream).read_line(&mut reply).unwrap();
        reply
    }

    #[test]
    fn socket_serves_every_contract_request() {
        // `MockDriver` is not `Send`, so the server stays on this thread and
        // the client runs on the spawned one.
        let path = socket_path("contract");
        let config_path = std::env::temp_dir().join(format!("cc-{}.json", std::process::id()));
        let _ = fs::remove_file(&config_path);
        let listener = bind(&path).expect("bind");
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, SOCKET_MODE);

        let requests = vec![
            Request::Status,
            Request::SetLimit {
                upper: 85,
                lower: None,
            },
            // Below the fixture's 79% battery, so discharging does not
            // immediately re-enable the adapter once `SetAdapter` runs.
            Request::SetLimit {
                upper: 75,
                lower: Some(70),
            },
            Request::SetAdapter { enabled: false },
            Request::SetMagsafeLed {
                mode: MagsafeLedMode::Reflect,
            },
            Request::TopUp,
            Request::CancelTopUp,
        ];
        let count = requests.len() + 2;

        let client_path = path.clone();
        let client = std::thread::spawn(move || {
            let mut replies = Vec::new();
            for request in &requests {
                let line = serde_json::to_string(request).unwrap();
                replies.push(round_trip(&client_path, &line));
            }
            // An out-of-range limit and unknown JSON must both be refused.
            replies.push(round_trip(
                &client_path,
                r#"{"cmd":"set_limit","upper":49}"#,
            ));
            replies.push(round_trip(&client_path, r#"{"cmd":"explode"}"#));
            replies
        });

        let daemon = Mutex::new(test_daemon(&config_path));
        for stream in listener.incoming().take(count) {
            handle(stream.expect("accept"), &daemon);
        }
        let replies = client.join().expect("client thread");

        for reply in &replies[..count - 2] {
            let response: Response = serde_json::from_str(reply.trim_end()).expect("decodes");
            assert!(response.ok, "expected ok for {reply}");
            assert!(response.status.is_some());
            assert!(response.error.is_none());
        }

        let refused: Response = serde_json::from_str(replies[count - 2].trim_end()).unwrap();
        assert!(!refused.ok);
        assert_eq!(refused.error.as_deref(), Some("upper must be 50..=100"));

        let unknown: Response = serde_json::from_str(replies[count - 1].trim_end()).unwrap();
        assert!(!unknown.ok);
        assert!(unknown.error.unwrap().starts_with("bad request:"));

        // The last accepted limit is the one that stuck.
        let status = daemon.lock().unwrap().status();
        assert_eq!((status.upper, status.lower), (75, 70));
        assert_eq!(status.magsafe_led, MagsafeLedMode::Reflect);
        assert!(!status.adapter_enabled);
        assert!(!status.top_up_active);

        fs::remove_file(&path).unwrap();
        let _ = fs::remove_file(&config_path);
    }

    #[test]
    fn bind_replaces_a_stale_socket_file() {
        let path = socket_path("stale");
        fs::write(&path, b"stale").unwrap();
        let listener = bind(&path).expect("bind over the stale file");
        assert!(UnixStream::connect(&path).is_ok());
        drop(listener);
        fs::remove_file(&path).unwrap();
    }
}
