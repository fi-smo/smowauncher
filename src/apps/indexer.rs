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
        });
    }
}
