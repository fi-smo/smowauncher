//! Clipboard access: writing text and files (CF_HDROP, as Explorer's "Copy"), and reading
//! text for the clipboard history while respecting the "don't record this" markers that
//! password managers set.

use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, GetClipboardOwner, IsClipboardFormatAvailable, OpenClipboard,
    RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::System::Ole::{CF_DIB, CF_DIBV5, CF_HDROP, CF_UNICODETEXT};
use windows::Win32::UI::Shell::DROPFILES;
use windows::core::w;

fn global_from(bytes: &[u8]) -> Option<HGLOBAL> {
    unsafe {
        let h = GlobalAlloc(GMEM_MOVEABLE, bytes.len()).ok()?;
        let p = GlobalLock(h) as *mut u8;
        if p.is_null() {
            let _ = GlobalFree(Some(h));
            return None;
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
        let _ = GlobalUnlock(h);
        Some(h)
    }
}

/// Puts several formats on the clipboard at once. Returns false if the clipboard was busy.
fn set(formats: &[(u32, Vec<u8>)]) -> bool {
    unsafe {
        // Another app may hold the clipboard for a moment.
        if !open_with_retry() {
            return false;
        }
        let _ = EmptyClipboard();
        for (format, bytes) in formats {
            if let Some(h) = global_from(bytes) {
                // On success the clipboard owns the memory.
                if SetClipboardData(*format, Some(HANDLE(h.0))).is_err() {
                    let _ = GlobalFree(Some(h));
                }
            }
        }
        let _ = CloseClipboard();
        true
    }
}

fn open_with_retry() -> bool {
    for _ in 0..10 {
        if unsafe { OpenClipboard(None) }.is_ok() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    false
}

/// Process name of the clipboard's current owner ("KeePassXC", "Code", …).
fn owner_process() -> String {
    unsafe {
        let Ok(owner) = GetClipboardOwner() else { return String::new() };
        if owner.is_invalid() {
            return String::new();
        }
        let mut pid = 0u32;
        windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId(owner, Some(&mut pid));
        let exe = super::windows_list::exe_path(pid);
        std::path::Path::new(&exe).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
    }
}

/// What the clipboard history records.
pub enum Content {
    Text(String),
    /// PNG bytes, width, height.
    Image(Vec<u8>, u32, u32),
}

/// Copies a clipboard format's bytes (the clipboard must be open).
unsafe fn format_bytes(format: u32, max: usize) -> Option<Vec<u8>> {
    unsafe {
        let h = GetClipboardData(format).ok()?;
        let size = GlobalSize(HGLOBAL(h.0));
        if size == 0 || size > max {
            return None;
        }
        let p = GlobalLock(HGLOBAL(h.0)) as *const u8;
        if p.is_null() {
            return None;
        }
        let bytes = std::slice::from_raw_parts(p, size).to_vec();
        let _ = GlobalUnlock(HGLOBAL(h.0));
        Some(bytes)
    }
}

fn png_format() -> u32 {
    unsafe { RegisterClipboardFormatW(w!("PNG")) }
}

/// Reads the clipboard for the history: (content, source process). Text wins when both are
/// there (e.g. cells copied from a spreadsheet); images only when `images` is set. Returns
/// None for other content and for content its source asked not to be recorded.
pub fn read_for_history(max_text_bytes: usize, images: bool, max_image_bytes: usize) -> Option<(Content, String)> {
    unsafe {
        // Password managers and other apps put these markers next to sensitive data.
        for marker in [w!("ExcludeClipboardContentFromMonitorProcessing"), w!("Clipboard Viewer Ignore")] {
            if IsClipboardFormatAvailable(RegisterClipboardFormatW(marker)).is_ok() {
                return None;
            }
        }
        let text = IsClipboardFormatAvailable(CF_UNICODETEXT.0 as u32).is_ok();
        let png = images && IsClipboardFormatAvailable(png_format()).is_ok();
        let dib = images && (IsClipboardFormatAvailable(CF_DIBV5.0 as u32).is_ok() || IsClipboardFormatAvailable(CF_DIB.0 as u32).is_ok());
        if !text && !png && !dib {
            return None;
        }
        let source = owner_process();
        if !open_with_retry() {
            return None;
        }
        let result = (|| {
            let history_flag = RegisterClipboardFormatW(w!("CanIncludeInClipboardHistory"));
            if let Ok(h) = GetClipboardData(history_flag) {
                let p = GlobalLock(HGLOBAL(h.0)) as *const u32;
                let allowed = p.is_null() || *p != 0;
                let _ = GlobalUnlock(HGLOBAL(h.0));
                if !allowed {
                    return None;
                }
            }
            if text {
                let bytes = format_bytes(CF_UNICODETEXT.0 as u32, max_text_bytes * 2)?;
                let units: Vec<u16> = bytes.chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]])).collect();
                let len = units.iter().position(|&c| c == 0).unwrap_or(units.len());
                return Some(Content::Text(String::from_utf16_lossy(&units[..len])));
            }
            // Apps that put a PNG there (Snipping Tool, browsers) keep transparency intact.
            if png && let Some(bytes) = format_bytes(png_format(), max_image_bytes) {
                if let Some((w, h)) = crate::imaging::png_size(&bytes) {
                    return Some(Content::Image(bytes, w, h));
                }
            }
            // A bitmap: copied now, converted to PNG after the clipboard is closed.
            let dib = format_bytes(CF_DIBV5.0 as u32, max_image_bytes * 4).or_else(|| format_bytes(CF_DIB.0 as u32, max_image_bytes * 4))?;
            Some(Content::Image(dib, 0, 0))
        })();
        let _ = CloseClipboard();
        let content = match result? {
            Content::Image(dib, 0, 0) => {
                let img = crate::imaging::from_dib(&dib)?;
                let png = crate::imaging::encode_png(&img).filter(|p| p.len() <= max_image_bytes)?;
                Content::Image(png, img.width, img.height)
            }
            other => other,
        };
        Some((content, source))
    }
}

/// Puts an image on the clipboard as a bitmap (every app) and as PNG (apps that prefer it).
pub fn set_image(png: &[u8]) -> bool {
    let Some(img) = crate::imaging::decode_png(png) else { return false };
    set(&[(CF_DIB.0 as u32, crate::imaging::to_dib(&img)), (png_format(), png.to_vec())])
}

/// The clipboard's current text, if any (for the {clipboard} snippet placeholder).
pub fn get_text() -> Option<String> {
    unsafe {
        IsClipboardFormatAvailable(CF_UNICODETEXT.0 as u32).ok()?;
        if !open_with_retry() {
            return None;
        }
        let text = (|| {
            let h = GetClipboardData(CF_UNICODETEXT.0 as u32).ok()?;
            let size = GlobalSize(HGLOBAL(h.0));
            let p = GlobalLock(HGLOBAL(h.0)) as *const u16;
            if p.is_null() {
                return None;
            }
            let units = std::slice::from_raw_parts(p, size / 2);
            let len = units.iter().position(|&c| c == 0).unwrap_or(units.len());
            let text = String::from_utf16_lossy(&units[..len]);
            let _ = GlobalUnlock(HGLOBAL(h.0));
            Some(text)
        })();
        let _ = CloseClipboard();
        text
    }
}

fn utf16z(s: &str) -> Vec<u8> {
    s.encode_utf16().chain(std::iter::once(0)).flat_map(|u| u.to_le_bytes()).collect()
}

pub fn set_text(text: &str) -> bool {
    set(&[(CF_UNICODETEXT.0 as u32, utf16z(text))])
}

/// Copies files so they can be pasted in Explorer (and also as their path in text fields).
pub fn set_files(paths: &[&str]) -> bool {
    let header = DROPFILES {
        pFiles: size_of::<DROPFILES>() as u32,
        fWide: true.into(),
        ..Default::default()
    };
    let mut drop = unsafe {
        std::slice::from_raw_parts(&header as *const DROPFILES as *const u8, size_of::<DROPFILES>()).to_vec()
    };
    for p in paths {
        drop.extend(utf16z(p));
    }
    drop.extend([0, 0]); // list terminator
    // DROPEFFECT_COPY, so Explorer copies instead of moving on paste.
    let effect = unsafe { RegisterClipboardFormatW(w!("Preferred DropEffect")) };
    set(&[
        (CF_HDROP.0 as u32, drop),
        (effect, 1u32.to_le_bytes().to_vec()),
        (CF_UNICODETEXT.0 as u32, utf16z(&paths.join("\r\n"))),
    ])
}
