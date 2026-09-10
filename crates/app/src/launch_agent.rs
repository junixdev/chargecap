//! Launch-at-login through a per-user LaunchAgent.
//!
//! The agent plist lives at `~/Library/LaunchAgents/<APP_LABEL>.plist` and
//! runs this binary with `RunAtLoad`. The menu checkmark follows the presence
//! of that file.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};

/// Environment variable that overrides the plist path, for tests.
pub const PLIST_ENV: &str = "CHARGECAP_AGENT_PLIST";

/// Returns the LaunchAgent plist path.
pub fn plist_path() -> PathBuf {
    if let Some(value) = std::env::var_os(PLIST_ENV) {
        if !value.is_empty() {
            return PathBuf::from(value);
        }
    }
    let home = std::env::var_os("HOME").unwrap_or_default();
    PathBuf::from(home)
        .join("Library/LaunchAgents")
        .join(format!("{}.plist", proto::APP_LABEL))
}

/// Returns true when the LaunchAgent is installed.
pub fn is_enabled(path: &Path) -> bool {
    path.is_file()
}

/// Writes the plist and loads it.
pub fn enable(path: &Path, binary: &Path) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    }
    std::fs::write(path, plist(binary))
        .with_context(|| format!("cannot write {}", path.display()))?;
    // A load failure is not fatal: the plist still runs at the next login.
    let _ = launchctl(["load", "-w"], path);
    Ok(())
}

/// Unloads the LaunchAgent and removes the plist.
pub fn disable(path: &Path) -> Result<()> {
    let _ = launchctl(["unload", "-w"], path);
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("cannot remove {}", path.display())),
    }
}

fn launchctl(args: [&str; 2], path: &Path) -> std::io::Result<std::process::Output> {
    Command::new("launchctl").args(args).arg(path).output()
}

/// Renders the LaunchAgent plist that runs `binary` at login.
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
	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>ProcessType</key>
	<string>Interactive</string>
</dict>
</plist>
"#,
        label = proto::APP_LABEL,
        binary = binary.display(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_names_the_label_and_the_binary() {
        let binary = std::env::current_exe().unwrap();
        let text = plist(&binary);
        assert!(text.contains(proto::APP_LABEL));
        assert!(text.contains(&binary.display().to_string()));
        assert!(text.contains("<key>RunAtLoad</key>"));
    }

    #[test]
    fn is_enabled_follows_the_file() {
        let dir = std::env::temp_dir().join(format!("chargecap-agent-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("agent.plist");
        assert!(!is_enabled(&path));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, plist(Path::new("/usr/local/bin/chargecap"))).unwrap();
        assert!(is_enabled(&path));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
