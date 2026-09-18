//! Self-update from GitHub releases.
//!
//! Blocking helpers — call on the background pool. The install step swaps the
//! bundle from a detached shell after the app quits, then relaunches.

use std::path::{Path, PathBuf};
use std::process::Command;

const RELEASES_API: &str = "https://api.github.com/repos/bobbycoleman-dev/oxide/releases/latest";

pub struct ReleaseInfo {
    pub version: String,
    pub dmg_url: String,
}

fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let v = v.trim().trim_start_matches('v');
    let mut parts = v.splitn(3, '.').map(|p| {
        p.chars()
            .take_while(|c| c.is_ascii_digit())
            .collect::<String>()
            .parse::<u64>()
            .ok()
    });
    Some((parts.next()??, parts.next()??, parts.next()??))
}

pub fn is_newer(remote: &str, local: &str) -> bool {
    match (parse_version(remote), parse_version(local)) {
        (Some(r), Some(l)) => r > l,
        _ => false,
    }
}

/// Query the latest release. Returns None when there's no DMG asset yet.
pub fn fetch_latest() -> Result<Option<ReleaseInfo>, String> {
    let out = Command::new("curl")
        .args([
            "-sSL",
            "--max-time",
            "20",
            "-H",
            "Accept: application/vnd.github+json",
            "-H",
            "User-Agent: oxide-terminal",
            RELEASES_API,
        ])
        .output()
        .map_err(|e| format!("update check failed: {e}"))?;
    if !out.status.success() {
        return Err("update check failed: network unreachable".into());
    }
    let json: serde_json::Value =
        serde_json::from_slice(&out.stdout).map_err(|e| format!("update check failed: {e}"))?;
    // A 404 body ({"message": "Not Found"}) means no releases yet — not an error.
    let Some(tag) = json["tag_name"].as_str() else {
        return Ok(None);
    };
    let Some(dmg_url) = dmg_url(&json) else {
        return Ok(None);
    };
    Ok(Some(ReleaseInfo {
        version: tag.trim_start_matches('v').to_string(),
        dmg_url: dmg_url.to_string(),
    }))
}

/// Releases carry the same DMG twice: `Oxide-x.y.z-update.dmg` for the
/// updater and `Oxide-x.y.z.dmg` for the website, so GitHub's per-asset
/// download counts keep installs and updates apart. Older releases only have
/// the plain one.
fn dmg_url(release: &serde_json::Value) -> Option<&str> {
    let assets = release["assets"].as_array()?;
    let named = |suffix: &str| {
        assets
            .iter()
            .find(|a| a["name"].as_str().is_some_and(|n| n.ends_with(suffix)))
    };
    named("-update.dmg").or_else(|| named(".dmg"))?["browser_download_url"].as_str()
}

/// The version that ran last time, when this binary is newer than it — i.e.
/// the first launch after an update. Records the current version either way,
/// so a second window opened at startup sees nothing.
pub fn note_launch_version() -> Option<String> {
    let path = directories::BaseDirs::new()?
        .home_dir()
        .join(".cache/oxide/last_version.txt");
    let current = env!("CARGO_PKG_VERSION");
    let previous = std::fs::read_to_string(&path)
        .ok()
        .map(|s| s.trim().to_string());
    if previous.as_deref() != Some(current) {
        let _ = std::fs::create_dir_all(path.parent()?);
        let _ = std::fs::write(&path, current);
    }
    updated_from(previous.as_deref(), current)
}

fn updated_from(previous: Option<&str>, current: &str) -> Option<String> {
    previous
        .filter(|p| is_newer(current, p))
        .map(str::to_string)
}

fn updates_dir() -> Option<PathBuf> {
    let dir = directories::BaseDirs::new()?
        .home_dir()
        .join(".cache/oxide/updates");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

pub fn download(info: &ReleaseInfo) -> Result<PathBuf, String> {
    let dir = updates_dir().ok_or("no cache directory")?;
    let dest = dir.join(format!("Oxide-{}.dmg", info.version));
    let partial = dir.join(format!("Oxide-{}.dmg.partial", info.version));
    if dest.exists() {
        return Ok(dest);
    }
    let status = Command::new("curl")
        .args(["-fsSL", "--max-time", "600", "-o"])
        .arg(&partial)
        .arg(&info.dmg_url)
        .status()
        .map_err(|e| format!("download failed: {e}"))?;
    if !status.success() {
        let _ = std::fs::remove_file(&partial);
        return Err("update download failed".into());
    }
    std::fs::rename(&partial, &dest).map_err(|e| format!("download failed: {e}"))?;
    Ok(dest)
}

/// The .app bundle this process is running from, if any.
pub fn installed_bundle() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let bundle = exe.parent()?.parent()?.parent()?;
    (bundle.extension().and_then(|e| e.to_str()) == Some("app")).then(|| bundle.to_path_buf())
}

fn sh_quote(p: &Path) -> String {
    format!("'{}'", p.to_string_lossy().replace('\'', r"'\''"))
}

/// Kick off the swap-and-relaunch script. The caller should quit the app
/// immediately after this returns Ok — the script waits for us to exit,
/// replaces the bundle, and reopens it.
pub fn install_and_restart(dmg: &Path) -> Result<(), String> {
    let Some(bundle) = installed_bundle() else {
        // Not running from an installed bundle (e.g. cargo run): hand the DMG
        // to the user for a drag install instead of guessing a destination.
        Command::new("open")
            .arg(dmg)
            .status()
            .map_err(|e| e.to_string())?;
        return Ok(());
    };
    let script = format!(
        r#"
sleep 1
MOUNT=$(mktemp -d)
hdiutil attach -nobrowse -readonly -mountpoint "$MOUNT" {dmg} || exit 1
APP=$(/bin/ls -d "$MOUNT"/*.app 2>/dev/null | head -1)
if [ -n "$APP" ]; then
  rm -rf {staging}
  ditto "$APP" {staging} && rm -rf {bundle} && mv {staging} {bundle}
fi
hdiutil detach "$MOUNT" -quiet || hdiutil detach "$MOUNT" -force || true
open {bundle}
"#,
        dmg = sh_quote(dmg),
        bundle = sh_quote(&bundle),
        staging = sh_quote(&bundle.with_extension("app.updating")),
    );
    Command::new("/bin/bash")
        .arg("-c")
        .arg(script)
        .spawn()
        .map_err(|e| format!("couldn't start installer: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn updated_from_only_reports_upgrades() {
        assert_eq!(updated_from(Some("0.5.0"), "0.5.1"), Some("0.5.0".into()));
        assert_eq!(updated_from(Some("0.5.1"), "0.5.1"), None, "same version");
        assert_eq!(
            updated_from(Some("0.6.0"), "0.5.1"),
            None,
            "dev build older than installed"
        );
        assert_eq!(updated_from(None, "0.5.1"), None, "fresh install");
    }

    #[test]
    fn prefers_update_dmg_and_falls_back() {
        let both = serde_json::json!({"assets": [
            {"name": "Oxide-1.0.0.dmg", "browser_download_url": "site"},
            {"name": "Oxide-1.0.0-update.dmg", "browser_download_url": "update"},
        ]});
        assert_eq!(dmg_url(&both), Some("update"));
        let old = serde_json::json!({"assets": [{"name": "Oxide-0.5.0.dmg", "browser_download_url": "site"}]});
        assert_eq!(dmg_url(&old), Some("site"));
        assert_eq!(dmg_url(&serde_json::json!({"assets": []})), None);
    }

    #[test]
    fn version_comparison() {
        assert!(is_newer("v0.2.0", "0.1.0"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("v0.1.0", "0.2.0"));
        assert!(!is_newer("garbage", "0.1.0"));
    }
}
