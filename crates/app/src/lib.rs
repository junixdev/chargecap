//! Support code for the `chargecap` menu-bar app.
//!
//! The app never touches the SMC. It talks to `chargecapd` over the Unix
//! socket contract in [`proto`]. The UI itself lives in `src/main.rs`; this
//! library holds the parts that can be tested without a menu bar.

pub mod client;
pub mod fake;
pub mod launch_agent;
pub mod state;
pub mod ui;
pub mod update;

#[cfg(target_os = "macos")]
pub mod dialog;
