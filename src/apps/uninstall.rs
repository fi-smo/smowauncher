//! "Uninstall…" for app results: Store/MSIX apps are removed through the PackageManager,
//! desktop apps get their own uninstaller (found in the registry's Uninstall keys).

use super::AppEntry;
use crate::platform::{shell, wide};
use std::path::Path;
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY, REG_SAM_FLAGS, REG_VALUE_TYPE,
    RegCloseKey, RegEnumKeyExW, RegOpenKeyExW, RegQueryValueExW,
};
use windows::core::{HSTRING, PCWSTR, PWSTR};

const UNINSTALL_KEY: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall";

#[derive(Debug, PartialEq)]
pub enum Uninstaller {
    /// MSIX/Store package full name.
    Package(String),
    /// Executable and arguments of a desktop app's uninstaller.
    Command(String, String),
    /// Nothing matched: open Settings › Installed apps.
    Settings,
}

/// What to run to uninstall `app` (reads the registry / package catalog; call off the UI thread).
pub fn find(app: &AppEntry) -> Result<Uninstaller, String> {
    if app.packaged {
        return find_package(app);
    }
    if let Some(u) = find_registered(app) {
        return Ok(u);
    }
    let windir = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into()).to_lowercase();
    if app.path.as_deref().is_some_and(|p| clean_path(p).starts_with(&windir)) {
        return Err(format!("{} is part of Windows and can't be uninstalled.", app.name));
    }
    Ok(Uninstaller::Settings)
}

// ------------------------------------------------------------------ packaged apps

fn find_package(app: &AppEntry) -> Result<Uninstaller, String> {
    use windows::ApplicationModel::PackageSignatureKind;
    use windows::Management::Deployment::PackageManager;
    // AUMID: "<family name>!<app id>".
    let family = app.id.split('!').next().unwrap_or_default();
    let pm = PackageManager::new().map_err(|e| e.message())?;
    let packages = pm
        .FindPackagesByUserSecurityIdPackageFamilyName(&HSTRING::new(), &HSTRING::from(family))
        .map_err(|e| e.message())?;
    for p in packages {
        if p.IsFramework().unwrap_or(false) || p.IsResourcePackage().unwrap_or(false) {
            continue;
        }
        if p.SignatureKind().is_ok_and(|k| k == PackageSignatureKind::System) {
            return Err(format!("{} is part of Windows and can't be uninstalled.", app.name));
        }
        let full = p.Id().and_then(|id| id.FullName()).map_err(|e| e.message())?;
        return Ok(Uninstaller::Package(full.to_string()));
    }
    Err(format!("Couldn't find the package for {}.", app.name))
}

/// Removes a package for the current user (blocking).
pub fn remove_package(full_name: &str) -> Result<(), String> {
    use windows::Management::Deployment::PackageManager;
    let pm = PackageManager::new().map_err(|e| e.message())?;
    let result = pm.RemovePackageAsync(&HSTRING::from(full_name)).and_then(|op| op.join());
    match result {
        Ok(r) if r.ExtendedErrorCode().is_ok_and(|c| c.is_err()) => {
            Err(r.ErrorText().map(|t| t.to_string()).unwrap_or_else(|_| "removal failed".into()))
        }
        Ok(_) => Ok(()),
        Err(e) => Err(e.message()),
    }
}

// ------------------------------------------------------------------ desktop apps

#[derive(Debug, Default)]
struct Entry {
    name: String,
    uninstall: String,
    install_location: String,
    display_icon: String,
}

fn find_registered(app: &AppEntry) -> Option<Uninstaller> {
    let entries = registered_entries();
    let (score, best) = entries.iter().map(|e| (score(app, e), e)).max_by_key(|(s, _)| *s)?;
    if score < 2 {
        log::info!("uninstall: no registry entry for {} (best score {score})", app.name);
        return None;
    }
    log::info!("uninstall: {} -> {} ({})", app.name, best.name, best.uninstall);
    let (exe, args) = split_command(&best.uninstall)?;
    Some(Uninstaller::Command(exe, args))
}

fn registered_entries() -> Vec<Entry> {
    let mut out = Vec::new();
    for (root, view) in [
        (HKEY_CURRENT_USER, KEY_WOW64_64KEY),
        (HKEY_LOCAL_MACHINE, KEY_WOW64_64KEY),
        (HKEY_LOCAL_MACHINE, KEY_WOW64_32KEY),
    ] {
        let Some(key) = open(root, UNINSTALL_KEY, view) else { continue };
        for sub in subkeys(key) {
            let Some(k) = open(key, &sub, view) else { continue };
            let get = |name: &str| value(k, name).unwrap_or_default();
            let hidden = value(k, "SystemComponent").as_deref() == Some("1");
            let e = Entry {
                name: get("DisplayName"),
                uninstall: get("UninstallString"),
                install_location: get("InstallLocation"),
                display_icon: get("DisplayIcon"),
            };
            unsafe {
                let _ = RegCloseKey(k);
            }
            if !hidden && !e.name.is_empty() && !e.uninstall.is_empty() {
                out.push(e);
            }
        }
        unsafe {
            let _ = RegCloseKey(key);
        }
    }
    out
}

fn open(root: HKEY, path: &str, view: REG_SAM_FLAGS) -> Option<HKEY> {
    let path = wide(path);
    let mut key = HKEY::default();
    unsafe { RegOpenKeyExW(root, PCWSTR(path.as_ptr()), Some(0), KEY_READ | view, &mut key) }.ok().ok()?;
    Some(key)
}

fn subkeys(key: HKEY) -> Vec<String> {
    let mut out = Vec::new();
    for i in 0.. {
        let mut buf = [0u16; 256];
        let mut len = buf.len() as u32;
        let r = unsafe { RegEnumKeyExW(key, i, Some(PWSTR(buf.as_mut_ptr())), &mut len, None, None, None, None) };
        if r.is_err() {
            break;
        }
        out.push(String::from_utf16_lossy(&buf[..len as usize]));
    }
    out
}

/// A string (REG_SZ / REG_EXPAND_SZ, expanded) or DWORD value as text.
fn value(key: HKEY, name: &str) -> Option<String> {
    let name = wide(name);
    let mut kind = REG_VALUE_TYPE::default();
    let mut buf = vec![0u8; 4096];
    let mut len = buf.len() as u32;
    unsafe { RegQueryValueExW(key, PCWSTR(name.as_ptr()), None, Some(&mut kind), Some(buf.as_mut_ptr()), Some(&mut len)) }
        .ok()
        .ok()?;
    let bytes = &buf[..len as usize];
    match kind.0 {
        // REG_SZ, REG_EXPAND_SZ
        1 | 2 => {
            let units: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            let s = String::from_utf16_lossy(&units).trim_end_matches('\0').trim().to_owned();
            Some(if kind.0 == 2 { shell::expand_env(&s) } else { s })
        }
        // REG_DWORD
        4 if bytes.len() >= 4 => Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]).to_string()),
        _ => None,
    }
}

fn normalize(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// `"C:\x\app.exe",0` → `c:\x\app.exe`
fn clean_path(s: &str) -> String {
    let s = s.trim();
    let s = match s.strip_prefix('"') {
        Some(rest) => rest.split('"').next().unwrap_or_default(),
        None => s.rsplit_once(',').filter(|(_, n)| n.trim().parse::<i32>().is_ok()).map_or(s, |(p, _)| p),
    };
    s.trim().trim_end_matches('\\').to_lowercase()
}

/// A folder specific enough to identify one app (not `C:\Program Files`, not under Windows).
fn specific_dir(dir: &str) -> bool {
    let windir = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into()).to_lowercase();
    !dir.is_empty() && dir.split('\\').filter(|c| !c.is_empty()).count() >= 3 && !dir.starts_with(&windir)
}

fn is_under(path: &str, dir: &str) -> bool {
    specific_dir(dir) && path.strip_prefix(dir).is_some_and(|rest| rest.starts_with('\\'))
}

fn score(app: &AppEntry, e: &Entry) -> u32 {
    let mut s = 0;
    let (name, reg_name) = (normalize(&app.name), normalize(&e.name));
    if !name.is_empty() && name == reg_name {
        s += 2;
    } else if name.len() >= 4 && reg_name.len() >= 4 && (reg_name.contains(&name) || name.contains(&reg_name)) {
        s += 1;
    }
    if let Some(path) = app.path.as_deref().map(clean_path) {
        let icon = clean_path(&e.display_icon);
        let location = clean_path(&e.install_location);
        let uninstaller_dir = split_command(&e.uninstall)
            .and_then(|(exe, _)| Path::new(&exe).parent().map(|p| p.to_string_lossy().to_lowercase()))
            .unwrap_or_default();
        if !icon.is_empty() && icon == path {
            s += 4;
        } else if is_under(&path, &location) || is_under(&path, &uninstaller_dir) {
            s += 3;
        }
    }
    s
}

/// Splits a registered command line into executable and arguments. MSI "repair/modify"
/// commands (`MsiExec.exe /I{guid}`) become uninstalls (`/X{guid}`).
fn split_command(command: &str) -> Option<(String, String)> {
    let c = command.trim();
    let (exe, args) = if let Some(rest) = c.strip_prefix('"') {
        let end = rest.find('"')?;
        (&rest[..end], rest[end + 1..].trim())
    } else {
        let lower = c.to_lowercase();
        let end = lower.find(".exe").map(|i| i + 4).unwrap_or(c.find(' ').unwrap_or(c.len()));
        (&c[..end], c[end..].trim())
    };
    if exe.is_empty() {
        return None;
    }
    let mut args = args.to_owned();
    if exe.to_lowercase().ends_with("msiexec.exe") && args.len() >= 2 && args[..2].eq_ignore_ascii_case("/i") {
        args.replace_range(..2, "/X");
    }
    Some((exe.to_owned(), args))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(name: &str, path: Option<&str>) -> AppEntry {
        AppEntry {
            id: name.into(),
            name: name.into(),
            launch: String::new(),
            path: path.map(Into::into),
            keywords: String::new(),
            packaged: false,
            icon: None,
        }
    }

    #[test]
    fn splits_commands() {
        assert_eq!(
            split_command(r#""C:\Program Files\X\unins000.exe" /SILENT"#),
            Some((r"C:\Program Files\X\unins000.exe".into(), "/SILENT".into()))
        );
        assert_eq!(
            split_command("MsiExec.exe /I{1234-5678}"),
            Some(("MsiExec.exe".into(), "/X{1234-5678}".into()))
        );
        assert_eq!(
            split_command(r"C:\Users\u\AppData\Local\Discord\Update.exe --uninstall"),
            Some((r"C:\Users\u\AppData\Local\Discord\Update.exe".into(), "--uninstall".into()))
        );
    }

    #[test]
    fn scores_entries() {
        let discord = Entry {
            name: "Discord".into(),
            uninstall: r"C:\Users\u\AppData\Local\Discord\Update.exe --uninstall".into(),
            ..Default::default()
        };
        let a = app("Discord", Some(r"C:\Users\u\AppData\Local\Discord\Update.exe"));
        assert!(score(&a, &discord) >= 5);

        let vlc = Entry {
            name: "VLC media player".into(),
            uninstall: r#""C:\Program Files\VideoLAN\VLC\uninstall.exe""#.into(),
            display_icon: r#""C:\Program Files\VideoLAN\VLC\vlc.exe",0"#.into(),
            ..Default::default()
        };
        assert!(score(&app("VLC media player", Some(r"C:\Program Files\VideoLAN\VLC\vlc.exe")), &vlc) >= 6);

        // A generic install root or an MSI uninstaller in System32 must not match everything.
        let msi = Entry {
            name: "Some Tool".into(),
            uninstall: "MsiExec.exe /X{1}".into(),
            install_location: r"C:\Program Files".into(),
            ..Default::default()
        };
        assert!(score(&app("Paint.NET", Some(r"C:\Program Files\paint.net\paintdotnet.exe")), &msi) < 2);
    }
}
