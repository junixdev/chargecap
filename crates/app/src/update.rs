//! Update check and install against the GitHub Releases of this repo.
//!
//! The pure parts, version parsing and picking the newest release from the
//! API JSON, are tested here. The network and install steps shell out to the
//! tools every Mac ships with: `curl`, `hdiutil`, `ditto`, `osascript` and
//! `open`. No TLS stack is linked into the app.
//!
//! Install flow, run on a worker thread:
//!
//! 1. download the `.dmg` asset into the user's cache directory,
//! 2. attach it read-only at a private mount point,
//! 3. replace `/Applications/chargecap.app` with the copy inside,
//! 4. re-install the root daemon from the new bundle, through an admin
//!    prompt, because the daemon binary ships inside the bundle,
//! 5. detach the image and relaunch the app.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

/// GitHub repository the app checks for releases.
pub const REPO: &str = "junixdev/chargecap";

/// The version compiled into this binary.
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

/// How often the app checks for a new release when automatic checks are on.
pub const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// Where the app is expected to live for an in-place update.
pub const APP_PATH: &str = "/Applications/chargecap.app";

/// A semantic version: `MAJOR.MINOR.PATCH` with an optional pre-release tag.
///
/// Only what release tags need: numeric triple and dotted pre-release
/// identifiers, compared as in the SemVer spec. Build metadata is ignored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pub pre: Vec<String>,
}

impl Version {
    /// Parses `1.2.3`, `v1.2.3`, `1.2.3-beta.1` or `v1.2.3-rc.2+build`.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim().trim_start_matches('v');
        let text = text.split('+').next()?;
        let (core, pre) = match text.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (text, None),
        };
        let mut parts = core.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        let patch = parts.next()?.parse().ok()?;
        if parts.next().is_some() {
            return None;
        }
        let pre = match pre {
            Some("") => return None,
            Some(pre) => pre.split('.').map(str::to_string).collect(),
            None => Vec::new(),
        };
        Some(Self {
            major,
            minor,
            patch,
            pre,
        })
    }

    /// True for `1.2.3-beta.1`, false for `1.2.3`.
    pub fn is_prerelease(&self) -> bool {
        !self.pre.is_empty()
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if self.is_prerelease() {
            write!(f, "-{}", self.pre.join("."))?;
        }
        Ok(())
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        let core =
            (self.major, self.minor, self.patch).cmp(&(other.major, other.minor, other.patch));
        if core != Ordering::Equal {
            return core;
        }
        // A release is newer than any of its pre-releases.
        match (self.pre.is_empty(), other.pre.is_empty()) {
            (true, true) => return Ordering::Equal,
            (true, false) => return Ordering::Greater,
            (false, true) => return Ordering::Less,
            (false, false) => {}
        }
        for (a, b) in self.pre.iter().zip(&other.pre) {
            let ord = match (a.parse::<u64>(), b.parse::<u64>()) {
                (Ok(a), Ok(b)) => a.cmp(&b),
                // Numeric identifiers sort before alphanumeric ones.
                (Ok(_), Err(_)) => Ordering::Less,
                (Err(_), Ok(_)) => Ordering::Greater,
                (Err(_), Err(_)) => a.cmp(b),
            };
            if ord != Ordering::Equal {
                return ord;
            }
        }
        self.pre.len().cmp(&other.pre.len())
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// A release that is newer than the running app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    /// The GitHub tag, for example `v0.2.0`.
    pub tag: String,
    /// The release page, opened when there is no `.dmg` to install.
    pub page_url: String,
    /// Direct download of the `.dmg` asset, when the release has one.
    pub dmg_url: Option<String>,
}

#[derive(Deserialize)]
struct ApiRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<ApiAsset>,
}

#[derive(Deserialize)]
struct ApiAsset {
    name: String,
    browser_download_url: String,
}

/// Picks the newest release in `json` that is newer than `current`.
///
/// `json` is the body of `GET /repos/{owner}/{repo}/releases`. Drafts are
/// skipped. Pre-releases count only when `current` is itself a pre-release,
/// so beta users follow the beta channel and everyone else sees stable
/// builds only.
pub fn newest(json: &str, current: &Version) -> Result<Option<Release>> {
    let releases: Vec<ApiRelease> =
        serde_json::from_str(json).context("cannot decode the GitHub releases list")?;
    let mut best: Option<Release> = None;
    for release in releases {
        if release.draft || (release.prerelease && !current.is_prerelease()) {
            continue;
        }
        let Some(version) = Version::parse(&release.tag_name) else {
            continue;
        };
        if version <= *current {
            continue;
        }
        if best.as_ref().is_some_and(|b| b.version >= version) {
            continue;
        }
        let dmg_url = release
            .assets
            .iter()
            .find(|a| a.name.ends_with(".dmg"))
            .map(|a| a.browser_download_url.clone());
        best = Some(Release {
            version,
            tag: release.tag_name,
            page_url: release.html_url,
            dmg_url,
        });
    }
    Ok(best)
}

/// Returns the `.app` bundle that contains `exe`, if it runs from one.
///
/// `/Applications/chargecap.app/Contents/MacOS/chargecap` gives
/// `/Applications/chargecap.app`. A `cargo run` binary gives `None`.
pub fn bundle_of(exe: &Path) -> Option<PathBuf> {
    let bundle = exe.parent()?.parent()?.parent()?;
    let is_app = bundle
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("app"));
    is_app.then(|| bundle.to_path_buf())
}

/// Asks GitHub for the releases list and returns the newest update, if any.
pub fn check(current: &Version) -> Result<Option<Release>> {
    let url = format!("https://api.github.com/repos/{REPO}/releases?per_page=20");
    let output = Command::new("curl")
        .args(["-fsSL", "--max-time", "30"])
        .args(["-H", "Accept: application/vnd.github+json"])
        .args(["-H", "X-GitHub-Api-Version: 2022-11-28"])
        .args(["-A", &format!("chargecap/{CURRENT}")])
        .arg(&url)
        .output()
        .context("cannot run curl")?;
    if !output.status.success() {
        bail!(
            "cannot reach GitHub: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    newest(&String::from_utf8_lossy(&output.stdout), current)
}

/// Downloads `url` into the user's cache directory and returns the file.
pub fn download(url: &str) -> Result<PathBuf> {
    let home = std::env::var_os("HOME").unwrap_or_default();
    let dir = PathBuf::from(home).join("Library/Caches/chargecap");
    std::fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let name = url.rsplit('/').next().unwrap_or("update.dmg");
    let dest = dir.join(name);
    let status = Command::new("curl")
        .args(["-fsSL", "--max-time", "600", "-o"])
        .arg(&dest)
        .arg(url)
        .status()
        .context("cannot run curl")?;
    if !status.success() {
        bail!("the download failed ({status})");
    }
    Ok(dest)
}

/// Installs the app inside `dmg` over `app_path`, re-installs the daemon
/// from the new bundle through an admin prompt, and relaunches the app.
///
/// Returns once the new instance has been asked to start. The caller must
/// then quit, so two menu-bar items do not stay up.
pub fn install(dmg: &Path, app_path: &Path) -> Result<()> {
    let mount = std::env::temp_dir().join(format!("chargecap-update-{}", std::process::id()));
    std::fs::create_dir_all(&mount)?;
    let attached = run(
        Command::new("hdiutil")
            .args(["attach", "-nobrowse", "-readonly", "-quiet", "-mountpoint"])
            .arg(&mount)
            .arg(dmg),
        "cannot open the downloaded disk image",
    );
    let result = attached.and_then(|()| replace_and_reinstall(&mount, app_path));
    // Always try to detach; a stuck mount point is worse than a lost error.
    let _ = Command::new("hdiutil")
        .args(["detach", "-quiet", "-force"])
        .arg(&mount)
        .status();
    let _ = std::fs::remove_dir(&mount);
    let _ = std::fs::remove_file(dmg);
    result?;
    run(
        Command::new("open").arg("-n").arg(app_path),
        "the update is installed, but the app did not relaunch",
    )
}

fn replace_and_reinstall(mount: &Path, app_path: &Path) -> Result<()> {
    let source = std::fs::read_dir(mount)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| p.extension().is_some_and(|ext| ext == "app"))
        .context("the disk image has no .app inside")?;
    let daemon = source.join("Contents/MacOS/chargecapd");
    if !daemon.is_file() {
        bail!("the disk image does not contain chargecapd");
    }
    if app_path.exists() {
        std::fs::remove_dir_all(app_path)
            .with_context(|| format!("cannot remove the old {}", app_path.display()))?;
    }
    run(
        Command::new("ditto").arg(&source).arg(app_path),
        "cannot copy the new app into Applications",
    )?;
    // The daemon lives inside the bundle, so the new one must be installed
    // too. `chargecapd install` needs root; osascript shows the system
    // password prompt, which is what non-technical users expect to see.
    let script = format!(
        "do shell script \"{} install\" with administrator privileges",
        shell_quote(&app_path.join("Contents/MacOS/chargecapd"))
    );
    run(
        Command::new("osascript").args(["-e", &script]),
        "the new app is in place, but the helper was not updated",
    )
}

/// Runs `command` and turns a non-zero exit into an error with `what`.
fn run(command: &mut Command, what: &str) -> Result<()> {
    let output = command.output().with_context(|| what.to_string())?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let detail = stderr.trim();
    if detail.is_empty() {
        bail!("{what}");
    }
    bail!("{what}: {detail}")
}

/// Quotes `path` for use inside an AppleScript `do shell script` string.
fn shell_quote(path: &Path) -> String {
    let text = path
        .to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    format!("'{}'", text.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    #[test]
    fn parse_accepts_tags_and_plain_versions() {
        assert_eq!(v("v1.2.3").to_string(), "1.2.3");
        assert_eq!(v("1.2.3-beta.1").to_string(), "1.2.3-beta.1");
        assert_eq!(v("v0.1.0-rc.2+build.7").to_string(), "0.1.0-rc.2");
        assert!(Version::parse("1.2").is_none());
        assert!(Version::parse("1.2.3.4").is_none());
        assert!(Version::parse("1.2.3-").is_none());
        assert!(Version::parse("latest").is_none());
    }

    #[test]
    fn ordering_follows_semver() {
        assert!(v("0.2.0") > v("0.1.9"));
        assert!(v("1.0.0") > v("1.0.0-beta.1"));
        assert!(v("1.0.0-beta.2") > v("1.0.0-beta.1"));
        assert!(v("1.0.0-beta.10") > v("1.0.0-beta.9"));
        assert!(v("1.0.0-rc.1") > v("1.0.0-beta.9"));
        assert!(v("1.0.0-beta.1") > v("1.0.0-alpha"));
        assert!(v("1.0.0-alpha.1") > v("1.0.0-alpha"));
        assert_eq!(v("v1.0.0"), v("1.0.0"));
    }

    const RELEASES: &str = r#"[
      {"tag_name":"v0.3.0","html_url":"https://x/v0.3.0","draft":true,"prerelease":false,"assets":[]},
      {"tag_name":"v0.2.0-beta.1","html_url":"https://x/v0.2.0-beta.1","draft":false,"prerelease":true,
       "assets":[{"name":"chargecap-v0.2.0-beta.1-apple-silicon.dmg","browser_download_url":"https://dl/beta.dmg"}]},
      {"tag_name":"v0.1.5","html_url":"https://x/v0.1.5","draft":false,"prerelease":false,
       "assets":[{"name":"chargecap-v0.1.5-apple-silicon.zip","browser_download_url":"https://dl/zip"},
                 {"name":"chargecap-v0.1.5-apple-silicon.dmg","browser_download_url":"https://dl/stable.dmg"}]},
      {"tag_name":"v0.1.0","html_url":"https://x/v0.1.0","draft":false,"prerelease":false,"assets":[]},
      {"tag_name":"nightly","html_url":"https://x/nightly","draft":false,"prerelease":false,"assets":[]}
    ]"#;

    #[test]
    fn stable_users_only_see_stable_releases() {
        let release = newest(RELEASES, &v("0.1.0")).unwrap().unwrap();
        assert_eq!(release.tag, "v0.1.5");
        assert_eq!(release.dmg_url.as_deref(), Some("https://dl/stable.dmg"));
    }

    #[test]
    fn beta_users_see_the_newest_prerelease() {
        let release = newest(RELEASES, &v("0.1.0-beta.1")).unwrap().unwrap();
        assert_eq!(release.tag, "v0.2.0-beta.1");
        assert_eq!(release.dmg_url.as_deref(), Some("https://dl/beta.dmg"));
    }

    #[test]
    fn drafts_and_older_releases_are_ignored() {
        assert_eq!(newest(RELEASES, &v("0.1.5")).unwrap(), None);
        assert_eq!(newest(RELEASES, &v("9.0.0-beta.1")).unwrap(), None);
        assert_eq!(newest("[]", &v("0.1.0")).unwrap(), None);
        assert!(newest("not json", &v("0.1.0")).is_err());
    }

    #[test]
    fn a_release_without_a_dmg_still_counts() {
        let json = r#"[{"tag_name":"v0.9.0","html_url":"https://x/v0.9.0","assets":[]}]"#;
        let release = newest(json, &v("0.1.0")).unwrap().unwrap();
        assert_eq!(release.dmg_url, None);
        assert_eq!(release.page_url, "https://x/v0.9.0");
    }

    #[test]
    fn bundle_of_finds_the_app_around_the_binary() {
        assert_eq!(
            bundle_of(Path::new(
                "/Applications/chargecap.app/Contents/MacOS/chargecap"
            )),
            Some(PathBuf::from("/Applications/chargecap.app"))
        );
        assert_eq!(bundle_of(Path::new("/tmp/target/debug/chargecap")), None);
        assert_eq!(bundle_of(Path::new("chargecap")), None);
    }

    #[test]
    fn shell_quote_survives_spaces_and_quotes() {
        assert_eq!(shell_quote(Path::new("/a b/c")), "'/a b/c'");
        assert_eq!(shell_quote(Path::new("/it's")), r#"'/it'\''s'"#);
        assert_eq!(shell_quote(Path::new(r#"/say "hi""#)), r#"'/say \"hi\"'"#);
    }

    #[test]
    fn current_version_parses() {
        assert!(Version::parse(CURRENT).is_some());
    }
}
