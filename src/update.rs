//! Self-update from GitHub Releases.
//!
//! The installed copy asks the GitHub API for the latest release, downloads `smowauncher.exe`
//! when it's newer, verifies it against the `smowauncher.exe.sha256` published with it, and
//! stages it next to the running exe. `apply` then swaps the files (a running exe can be
//! renamed on Windows), starts the new version with our own (elevated) token and returns so
//! the caller can quit. The new process waits for the old one to exit before taking over.

use serde::Deserialize;
use std::path::PathBuf;

pub const REPO: &str = "fi-smo/smowauncher";
const EXE_ASSET: &str = "smowauncher.exe";
const SHA_ASSET: &str = "smowauncher.exe.sha256";
const MAX_EXE_BYTES: usize = 64 * 1024 * 1024;

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

pub enum Outcome {
    UpToDate(String),
    /// Downloaded and verified; call `apply` to switch to it.
    Staged(String),
}

/// "v1.2.3" / "1.2.3" → (1, 2, 3). Anything after a '-' (pre-release tags) is ignored.
pub fn parse_version(v: &str) -> Option<(u32, u32, u32)> {
    let v = v.trim().trim_start_matches(['v', 'V']);
    let core = v.split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|p| p.parse::<u32>().ok());
    Some((parts.next()??, parts.next().flatten().unwrap_or(0), parts.next().flatten().unwrap_or(0)))
}

pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

fn install_exe() -> PathBuf {
    crate::platform::autostart::install_dir().join("smowauncher.exe")
}

fn staged_exe() -> PathBuf {
    crate::platform::autostart::install_dir().join("smowauncher.new.exe")
}

fn old_exe() -> PathBuf {
    crate::platform::autostart::install_dir().join("smowauncher.old.exe")
}

/// Only the installed copy updates itself (never a development build).
pub fn is_installed_copy() -> bool {
    let (Ok(me), Ok(installed)) = (std::env::current_exe(), std::fs::canonicalize(install_exe())) else { return false };
    std::fs::canonicalize(me).is_ok_and(|m| m == installed)
}

/// Removes the previous version left behind by an update.
pub fn cleanup() {
    let _ = std::fs::remove_file(old_exe());
}

fn sha256_hex(data: &[u8]) -> Result<String, String> {
    use windows::Win32::Security::Cryptography::{BCRYPT_SHA256_ALG_HANDLE, BCryptHash};
    let mut out = [0u8; 32];
    let status = unsafe { BCryptHash(BCRYPT_SHA256_ALG_HANDLE, None, data, &mut out) };
    if status.is_err() {
        return Err(format!("hashing failed: {status:?}"));
    }
    Ok(out.iter().map(|b| format!("{b:02x}")).collect())
}

/// Checks GitHub and, if there's a newer release, downloads and verifies it (blocking).
pub fn check_and_stage() -> Result<Outcome, String> {
    let body = crate::platform::http::get_url(&format!("https://api.github.com/repos/{REPO}/releases/latest"), 1024 * 1024)?;
    let release: Release = serde_json::from_slice(&body).map_err(|e| format!("bad release data: {e}"))?;
    let latest = parse_version(&release.tag_name).ok_or_else(|| format!("unrecognized tag {}", release.tag_name))?;
    let current = parse_version(current_version()).unwrap_or((0, 0, 0));
    let version = release.tag_name.trim_start_matches('v').to_owned();
    if release.draft || release.prerelease || latest <= current {
        return Ok(Outcome::UpToDate(current_version().to_owned()));
    }
    let url = |name: &str| {
        release.assets.iter().find(|a| a.name.eq_ignore_ascii_case(name)).map(|a| a.browser_download_url.clone())
    };
    let exe_url = url(EXE_ASSET).ok_or("release has no smowauncher.exe")?;
    let sha_url = url(SHA_ASSET).ok_or("release has no checksum file")?;

    let expected = String::from_utf8_lossy(&crate::platform::http::get_url(&sha_url, 4096)?)
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_lowercase();
    let exe = crate::platform::http::get_url(&exe_url, MAX_EXE_BYTES)?;
    let actual = sha256_hex(&exe)?;
    if expected.len() != 64 || actual != expected {
        return Err(format!("checksum mismatch for v{version} (expected {expected}, got {actual})"));
    }
    if exe.len() < 1024 * 1024 || &exe[..2] != b"MZ" {
        return Err("downloaded file isn't an executable".into());
    }
    std::fs::write(staged_exe(), &exe).map_err(|e| format!("saving update: {e}"))?;
    log::info!("update: v{version} downloaded and verified");
    Ok(Outcome::Staged(version))
}

/// Swaps in the staged exe and starts it. The caller must quit right after this returns Ok.
pub fn apply() -> Result<(), String> {
    let (current, staged, old) = (install_exe(), staged_exe(), old_exe());
    if !staged.exists() {
        return Err("no update staged".into());
    }
    let _ = std::fs::remove_file(&old);
    std::fs::rename(&current, &old).map_err(|e| format!("moving current exe aside: {e}"))?;
    if let Err(e) = std::fs::rename(&staged, &current) {
        let _ = std::fs::rename(&old, &current); // put things back
        return Err(format!("installing update: {e}"));
    }
    // Started directly (not through the task) so it inherits our token, elevation included.
    std::process::Command::new(&current)
        .arg("--after-update")
        .arg(std::process::id().to_string())
        .spawn()
        .map_err(|e| {
            let _ = std::fs::rename(&current, &staged);
            let _ = std::fs::rename(&old, &current);
            format!("starting new version: {e}")
        })?;
    log::info!("update: restarting into the new version");
    Ok(())
}

/// `--after-update <pid>`: wait (up to 10 s) for the previous version to exit.
pub fn wait_for_previous(pid: u32) {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject};
    unsafe {
        if let Ok(h) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
            WaitForSingleObject(h, 10_000);
            let _ = CloseHandle(h);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(parse_version("v0.2.0"), Some((0, 2, 0)));
        assert_eq!(parse_version("1.10"), Some((1, 10, 0)));
        assert_eq!(parse_version("2.0.1-beta.1"), Some((2, 0, 1)));
        assert_eq!(parse_version("nightly"), None);
        assert!(parse_version("v0.10.0") > parse_version("v0.9.9"));
    }

    #[test]
    fn hashing() {
        assert_eq!(sha256_hex(b"abc").unwrap(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }
}
