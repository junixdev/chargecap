//! Development stand-in for `chargecapd`; see [`app::fake`].
//!
//! ```text
//! CHARGECAP_SOCKET=/tmp/cc-fake.sock cargo run -p app --bin fake-daemon
//! ```

use std::sync::{Arc, Mutex};

use anyhow::Result;

fn main() -> Result<()> {
    let path = app::client::socket_path();
    let listener = app::fake::listen(&path)?;
    eprintln!("fake-daemon: listening on {}", path.display());
    app::fake::serve(listener, Arc::new(Mutex::new(app::fake::canned_status())));
    Ok(())
}
