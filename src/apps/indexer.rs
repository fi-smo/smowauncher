//! Runs inside the `--index` child process: enumerates apps, prints them, then fills the icon cache.

use super::AppEntry;
use std::collections::HashSet;
use std::io::Write;
use std::path::Path;
use windows::Win32::Storage::EnhancedStorage::PKEY_Link_TargetParsingPath;
use windows::Win32::System::Com::{
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoTaskMemFree,
};
use windows::Win32::UI::Shell::{
    BHID_EnumItems, FOLDERID_AppsFolder, IEnumShellItems, IShellItem, IShellItem2,
    KF_FLAG_DEFAULT, SHGetKnownFolderItem, SHGetKnownFolderPath, SIGDN, SIGDN_NORMALDISPLAY,
    SIGDN_PARENTRELATIVEPARSING,
};
use windows::core::{GUID, Interface, PWSTR};

/// Entry point of `smowauncher --index <icon_size>`.
pub fn run(args: &[String]) -> i32 {
    let icon_size: u32 = args.first().and_then(|s| s.parse().ok()).unwrap_or(32).clamp(16, 256);
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
    }
    let cfg = crate::config::load();
    let t = std::time::Instant::now();
    let apps = enumerate(&cfg);
    log::info!("indexer: {} apps in {:?}", apps.len(), t.elapsed());

    let mut out = std::io::stdout().lock();
    let json = serde_json::to_string(&apps).unwrap_or_else(|_| "[]".into());
    let _ = writeln!(out, "APPS {json}");
    let _ = out.flush();

    let t = std::time::Instant::now();
    let made = super::icons::extract_missing(&apps, icon_size);
    log::info!("indexer: {made} new icons in {:?}", t.elapsed());
    let _ = writeln!(out, "ICONS");
    let _ = out.flush();
    0
}

pub fn enumerate(cfg: &crate::config::Config) -> Vec<AppEntry> {
    let mut apps = Vec::with_capacity(512);
    if let Err(e) = enumerate_apps_folder(&mut apps) {
        log::error!("indexer: AppsFolder enumeration failed: {e}");
    }
    for folder in &cfg.apps.extra_folders {
        scan_folder(Path::new(folder), 0, &mut apps);
    }

    // Internet shortcuts (Steam/Epic games, web links) carry their icon in the .url file;
    // the shell would only give us a generic document icon for the URL itself.
    let url_icons = start_menu_url_icons();
    for app in &mut apps {
        if let Some(url) = app.path.as_deref().filter(|p| is_url(p)) {
            app.icon = url_icons.get(&url.to_lowercase()).cloned();
            app.path = None; // nothing on disk to "open folder" for
            app.keywords.clear(); // "2218750" from steam://rungameid/2218750 isn't a useful term
        }
    }

    let exclude: Vec<String> = cfg.apps.exclude.iter().map(|s| s.to_lowercase()).collect();
    let mut seen = HashSet::new();
    apps.retain(|a| {
        let lname = a.name.to_lowercase();
        if exclude.iter().any(|x| !x.is_empty() && lname.contains(x.as_str())) {
            return false;
        }
        // The same program often appears several times (per-user + all-users shortcuts).
        let key = (lname, a.path.as_deref().unwrap_or(&a.id).to_lowercase());
        seen.insert(key)
    });
    apps
}

fn is_url(s: &str) -> bool {
    s.split_once("://").is_some_and(|(scheme, _)| scheme.len() > 1 && scheme.chars().all(|c| c.is_ascii_alphanumeric()))
}

/// Maps lowercase URL → icon file for every `.url` shortcut in both Start Menu folders.
fn start_menu_url_icons() -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    let roots = ["APPDATA", "PROGRAMDATA"]
        .iter()
        .filter_map(std::env::var_os)
        .map(|base| std::path::PathBuf::from(base).join(r"Microsoft\Windows\Start Menu\Programs"));
    let mut stack: Vec<std::path::PathBuf> = roots.collect();
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("url")) {
                if let Some((url, icon)) = std::fs::read(&path).ok().and_then(|b| parse_url_file(&String::from_utf8_lossy(&b))) {
                    map.insert(url.to_lowercase(), icon);
                }
            }
        }
    }
    map
}

/// Extracts (URL, IconFile) from an internet shortcut's `[InternetShortcut]` section.
fn parse_url_file(text: &str) -> Option<(String, String)> {
    let mut in_section = false;
    let (mut url, mut icon) = (None, None);
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_section = line.eq_ignore_ascii_case("[InternetShortcut]");
        } else if in_section && let Some((k, v)) = line.split_once('=') {
            match k.trim().to_ascii_lowercase().as_str() {
                "url" => url = Some(v.trim().to_owned()),
                "iconfile" => icon = Some(v.trim().to_owned()),
                _ => {}
            }
        }
    }
    let icon = icon.filter(|i| !i.is_empty() && Path::new(i).exists())?;
    Some((url?, icon))
}

fn display_name(item: &IShellItem, kind: SIGDN) -> Option<String> {
    unsafe {
        let p: PWSTR = item.GetDisplayName(kind).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        s
    }
}

fn enumerate_apps_folder(out: &mut Vec<AppEntry>) -> windows::core::Result<()> {
    unsafe {
        let folder: IShellItem = SHGetKnownFolderItem(&FOLDERID_AppsFolder, KF_FLAG_DEFAULT, None)?;
        let items: IEnumShellItems = folder.BindToHandler(None, &BHID_EnumItems)?;
        loop {
            let mut batch: [Option<IShellItem>; 32] = Default::default();
            let mut fetched = 0u32;
            let hr = items.Next(&mut batch, Some(&mut fetched));
            for item in batch.iter().take(fetched as usize).flatten() {
                if let Some(entry) = apps_folder_entry(item) {
                    out.push(entry);
                }
            }
            if hr.is_err() || fetched == 0 {
                break;
            }
        }
    }
    Ok(())
}

fn apps_folder_entry(item: &IShellItem) -> Option<AppEntry> {
    let name = display_name(item, SIGDN_NORMALDISPLAY)?;
    let parsing = display_name(item, SIGDN_PARENTRELATIVEPARSING)?;
    if name.is_empty() || parsing.is_empty() {
        return None;
    }
    let link_target = item
        .cast::<IShellItem2>()
        .ok()
        .and_then(|i2| unsafe { i2.GetString(&PKEY_Link_TargetParsingPath) }.ok())
        .and_then(|p| {
            let s = unsafe { p.to_string() }.ok();
            unsafe { CoTaskMemFree(Some(p.0 as *const _)) };
            s
        })
        .filter(|s| !s.is_empty());
    let path = link_target.or_else(|| resolve_known_folder_path(&parsing));
    // Packaged apps are identified by an AUMID ("Publisher.App_hash!App").
    let packaged = path.is_none() && parsing.contains('!');
    let keywords = path
        .as_deref()
        .and_then(|p| Path::new(p).file_stem())
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    Some(AppEntry {
        launch: format!("shell:AppsFolder\\{parsing}"),
        id: parsing,
        name,
        path,
        keywords,
        packaged,
        icon: None,
    })
}

/// Desktop apps without a shortcut appear as "{KNOWNFOLDERID}\sub\app.exe" or a plain path.
fn resolve_known_folder_path(parsing: &str) -> Option<String> {
    if parsing.len() > 3 && parsing.as_bytes()[1] == b':' {
        return Some(parsing.to_owned());
    }
    let rest = parsing.strip_prefix('{')?;
    let (guid, tail) = rest.split_once("}\\")?;
    let guid = GUID::try_from(guid).ok()?;
    let base = unsafe { SHGetKnownFolderPath(&guid, KF_FLAG_DEFAULT, None) }.ok()?;
    let base_s = unsafe { base.to_string() }.ok();
    unsafe { CoTaskMemFree(Some(base.0 as *const _)) };
    Some(format!("{}\\{tail}", base_s?))
}

fn scan_folder(dir: &Path, depth: u32, out: &mut Vec<AppEntry>) {
    let Ok(read) = std::fs::read_dir(dir) else { return };
    for entry in read.flatten() {
        let path = entry.path();
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_dir() {
            if depth < 3 {
                scan_folder(&path, depth + 1, out);
            }
            continue;
        }
        let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase());
        if !matches!(ext.as_deref(), Some("exe" | "lnk" | "appref-ms" | "url")) {
            continue;
        }
        let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let p = path.to_string_lossy().into_owned();
        out.push(AppEntry {
            id: p.clone(),
            keywords: name.to_lowercase(),
            name,
            launch: p.clone(),
            path: Some(p),
            packaged: false,
            icon: None,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        assert!(is_url("steam://rungameid/2218750"));
        assert!(is_url("https://example.com"));
        assert!(!is_url(r"C:\Program Files\app.exe"));
        assert!(!is_url("{7C5A40EF-A0FB-4BFC-874A-C0F2E0B9FA8E}\\Steam\\Steam.exe"));
    }

    #[test]
    fn url_file() {
        let exe = std::env::current_exe().unwrap();
        let text = format!(
            "[{{000214A0-0000-0000-C000-000000000046}}]\r\nProp3=19,0\r\n[InternetShortcut]\r\nIDList=\r\nIconIndex=0\r\nURL=steam://rungameid/2218750\r\nIconFile={}\r\n",
            exe.display()
        );
        let (url, icon) = parse_url_file(&text).unwrap();
        assert_eq!(url, "steam://rungameid/2218750");
        assert_eq!(icon, exe.display().to_string());
        // Missing icon file on disk → no icon.
        assert!(parse_url_file("[InternetShortcut]\nURL=x://y\nIconFile=C:\\nope\\missing.ico").is_none());
    }
}
