//! Icons for file results. Most files share an icon per extension, so we ask the shell by
//! extension only (`SHGFI_USEFILEATTRIBUTES`: no disk access, no per-file icon handlers).
//! Executables, shortcuts and a few other types carry their own icon and are looked up per file.
//! Extraction runs on a background thread; results come back as premultiplied RGBA.

use crate::apps::icons::{hbitmap_to_rgba, premultiply, resample};
use crate::platform::wide;
use std::sync::mpsc::{Sender, channel};
use std::sync::{Mutex, OnceLock};
use windows::Win32::Graphics::Gdi::{DeleteObject, HGDIOBJ};
use windows::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL, FILE_FLAGS_AND_ATTRIBUTES};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::UI::Shell::{SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON, SHGFI_USEFILEATTRIBUTES, SHGetFileInfoW};
use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, ICONINFO};
use windows::core::PCWSTR;

/// Extensions whose icon lives in the file itself.
const PER_FILE: [&str; 7] = ["exe", "lnk", "ico", "url", "appref-ms", "msc", "cpl"];

pub struct Icon {
    pub key: String,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Cache key for a path: shared per extension unless the type has per-file icons.
pub fn key_for(path: &str, folder: bool) -> String {
    if folder {
        return if path.len() <= 3 { format!("drive:{path}") } else { "folder".into() };
    }
    let name = path.rsplit('\\').next().unwrap_or(path);
    match name.rfind('.') {
        Some(i) if i > 0 => {
            let ext = name[i + 1..].to_lowercase();
            if PER_FILE.contains(&ext.as_str()) { format!("file:{}", path.to_lowercase()) } else { format!(".{ext}") }
        }
        _ => "file".into(),
    }
}

type Job = (String, String, bool);
static JOBS: OnceLock<Mutex<Sender<Job>>> = OnceLock::new();

/// Starts the icon thread. `on_icon` runs on that thread.
pub fn spawn(size: u32, on_icon: impl Fn(Icon) + Send + 'static) {
    let (tx, rx) = channel::<Job>();
    let _ = JOBS.set(Mutex::new(tx));
    std::thread::Builder::new()
        .name("file-icons".into())
        .stack_size(512 * 1024)
        .spawn(move || {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            }
            while let Ok((key, path, folder)) = rx.recv() {
                if let Some((w, h, rgba)) = extract(&key, &path, folder, size) {
                    on_icon(Icon { key, width: w, height: h, rgba });
                }
            }
        })
        .expect("spawn file icon thread");
}

pub fn request(key: String, path: String, folder: bool) {
    if let Some(tx) = JOBS.get() {
        let _ = tx.lock().unwrap().send((key, path, folder));
    }
}

fn extract(key: &str, path: &str, folder: bool, size: u32) -> Option<(u32, u32, Vec<u8>)> {
    let per_file = key.starts_with("file:") || key.starts_with("drive:");
    // With USEFILEATTRIBUTES the shell only looks at the name's extension.
    let (query, attrs) = match (per_file, folder) {
        (true, _) => (path.to_owned(), FILE_ATTRIBUTE_NORMAL),
        (false, true) => ("folder".to_owned(), FILE_ATTRIBUTE_DIRECTORY),
        (false, false) => (format!("file{}", key.strip_prefix("file").unwrap_or(key)), FILE_ATTRIBUTE_NORMAL),
    };
    let flags = if per_file { SHGFI_ICON | SHGFI_LARGEICON } else { SHGFI_ICON | SHGFI_LARGEICON | SHGFI_USEFILEATTRIBUTES };
    let q = wide(&query);
    let mut info = SHFILEINFOW::default();
    let ok = unsafe {
        SHGetFileInfoW(
            PCWSTR(q.as_ptr()),
            FILE_FLAGS_AND_ATTRIBUTES(attrs.0),
            Some(&mut info),
            size_of::<SHFILEINFOW>() as u32,
            flags,
        )
    };
    if ok == 0 || info.hIcon.is_invalid() {
        return None;
    }
    let result = unsafe {
        let mut ii = ICONINFO::default();
        let r = if GetIconInfo(info.hIcon, &mut ii).is_ok() {
            let r = hbitmap_to_rgba(ii.hbmColor);
            let _ = DeleteObject(HGDIOBJ(ii.hbmColor.0));
            let _ = DeleteObject(HGDIOBJ(ii.hbmMask.0));
            r
        } else {
            None
        };
        let _ = DestroyIcon(info.hIcon);
        r
    };
    let (w, h, mut px) = result?;
    premultiply(&mut px);
    let scale = size as f32 / w.max(h) as f32;
    let (dw, dh) = (((w as f32 * scale).round() as u32).max(1), ((h as f32 * scale).round() as u32).max(1));
    Some((dw, dh, resample(&px, w as usize, h as usize, dw as usize, dh as usize)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys() {
        assert_eq!(key_for(r"C:\a\Report.PDF", false), ".pdf");
        assert_eq!(key_for(r"C:\a\Tool.exe", false), r"file:c:\a\tool.exe");
        assert_eq!(key_for(r"C:\a\Makefile", false), "file");
        assert_eq!(key_for(r"C:\a\sub", true), "folder");
        assert_eq!(key_for(r"D:\", true), r"drive:D:\");
    }
}
