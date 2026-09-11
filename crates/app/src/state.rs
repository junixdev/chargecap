//! Small on-disk state for the app: the last limit the user picked and the
//! update-check settings.
//!
//! Kept in `~/Library/Application Support/chargecap/app.json`, so turning the
//! limit off and on again restores the same percentage.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Environment variable that overrides the state file, for tests.
pub const STATE_ENV: &str = "CHARGECAP_APP_STATE";

/// Persisted app state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppState {
    /// The last limit below 100 the user chose.
    pub last_upper: u8,
    /// Whether the app asks GitHub for a newer release once a day.
    pub check_updates: bool,
    /// Unix time of the last automatic update check, in seconds.
    pub last_update_check: u64,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            last_upper: proto::DEFAULT_UPPER,
            check_updates: true,
            last_update_check: 0,
        }
    }
}

/// Returns the state file path.
pub fn state_path() -> PathBuf {
    if let Some(value) = std::env::var_os(STATE_ENV) {
        if !value.is_empty() {
            return PathBuf::from(value);
        }
    }
    let home = std::env::var_os("HOME").unwrap_or_default();
    PathBuf::from(home)
        .join("Library/Application Support/chargecap")
        .join("app.json")
}

/// Reads the state at `path`, falling back to the default.
///
/// A missing or damaged file is not an error: the app starts with defaults.
pub fn load(path: &Path) -> AppState {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Writes `state` to `path`, creating the directory when needed.
pub fn save(path: &Path, state: &AppState) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    }
    let text = serde_json::to_string_pretty(state)?;
    std::fs::write(path, text).with_context(|| format!("cannot write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("chargecap-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn load_returns_the_default_when_the_file_is_missing() {
        let path = temp_dir("state-missing").join("app.json");
        assert_eq!(load(&path), AppState::default());
        assert_eq!(AppState::default().last_upper, proto::DEFAULT_UPPER);
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = temp_dir("state-round-trip");
        let path = dir.join("app.json");
        let state = AppState {
            last_upper: 65,
            check_updates: false,
            last_update_check: 1_700_000_000,
        };
        save(&path, &state).unwrap();
        assert_eq!(load(&path), state);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_fills_in_fields_an_older_file_does_not_have() {
        let dir = temp_dir("state-older");
        let path = dir.join("app.json");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, r#"{"last_upper": 70}"#).unwrap();
        let state = load(&path);
        assert_eq!(state.last_upper, 70);
        assert!(state.check_updates);
        assert_eq!(state.last_update_check, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_ignores_a_damaged_file() {
        let dir = temp_dir("state-damaged");
        let path = dir.join("app.json");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, "not json").unwrap();
        assert_eq!(load(&path), AppState::default());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
