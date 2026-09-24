//! Minimal clipboard writes: text, and files (CF_HDROP, as Explorer's "Copy").

use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::Win32::System::Ole::{CF_HDROP, CF_UNICODETEXT};
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
        let mut opened = false;
        for _ in 0..10 {
            if OpenClipboard(None).is_ok() {
                opened = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        if !opened {
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
