//! Installed-application index.
//!
//! Enumeration and icon extraction run in a short-lived child process (`smowauncher --index`):
//! the shell namespace pulls a lot of COM/shell extension DLLs into whichever process uses it,
//! and doing that in a throwaway process keeps the resident launcher small.

pub mod icons;
pub mod indexer;

use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader};
use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppEntry {
    /// Stable identity (AppsFolder parsing name or file path).
    pub id: String,
    pub name: String,
    /// What to pass to ShellExecute.
    pub launch: String,
    /// Target file on disk, when known (for "open folder" and exe-name matching).
    pub path: Option<String>,
    /// Lowercase extra search terms (exe file stem).
    pub keywords: String,
    /// Store/UWP app (no file path).
    pub packaged: bool,
}

pub enum IndexEvent {
    Apps(Vec<AppEntry>),
    IconsReady,
    Failed(String),
}

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;

/// Spawns the indexer process; `on_event` is called from a background thread.
pub fn index_async(icon_size: u32, on_event: impl Fn(IndexEvent) + Send + 'static) {
    std::thread::Builder::new()
        .name("indexer-pipe".into())
        .stack_size(256 * 1024)
        .spawn(move || {
            let exe = match std::env::current_exe() {
                Ok(e) => e,
                Err(e) => return on_event(IndexEvent::Failed(e.to_string())),
            };
            let child = Command::new(exe)
                .arg("--index")
                .arg(icon_size.to_string())
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .creation_flags(CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS)
                .spawn();
            let mut child = match child {
                Ok(c) => c,
                Err(e) => return on_event(IndexEvent::Failed(e.to_string())),
            };
            let mut reader = BufReader::new(child.stdout.take().expect("piped stdout"));
            let mut line = String::new();
            let mut got_apps = false;
            while reader.read_line(&mut line).unwrap_or(0) > 0 {
                let l = line.trim_end();
                if let Some(json) = l.strip_prefix("APPS ") {
                    match serde_json::from_str::<Vec<AppEntry>>(json) {
                        Ok(apps) => {
                            got_apps = true;
                            on_event(IndexEvent::Apps(apps));
                        }
                        Err(e) => on_event(IndexEvent::Failed(e.to_string())),
                    }
                } else if l == "ICONS" {
                    on_event(IndexEvent::IconsReady);
                }
                line.clear();
            }
            let _ = child.wait();
            if !got_apps {
                on_event(IndexEvent::Failed("indexer produced no output".into()));
            }
        })
        .expect("spawn indexer thread");
}

/// Calls `on_change` (debounced) when shortcuts in either Start Menu folder change,
/// so newly installed programs show up without waiting for the periodic re-index.
pub fn watch_start_menu(on_change: impl Fn() + Send + 'static) {
    use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
    use windows::Win32::Storage::FileSystem::{
        FILE_NOTIFY_CHANGE_DIR_NAME, FILE_NOTIFY_CHANGE_FILE_NAME, FindFirstChangeNotificationW,
        FindNextChangeNotification,
    };
    use windows::Win32::System::Threading::{INFINITE, WaitForMultipleObjects};
    use windows::core::HSTRING;

    let dirs: Vec<std::path::PathBuf> = ["APPDATA", "PROGRAMDATA"]
        .iter()
        .filter_map(std::env::var_os)
        .map(|base| std::path::PathBuf::from(base).join(r"Microsoft\Windows\Start Menu\Programs"))
        .filter(|p| p.exists())
        .collect();

    std::thread::Builder::new()
        .name("startmenu-watch".into())
        .stack_size(64 * 1024)
        .spawn(move || {
            let handles: Vec<HANDLE> = dirs
                .iter()
                .filter_map(|d| unsafe {
                    FindFirstChangeNotificationW(
                        &HSTRING::from(d.as_os_str()),
                        true,
                        FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_DIR_NAME,
                    )
                    .ok()
                })
                .collect();
            if handles.is_empty() {
                return;
            }
            let mut pending = false;
            loop {
                // After a change, wait for 3 s of quiet (installers write many files) before re-indexing.
                let timeout = if pending { 3000 } else { INFINITE };
                let r = unsafe { WaitForMultipleObjects(&handles, false, timeout) };
                if r == WAIT_TIMEOUT {
                    pending = false;
                    log::info!("start menu changed; re-indexing");
                    on_change();
                    continue;
                }
                let i = r.0.wrapping_sub(WAIT_OBJECT_0.0) as usize;
                if i >= handles.len() {
                    return;
                }
                pending = true;
                if unsafe { FindNextChangeNotification(handles[i]) }.is_err() {
                    return;
                }
            }
        })
        .expect("spawn start menu watcher");
}
