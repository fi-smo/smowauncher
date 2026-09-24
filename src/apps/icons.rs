//! Icon disk cache. Extraction happens in the indexer process; the launcher only reads
//! small raw RGBA files (`w:u32 h:u32` + premultiplied RGBA) on demand.

use super::AppEntry;
use std::path::PathBuf;
use windows::Win32::Foundation::SIZE;
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, DeleteObject, GetDC, GetDIBits,
    GetObjectW, HBITMAP, HGDIOBJ, ReleaseDC,
};
use windows::Win32::UI::Shell::{
    IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_ICONONLY,
};
use windows::core::HSTRING;

fn fnv1a(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

pub fn cache_path(app_id: &str, size: u32) -> PathBuf {
    crate::paths::icon_dir().join(format!("{:016x}_{size}.bin", fnv1a(app_id)))
}

/// Extracts icons that aren't cached yet. Returns how many were written.
pub fn extract_missing(apps: &[AppEntry], size: u32) -> usize {
    let mut made = 0;
    for app in apps {
        let path = cache_path(&app.id, size);
        if path.exists() {
            continue;
        }
        let source = if app.packaged || app.path.is_none() { &app.launch } else { app.path.as_ref().unwrap() };
        // Shortcut targets sometimes point at missing files; fall back to the AppsFolder item.
        let pixels = extract(source, size).or_else(|| extract(&app.launch, size));
        if let Some((w, h, rgba)) = pixels {
            let mut buf = Vec::with_capacity(8 + rgba.len());
            buf.extend_from_slice(&w.to_le_bytes());
            buf.extend_from_slice(&h.to_le_bytes());
            buf.extend_from_slice(&rgba);
            if std::fs::write(&path, buf).is_ok() {
                made += 1;
            }
        }
    }
    made
}

fn extract(parsing_name: &str, size: u32) -> Option<(u32, u32, Vec<u8>)> {
    unsafe {
        let factory: IShellItemImageFactory =
            SHCreateItemFromParsingName(&HSTRING::from(parsing_name), None).ok()?;
        let hbmp = factory.GetImage(SIZE { cx: size as i32, cy: size as i32 }, SIIGBF_ICONONLY).ok()?;
        let result = hbitmap_to_rgba(hbmp);
        let _ = DeleteObject(HGDIOBJ(hbmp.0));
        result
    }
}

unsafe fn hbitmap_to_rgba(hbmp: HBITMAP) -> Option<(u32, u32, Vec<u8>)> {
    unsafe {
        let mut bm = BITMAP::default();
        if GetObjectW(HGDIOBJ(hbmp.0), size_of::<BITMAP>() as i32, Some(&mut bm as *mut _ as *mut _)) == 0 {
            return None;
        }
        let (w, h) = (bm.bmWidth, bm.bmHeight.abs());
        if w <= 0 || h <= 0 {
            return None;
        }
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h, // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut px = vec![0u8; (w * h * 4) as usize];
        let dc = GetDC(None);
        let lines = GetDIBits(dc, hbmp, 0, h as u32, Some(px.as_mut_ptr() as *mut _), &mut info, DIB_RGB_COLORS);
        ReleaseDC(None, dc);
        if lines == 0 {
            return None;
        }
        // BGRA (premultiplied) → RGBA (premultiplied). Bitmaps without alpha get an opaque one.
        let has_alpha = px.chunks_exact(4).any(|p| p[3] != 0);
        for p in px.chunks_exact_mut(4) {
            p.swap(0, 2);
            if !has_alpha {
                p[3] = 255;
            }
        }
        Some((w as u32, h as u32, px))
    }
}

/// Loads a cached icon as a Slint image (UI thread).
pub fn load(app_id: &str, size: u32) -> Option<slint::Image> {
    let bytes = std::fs::read(cache_path(app_id, size)).ok()?;
    if bytes.len() < 8 {
        return None;
    }
    let w = u32::from_le_bytes(bytes[0..4].try_into().ok()?);
    let h = u32::from_le_bytes(bytes[4..8].try_into().ok()?);
    let data = &bytes[8..];
    if data.len() != (w * h * 4) as usize {
        return None;
    }
    let buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(data, w, h);
    Some(slint::Image::from_rgba8_premultiplied(buf))
}

pub fn clear_cache() {
    let _ = std::fs::remove_dir_all(crate::paths::icon_dir());
}
