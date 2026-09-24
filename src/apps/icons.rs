//! Icon disk cache. Extraction happens in the indexer process; the launcher only reads
//! small raw RGBA files (`w:u32 h:u32` + premultiplied RGBA) on demand.
//!
//! Icons are extracted from the shell at a large standard size and downscaled here with an
//! area-averaging filter to the exact on-screen pixel size, so they are drawn 1:1 (sharp)
//! instead of being resampled by the shell from whatever size the icon file happens to have.

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

/// Bump when the cache format or processing changes; stale files are deleted on the next index.
const CACHE_VERSION: u32 = 6;

fn cache_suffix(size: u32) -> String {
    format!("_{size}_v{CACHE_VERSION}.bin")
}

pub fn cache_path(app_id: &str, size: u32) -> PathBuf {
    crate::paths::icon_dir().join(format!("{:016x}{}", fnv1a(app_id), cache_suffix(size)))
}

/// Removes icons from older cache versions or other sizes (e.g. after a DPI change).
fn remove_stale(size: u32) {
    let suffix = cache_suffix(size);
    let Ok(dir) = std::fs::read_dir(crate::paths::icon_dir()) else { return };
    for entry in dir.flatten() {
        if !entry.file_name().to_string_lossy().ends_with(&suffix) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Extracts icons that aren't cached yet. Returns how many were written.
pub fn extract_missing(apps: &[AppEntry], size: u32) -> usize {
    remove_stale(size);
    let mut made = 0;
    let mut sources_used: std::collections::BTreeMap<u32, usize> = Default::default();
    for app in apps {
        let path = cache_path(&app.id, size);
        if path.exists() {
            continue;
        }
        let web_root;
        let source = match (&app.icon, &app.path) {
            (Some(icon), _) => icon,
            (None, Some(path)) if !app.packaged => path,
            // Web links: the shell picks the icon by the URL's file extension (a ".txt" link
            // gets a blank page); the site root gives the default browser's icon instead.
            _ if app.id.starts_with("http://") || app.id.starts_with("https://") => {
                let (scheme, rest) = app.id.split_once("://").unwrap_or_default();
                web_root = format!("{scheme}://{}/", rest.split('/').next().unwrap_or_default());
                &web_root
            }
            _ => &app.launch,
        };
        // Shortcut targets sometimes point at missing files; fall back to the AppsFolder item.
        let pixels = extract(source, size).or_else(|| extract(&app.launch, size));
        if let Some((w, h, rgba, source_size)) = pixels {
            *sources_used.entry(source_size).or_default() += 1;
            let mut buf = Vec::with_capacity(8 + rgba.len());
            buf.extend_from_slice(&w.to_le_bytes());
            buf.extend_from_slice(&h.to_le_bytes());
            buf.extend_from_slice(&rgba);
            if std::fs::write(&path, buf).is_ok() {
                made += 1;
            }
        }
    }
    if made > 0 {
        log::info!("icons: shell sizes used {sources_used:?}");
    }
    made
}

/// Returns premultiplied RGBA whose larger side is exactly `size` pixels, plus the shell
/// size it was made from.
fn extract(parsing_name: &str, size: u32) -> Option<(u32, u32, Vec<u8>, u32)> {
    // Asking the shell for a size larger than anything in the icon file yields a blurry
    // upscale inside a faint frame. Step down until we get a real image.
    let mut framed = None;
    let mut found = None;
    for source in source_sizes(size) {
        let Some(img) = shell_image(parsing_name, source) else { continue };
        if !has_shell_frame(img.0 as usize, img.1 as usize, &img.2) {
            found = Some((img, source));
            break;
        }
        framed.get_or_insert((img, source));
    }
    let ((w, h, mut px), source) = match found {
        Some(f) => f,
        None => {
            // Every size came back framed: use the largest one with the frame cut off.
            let ((w, h, px), source) = framed?;
            (strip_frame(w, h, px), source)
        }
    };
    premultiply(&mut px);
    let scale = size as f32 / w.max(h) as f32;
    let (dw, dh) = (((w as f32 * scale).round() as u32).max(1), ((h as f32 * scale).round() as u32).max(1));
    Some((dw, dh, resample(&px, w as usize, h as usize, dw as usize, dh as usize), source))
}

/// Shell sizes to try, best first: large enough to downscale from (≥ 2× target), then the
/// standard sizes below it for icons that don't contain big images.
fn source_sizes(target: u32) -> Vec<u32> {
    const STANDARD: [u32; 7] = [256, 128, 96, 64, 48, 32, 16];
    let best = STANDARD.iter().rev().copied().find(|&s| s >= target * 2).unwrap_or(256);
    STANDARD.into_iter().filter(|&s| s <= best).collect()
}

fn has_shell_frame(w: usize, h: usize, px: &[u8]) -> bool {
    shell_frame_depth(w, h, px).is_some()
}

/// Detects the frame the shell draws around upscaled small icons: a few pixels of faint
/// alpha along every edge, followed by a transparent gap. Returns the frame thickness
/// (where the gap starts). Real icons start transparent or are opaque right at the edge.
fn shell_frame_depth(w: usize, h: usize, px: &[u8]) -> Option<usize> {
    if w < 8 || h < 8 {
        return None;
    }
    let alpha = |x: usize, y: usize| px[(y * w + x) * 4 + 3];
    let max_depth = (w.min(h) / 16).max(2) + 2;
    let mut depth = 0;
    // Probe inward from the quarter points of all four edges (corners are rounded).
    for t in [w / 4, w / 2, 3 * w / 4] {
        let s = t * h / w; // same fraction along the vertical edges
        let probes: [&dyn Fn(usize) -> u8; 4] = [
            &|d| alpha(t, d),
            &|d| alpha(t, h - 1 - d),
            &|d| alpha(d, s),
            &|d| alpha(w - 1 - d, s),
        ];
        for probe in probes {
            let gap = (1..max_depth).find(|&d| probe(d) <= 4)?;
            if (0..gap).any(|d| !(8..=140).contains(&probe(d))) {
                return None;
            }
            depth = depth.max(gap);
        }
    }
    Some(depth)
}

/// Cuts the shell's frame (and the gap inside it) off a straight-alpha image.
fn strip_frame(w: u32, h: u32, px: Vec<u8>) -> (u32, u32, Vec<u8>) {
    let (w, h) = (w as usize, h as usize);
    let inset = shell_frame_depth(w, h, &px).unwrap_or(0);
    let (cw, ch) = (w - 2 * inset, h - 2 * inset);
    (cw as u32, ch as u32, crop(&px, w, inset, cw, ch))
}

fn crop(px: &[u8], w: usize, inset: usize, cw: usize, ch: usize) -> Vec<u8> {
    (inset..inset + ch).flat_map(|y| px[(y * w + inset) * 4..(y * w + inset + cw) * 4].iter().copied()).collect()
}

/// Shell icon as straight-alpha RGBA.
fn shell_image(parsing_name: &str, size: u32) -> Option<(u32, u32, Vec<u8>)> {
    unsafe {
        let factory: IShellItemImageFactory =
            SHCreateItemFromParsingName(&HSTRING::from(parsing_name), None).ok()?;
        let hbmp = factory.GetImage(SIZE { cx: size as i32, cy: size as i32 }, SIIGBF_ICONONLY).ok()?;
        let result = hbitmap_to_rgba(hbmp);
        let _ = DeleteObject(HGDIOBJ(hbmp.0));
        result
    }
}

/// The shell hands out straight (non-premultiplied) alpha; Slint wants premultiplied.
pub(crate) fn premultiply(px: &mut [u8]) {
    for p in px.chunks_exact_mut(4) {
        let a = p[3] as u32;
        for c in &mut p[..3] {
            *c = ((*c as u32 * a + 127) / 255) as u8;
        }
    }
}

/// For each destination pixel: the source pixels it covers and their weights (summing to 1).
fn box_weights(src: usize, dst: usize) -> Vec<Vec<(usize, f32)>> {
    let scale = src as f32 / dst as f32;
    (0..dst)
        .map(|d| {
            let (start, end) = (d as f32 * scale, (d + 1) as f32 * scale);
            let mut taps = Vec::new();
            let mut s = start.floor() as usize;
            while (s as f32) < end && s < src {
                let cover = end.min(s as f32 + 1.0) - start.max(s as f32);
                if cover > 0.0 {
                    taps.push((s, cover / scale));
                }
                s += 1;
            }
            taps
        })
        .collect()
}

/// Area-averaging resample of premultiplied RGBA (separable: horizontal, then vertical).
pub(crate) fn resample(src: &[u8], sw: usize, sh: usize, dw: usize, dh: usize) -> Vec<u8> {
    if (sw, sh) == (dw, dh) {
        return src.to_vec();
    }
    if dw > sw || dh > sh {
        return bilinear(src, sw, sh, dw, dh);
    }
    let xw = box_weights(sw, dw);
    let yw = box_weights(sh, dh);
    let mut tmp = vec![0f32; sh * dw * 4];
    for y in 0..sh {
        for (dx, taps) in xw.iter().enumerate() {
            let out = &mut tmp[(y * dw + dx) * 4..][..4];
            for &(sx, w) in taps {
                let p = &src[(y * sw + sx) * 4..][..4];
                for c in 0..4 {
                    out[c] += p[c] as f32 * w;
                }
            }
        }
    }
    let mut dst = vec![0u8; dw * dh * 4];
    for (dy, taps) in yw.iter().enumerate() {
        for dx in 0..dw {
            let mut acc = [0f32; 4];
            for &(sy, w) in taps {
                let p = &tmp[(sy * dw + dx) * 4..][..4];
                for c in 0..4 {
                    acc[c] += p[c] * w;
                }
            }
            let out = &mut dst[(dy * dw + dx) * 4..][..4];
            for c in 0..4 {
                out[c] = acc[c].round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    dst
}

/// Bilinear upscale of premultiplied RGBA (for icons whose best native size is small).
fn bilinear(src: &[u8], sw: usize, sh: usize, dw: usize, dh: usize) -> Vec<u8> {
    let coord = |d: usize, dn: usize, sn: usize| {
        let f = ((d as f32 + 0.5) * sn as f32 / dn as f32 - 0.5).clamp(0.0, (sn - 1) as f32);
        let i = f.floor() as usize;
        (i, (i + 1).min(sn - 1), f - i as f32)
    };
    let mut dst = vec![0u8; dw * dh * 4];
    for dy in 0..dh {
        let (y0, y1, ty) = coord(dy, dh, sh);
        for dx in 0..dw {
            let (x0, x1, tx) = coord(dx, dw, sw);
            for c in 0..4 {
                let p = |x: usize, y: usize| src[(y * sw + x) * 4 + c] as f32;
                let top = p(x0, y0) * (1.0 - tx) + p(x1, y0) * tx;
                let bottom = p(x0, y1) * (1.0 - tx) + p(x1, y1) * tx;
                dst[(dy * dw + dx) * 4 + c] = (top * (1.0 - ty) + bottom * ty).round() as u8;
            }
        }
    }
    dst
}

pub(crate) unsafe fn hbitmap_to_rgba(hbmp: HBITMAP) -> Option<(u32, u32, Vec<u8>)> {
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
        // BGRA → RGBA (straight alpha). Bitmaps without an alpha channel get an opaque one.
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

/// `--icon-debug <parsing name>`: prints edge alpha for every shell size (developer aid).
pub fn debug(parsing_name: &str) {
    for source in [256, 128, 96, 64, 48, 32, 16] {
        let Some((w, h, px)) = shell_image(parsing_name, source) else {
            println!("@{source}: failed");
            continue;
        };
        let w_ = w as usize;
        let row = |y: usize| -> Vec<u8> { (0..w_.min(10)).map(|x| px[(y * w_ + x) * 4 + 3]).collect() };
        let mid = |y: usize| px[(y * w_ + w_ / 2) * 4 + 3];
        println!(
            "@{source}: {w}x{h} framed={} rows0-3 {:?} {:?} {:?} {:?} mid-col top4 {:?}",
            has_shell_frame(w_, h as usize, &px),
            row(0),
            row(1),
            row(2),
            row(3),
            [mid(0), mid(1), mid(2), mid(3)]
        );
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn premultiplies() {
        let mut px = vec![255, 128, 0, 128, 10, 20, 30, 0];
        premultiply(&mut px);
        assert_eq!(px, vec![128, 64, 0, 128, 0, 0, 0, 0]);
    }

    #[test]
    fn resample_averages_and_keeps_solid_colors() {
        // 4x4 solid red → 3x3 must stay exactly solid red.
        let src: Vec<u8> = [255, 0, 0, 255].repeat(16);
        assert_eq!(resample(&src, 4, 4, 3, 3), [255, 0, 0, 255].repeat(9));
        // 2x1 black/white → 1x1 grey.
        let src = vec![0, 0, 0, 255, 255, 255, 255, 255];
        assert_eq!(resample(&src, 2, 1, 1, 1), vec![128, 128, 128, 255]);
    }

    #[test]
    fn source_size_order() {
        assert_eq!(source_sizes(39), vec![96, 64, 48, 32, 16]);
        assert_eq!(source_sizes(26), vec![64, 48, 32, 16]);
        assert_eq!(source_sizes(200), vec![256, 128, 96, 64, 48, 32, 16]);
    }

    fn image(w: usize, h: usize, alpha: impl Fn(usize, usize) -> u8) -> Vec<u8> {
        (0..h).flat_map(|y| (0..w).map(move |x| (x, y))).flat_map(|(x, y)| [200, 200, 200, alpha(x, y)]).collect()
    }

    fn edge(x: usize, y: usize, n: usize) -> usize {
        x.min(y).min(n - 1 - x).min(n - 1 - y)
    }

    #[test]
    fn detects_and_strips_shell_frame() {
        // Faint 1px frame, transparent gap, opaque content.
        let framed = image(16, 16, |x, y| match edge(x, y, 16) {
            0 => 60,
            1 => 0,
            _ => 255,
        });
        assert!(has_shell_frame(16, 16, &framed));
        let (w, h, px) = strip_frame(16, 16, framed);
        assert_eq!((w, h), (14, 14));
        assert!(!has_shell_frame(14, 14, &px));

        // What the shell really returns for PuTTY at 96 px: 2px soft frame (48, 36), then a gap.
        let soft = image(96, 96, |x, y| match edge(x, y, 96) {
            0 => 48,
            1 => 36,
            2..=9 => 0,
            _ => 255,
        });
        assert_eq!(shell_frame_depth(96, 96, &soft), Some(2));
        // And at 128 px: 38, 64, 13, then transparent.
        let soft = image(128, 128, |x, y| match edge(x, y, 128) {
            0 => 38,
            1 => 64,
            2 => 13,
            3..=12 => 0,
            _ => 255,
        });
        assert_eq!(shell_frame_depth(128, 128, &soft), Some(3));

        // Normal icon with a transparent border, and a full-bleed square tile.
        let normal = image(16, 16, |x, y| if edge(x, y, 16) == 0 { 0 } else { 255 });
        assert!(!has_shell_frame(16, 16, &normal));
        assert!(!has_shell_frame(16, 16, &image(16, 16, |_, _| 255)));
    }

    #[test]
    fn bilinear_upscale_is_smooth() {
        let src = vec![0, 0, 0, 255, 255, 255, 255, 255];
        let reds: Vec<u8> = resample(&src, 2, 1, 4, 1).chunks(4).map(|p| p[0]).collect();
        assert_eq!(reds, vec![0, 64, 191, 255]);
    }
}
