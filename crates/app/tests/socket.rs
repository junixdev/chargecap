//! Integration tests: the app client against the fake daemon.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use app::{client, fake};
use proto::Status;

/// Starts the fake daemon on a temp socket and returns the path.
///
/// The listener thread dies with the test process.
fn start(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("chargecap-{name}-{}.sock", std::process::id()));
    let listener = fake::listen(&path).expect("bind the fake daemon socket");
    let status = Arc::new(Mutex::new(fake::canned_status()));
    std::thread::spawn(move || fake::serve(listener, status));
    // Give the accept loop a moment before the first connect.
    std::thread::sleep(Duration::from_millis(50));
    path
}

#[test]
fn status_returns_the_canned_status() {
    let path = start("status");
    let status: Status = client::status(&path).expect("status");
    assert_eq!(status.battery_percent, 78);
    assert_eq!(status.upper, proto::DEFAULT_UPPER);
    assert_eq!(status.version, proto::VERSION);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn set_limit_changes_the_upper_limit() {
    let path = start("set-limit");
    let status = client::set_limit(&path, 65).expect("set_limit");
    assert_eq!(status.upper, 65);
    assert_eq!(status.lower, 65 - proto::DEFAULT_GAP);
    // The change sticks for the next request.
    assert_eq!(client::status(&path).expect("status").upper, 65);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn set_limit_to_100_turns_the_limit_off() {
    let path = start("limit-off");
    let status = client::set_limit(&path, proto::MAX_UPPER).expect("set_limit");
    assert_eq!(status.upper, proto::MAX_UPPER);
    assert_eq!(status.lower, proto::MAX_UPPER);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn the_daemon_rejects_an_out_of_range_limit() {
    let path = start("bad-limit");
    let error = client::set_limit(&path, 10).unwrap_err();
    assert_eq!(error.to_string(), "upper must be 50..=100");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_missing_socket_is_an_error_not_a_panic() {
    let path = std::env::temp_dir().join("chargecap-absent.sock");
    let _ = std::fs::remove_file(&path);
    assert!(client::status(&path).is_err());
}
