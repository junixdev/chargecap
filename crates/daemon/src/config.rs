//! On-disk daemon configuration.
//!
//! The config is a small JSON object at [`proto::CONFIG_PATH`]. A missing
//! file means defaults. Writes are atomic: the daemon writes a temp file in
//! the same directory and renames it over the target.

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use proto::MagsafeLedMode;

/// Mode of the config file.
const CONFIG_MODE: u32 = 0o644;
/// Mode of the directory that holds the config file.
const DIR_MODE: u32 = 0o755;

/// Everything the daemon persists between runs.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Config {
    /// Charging stops at or above this percent. 100 disables the limit.
    pub upper: u8,
    /// Charging restarts below this percent.
    pub lower: u8,
    pub magsafe_led: MagsafeLedMode,
    pub adapter_enabled: bool,
    pub top_up_active: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            upper: proto::DEFAULT_UPPER,
            lower: proto::DEFAULT_UPPER - proto::DEFAULT_GAP,
            magsafe_led: MagsafeLedMode::System,
            adapter_enabled: true,
            top_up_active: false,
        }
    }
}

impl Config {
    /// Reads the config at `path`. A missing file gives [`Config::default`].
    pub fn load(path: &Path) -> io::Result<Self> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(err) => return Err(err),
        };
        serde_json::from_str(&text).map_err(io::Error::from)
    }

    /// Writes the config to `path` atomically, mode 0644.
    ///
    /// The parent directory is created with mode 0755 if it is absent. The
    /// temp file is removed on every failure path, so a failed save never
    /// leaves a stray file behind.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            if !dir.exists() {
                fs::create_dir_all(dir)?;
                fs::set_permissions(dir, fs::Permissions::from_mode(DIR_MODE))?;
            }
        }
        let temp = temp_path(path);
        let mut text = serde_json::to_string_pretty(self).map_err(io::Error::from)?;
        text.push('\n');
        match write_temp(&temp, text.as_bytes()).and_then(|()| fs::rename(&temp, path)) {
            Ok(()) => Ok(()),
            Err(err) => {
                let _ = fs::remove_file(&temp);
                Err(err)
            }
        }
    }
}

/// Returns the temp file name used beside `path`.
fn temp_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "config.json".to_string());
    let temp = format!(".{name}.{}.tmp", std::process::id());
    match path.parent() {
        Some(dir) => dir.join(temp),
        None => PathBuf::from(temp),
    }
}

fn write_temp(temp: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    let mut file = fs::File::create(temp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::set_permissions(temp, fs::Permissions::from_mode(CONFIG_MODE))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Returns a unique empty directory under the OS temp directory.
    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "chargecap-test-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn missing_file_gives_defaults() {
        let dir = temp_dir("missing");
        let config = Config::load(&dir.join("config.json")).unwrap();
        assert_eq!(config, Config::default());
        assert_eq!(config.upper, 80);
        assert_eq!(config.lower, 78);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn round_trips_through_the_file() {
        let dir = temp_dir("round-trip");
        let path = dir.join("config.json");
        let config = Config {
            upper: 90,
            lower: 85,
            magsafe_led: MagsafeLedMode::Reflect,
            adapter_enabled: false,
            top_up_active: true,
        };
        config.save(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap(), config);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn save_creates_the_parent_directory() {
        let dir = temp_dir("parent");
        let path = dir.join("nested").join("config.json");
        Config::default().save(&path).unwrap();
        assert!(path.exists());
        let mode = fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, DIR_MODE);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn save_leaves_no_temp_file_and_uses_mode_0644() {
        let dir = temp_dir("atomic");
        let path = dir.join("config.json");
        Config::default().save(&path).unwrap();
        Config::default().save(&path).unwrap();
        let names: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["config.json".to_string()]);
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, CONFIG_MODE);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn wire_format_matches_the_brief() {
        let json = r#"{"upper":80,"lower":78,"magsafe_led":"system","adapter_enabled":true,"top_up_active":false}"#;
        assert_eq!(
            serde_json::from_str::<Config>(json).unwrap(),
            Config::default()
        );
    }
}
