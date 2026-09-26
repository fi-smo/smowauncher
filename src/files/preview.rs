//! Previews for the selected file: a shell thumbnail (images, PDFs, videos, documents — or
//! the large icon), the start of text files, a folder's first entries, and size/date/type.
//! Runs on its own thread; only the most recent request is worked on.

use crate::apps::icons::{hbitmap_to_rgba, premultiply, resample};
use crate::platform::wide;
use std::sync::{Condvar, Mutex, OnceLock};
use windows::Win32::Foundation::{FILETIME, SIZE, SYSTEMTIME};
use windows::Win32::Graphics::Gdi::{DeleteObject, HGDIOBJ};
use windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES;
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
use windows::Win32::UI::Shell::{
    IShellItemImageFactory, SHCreateItemFromParsingName, SHFILEINFOW, SHGFI_TYPENAME, SHGFI_USEFILEATTRIBUTES,
    SHGetFileInfoW, SIIGBF_BIGGERSIZEOK, SIIGBF_ICONONLY, SIIGBF_THUMBNAILONLY,
};
use windows::core::{HSTRING, PCWSTR};

const TEXT_BYTES: usize = 16 * 1024;
const TEXT_LINES: usize = 40;
const FOLDER_ENTRIES: usize = 30;

pub struct Request {
    pub path: String,
    pub folder: bool,
    /// Largest side of the thumbnail, in pixels.
    pub size: u32,
}

pub struct Preview {
    pub path: String,
    /// Premultiplied RGBA.
    pub image: Option<(u32, u32, Vec<u8>)>,
    /// The file is a picture/PDF/video with a real thumbnail (shown big), not just an icon.
    pub thumbnail: bool,
    pub text: Option<String>,
    /// "PNG File · 1.2 MB · 2026-09-25 14:03"
    pub meta: String,
}

static LATEST: OnceLock<(Mutex<Option<Request>>, Condvar)> = OnceLock::new();

/// Starts the preview thread. `on_preview` runs on that thread.
pub fn spawn(on_preview: impl Fn(Preview) + Send + 'static) {
    let _ = LATEST.set((Mutex::new(None), Condvar::new()));
    std::thread::Builder::new()
        .name("file-preview".into())
        .stack_size(1024 * 1024)
        .spawn(move || {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            }
            let (lock, cvar) = LATEST.get().unwrap();
            loop {
                let req = {
                    let mut slot = lock.lock().unwrap();
                    while slot.is_none() {
                        slot = cvar.wait(slot).unwrap();
                    }
                    slot.take().unwrap()
                };
                on_preview(build(&req));
            }
        })
        .expect("spawn preview thread");
}

/// Replaces any request not started yet (scrolling through results only previews the last).
pub fn request(req: Request) {
    if let Some((lock, cvar)) = LATEST.get() {
        *lock.lock().unwrap() = Some(req);
        cvar.notify_one();
    }
}

fn build(req: &Request) -> Preview {
    let meta = std::fs::metadata(&req.path).ok();
    let text = if req.folder { folder_listing(&req.path) } else { text_start(&req.path) };
    // Text files show their content; everything else a thumbnail (or its big icon).
    let (image, thumbnail) = match shell_image(&req.path, req.size, true) {
        Some(img) if text.is_none() => (Some(img), true),
        _ => (shell_image(&req.path, req.size.min(256), false), false),
    };
    let mut parts = vec![type_name(&req.path, req.folder)];
    if let Some(m) = &meta {
        if !req.folder {
            parts.push(human_size(m.len()));
        }
        if let Ok(t) = m.modified() {
            parts.push(local_time(t));
        }
    }
    Preview { path: req.path.clone(), image, thumbnail, text, meta: parts.into_iter().filter(|p| !p.is_empty()).collect::<Vec<_>>().join(" · ") }
}

/// A thumbnail (`thumbnail_only`) or the icon, fitted into `size`×`size`, premultiplied.
fn shell_image(path: &str, size: u32, thumbnail_only: bool) -> Option<(u32, u32, Vec<u8>)> {
    let flags = if thumbnail_only { SIIGBF_THUMBNAILONLY | SIIGBF_BIGGERSIZEOK } else { SIIGBF_ICONONLY | SIIGBF_BIGGERSIZEOK };
    let (w, h, mut px) = unsafe {
        let factory: IShellItemImageFactory = SHCreateItemFromParsingName(&HSTRING::from(path), None).ok()?;
        let hbmp = factory.GetImage(SIZE { cx: size as i32, cy: size as i32 }, flags).ok()?;
        let r = hbitmap_to_rgba(hbmp);
        let _ = DeleteObject(HGDIOBJ(hbmp.0));
        r?
    };
    // Photo thumbnails often come without alpha (all zero): they're opaque.
    if px.chunks_exact(4).all(|p| p[3] == 0) {
        px.chunks_exact_mut(4).for_each(|p| p[3] = 255);
    }
    premultiply(&mut px);
    let scale = (size as f32 / w.max(h) as f32).min(1.0);
    let (dw, dh) = (((w as f32 * scale).round() as u32).max(1), ((h as f32 * scale).round() as u32).max(1));
    if (dw, dh) == (w, h) {
        return Some((w, h, px));
    }
    Some((dw, dh, resample(&px, w as usize, h as usize, dw as usize, dh as usize)))
}

/// The first lines of a text file; None for binary files.
fn text_start(path: &str) -> Option<String> {
    use std::io::Read;
    let mut buf = vec![0u8; TEXT_BYTES];
    let n = std::fs::File::open(path).ok()?.read(&mut buf).ok()?;
    buf.truncate(n);
    let text = decode_text(&buf)?;
    let mut out: Vec<String> = text
        .lines()
        .take(TEXT_LINES)
        .map(|l| {
            let l = l.replace('\t', "    ");
            if l.chars().count() > 160 { l.chars().take(160).collect::<String>() + "…" } else { l }
        })
        .collect();
    while out.last().is_some_and(|l| l.trim().is_empty()) {
        out.pop();
    }
    (!out.is_empty()).then(|| out.join("\n"))
}

/// UTF-8 (with or without BOM) or UTF-16 (with BOM); None when the bytes look binary.
pub fn decode_text(buf: &[u8]) -> Option<String> {
    if let Some(rest) = buf.strip_prefix(&[0xFF, 0xFE]) {
        let units: Vec<u16> = rest.chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]])).collect();
        return Some(String::from_utf16_lossy(&units));
    }
    let buf = buf.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(buf);
    if buf.is_empty() || buf.contains(&0) {
        return None;
    }
    // A cut-off multi-byte character at the end is fine; lots of invalid bytes aren't.
    let text = String::from_utf8_lossy(buf);
    let bad = text.chars().filter(|&c| c == '\u{FFFD}').count();
    (bad * 50 <= text.chars().count()).then(|| text.into_owned())
}

fn folder_listing(path: &str) -> Option<String> {
    let mut entries: Vec<(bool, String)> = std::fs::read_dir(path)
        .ok()?
        .flatten()
        .map(|e| (e.file_type().is_ok_and(|t| t.is_dir()), e.file_name().to_string_lossy().into_owned()))
        .collect();
    if entries.is_empty() {
        return Some("(empty folder)".into());
    }
    let total = entries.len();
    // Folders first, then files, each alphabetically (like Explorer).
    entries.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.to_lowercase().cmp(&b.1.to_lowercase())));
    let mut lines: Vec<String> =
        entries.iter().take(FOLDER_ENTRIES).map(|(dir, name)| if *dir { format!("{name}\\") } else { name.clone() }).collect();
    if total > FOLDER_ENTRIES {
        lines.push(format!("… {} more", total - FOLDER_ENTRIES));
    }
    Some(lines.join("\n"))
}

fn type_name(path: &str, folder: bool) -> String {
    if folder {
        return "Folder".into();
    }
    let name = wide(path);
    let mut info = SHFILEINFOW::default();
    let ok = unsafe {
        SHGetFileInfoW(
            PCWSTR(name.as_ptr()),
            FILE_FLAGS_AND_ATTRIBUTES(0x80),
            Some(&mut info),
            size_of::<SHFILEINFOW>() as u32,
            SHGFI_TYPENAME | SHGFI_USEFILEATTRIBUTES,
        )
    };
    if ok == 0 {
        return String::new();
    }
    let len = info.szTypeName.iter().position(|&c| c == 0).unwrap_or(info.szTypeName.len());
    String::from_utf16_lossy(&info.szTypeName[..len])
}

pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["bytes", "KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} bytes");
    }
    let mut v = bytes as f64;
    let mut unit = 0;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    if v < 10.0 { format!("{v:.1} {}", UNITS[unit]) } else { format!("{v:.0} {}", UNITS[unit]) }
}

/// "2026-09-25 14:03" in local time.
fn local_time(t: std::time::SystemTime) -> String {
    // FILETIME: 100 ns ticks since 1601-01-01 (11644473600 s before the Unix epoch).
    let Ok(d) = t.duration_since(std::time::UNIX_EPOCH) else { return String::new() };
    let ticks = (d.as_secs() + 11_644_473_600) * 10_000_000 + d.subsec_nanos() as u64 / 100;
    let ft = FILETIME { dwLowDateTime: ticks as u32, dwHighDateTime: (ticks >> 32) as u32 };
    let (mut utc, mut local) = (SYSTEMTIME::default(), SYSTEMTIME::default());
    unsafe {
        if FileTimeToSystemTime(&ft, &mut utc).is_err() || SystemTimeToTzSpecificLocalTime(None, &utc, &mut local).is_err() {
            return String::new();
        }
    }
    format!("{:04}-{:02}-{:02} {:02}:{:02}", local.wYear, local.wMonth, local.wDay, local.wHour, local.wMinute)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(human_size(900), "900 bytes");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(25 * 1024 * 1024), "25 MB");
    }

    #[test]
    fn text_detection() {
        assert_eq!(decode_text(b"hello\nworld").as_deref(), Some("hello\nworld"));
        assert_eq!(decode_text(b"\xEF\xBB\xBFbom").as_deref(), Some("bom"));
        assert_eq!(decode_text(b"\xFF\xFEh\0i\0").as_deref(), Some("hi"));
        assert_eq!(decode_text(b"MZ\x90\0\x03\0"), None);
        assert_eq!(decode_text(b""), None);
    }
}
