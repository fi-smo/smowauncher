//! Small image helpers for clipboard images: PNG encode/decode, Windows DIB conversion and
//! thumbnails. Pixels are straight (non-premultiplied) RGBA unless a name says otherwise.

pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub px: Vec<u8>,
}

/// Images bigger than this are not kept in the clipboard history.
pub const MAX_PIXELS: u64 = 40_000_000;

pub fn png_size(png: &[u8]) -> Option<(u32, u32)> {
    // Signature (8) + IHDR length/type (8), then width and height, big-endian.
    if png.len() < 24 || &png[..8] != b"\x89PNG\r\n\x1a\n" || &png[12..16] != b"IHDR" {
        return None;
    }
    let w = u32::from_be_bytes(png[16..20].try_into().ok()?);
    let h = u32::from_be_bytes(png[20..24].try_into().ok()?);
    (w > 0 && h > 0).then_some((w, h))
}

pub fn decode_png(png: &[u8]) -> Option<Rgba> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(png));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    buf.truncate(info.buffer_size());
    let px = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => buf.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        png::ColorType::GrayscaleAlpha => buf.chunks_exact(2).flat_map(|p| [p[0], p[0], p[0], p[1]]).collect(),
        png::ColorType::Grayscale => buf.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::Indexed => return None, // expanded by normalize_to_color8
    };
    Some(Rgba { width: info.width, height: info.height, px })
}

pub fn encode_png(img: &Rgba) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, img.width, img.height);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_compression(png::Compression::Fast);
        let mut writer = enc.write_header().ok()?;
        writer.write_image_data(&img.px).ok()?;
        writer.finish().ok()?;
    }
    Some(out)
}

/// CF_DIB / CF_DIBV5 clipboard data (BITMAPINFOHEADER or V4/V5 header + pixels) to RGBA.
/// Handles the 24- and 32-bit uncompressed/bitfield layouts apps actually put there.
pub fn from_dib(dib: &[u8]) -> Option<Rgba> {
    let u32_at = |o: usize| dib.get(o..o + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
    let header = u32_at(0)? as usize;
    let width = i32::from_le_bytes(dib.get(4..8)?.try_into().ok()?);
    let height = i32::from_le_bytes(dib.get(8..12)?.try_into().ok()?);
    let bits = u16::from_le_bytes(dib.get(14..16)?.try_into().ok()?);
    let compression = u32_at(16)?;
    const BI_RGB: u32 = 0;
    const BI_BITFIELDS: u32 = 3;
    if width <= 0 || height == 0 || !(bits == 24 || bits == 32) || !(compression == BI_RGB || compression == BI_BITFIELDS) {
        return None;
    }
    let (w, h) = (width as usize, height.unsigned_abs() as usize);
    if (w * h) as u64 > MAX_PIXELS {
        return None;
    }
    // With a plain 40-byte header, bitfield masks follow it.
    let offset = header + if compression == BI_BITFIELDS && header == 40 { 12 } else { 0 };
    let stride = (w * bits as usize / 8).div_ceil(4) * 4;
    let data = dib.get(offset..offset + stride * h)?;
    let mut px = Vec::with_capacity(w * h * 4);
    for row in 0..h {
        // Positive height = bottom-up rows.
        let src_row = if height > 0 { h - 1 - row } else { row };
        let line = &data[src_row * stride..];
        for x in 0..w {
            match bits {
                32 => {
                    let p = &line[x * 4..x * 4 + 4];
                    px.extend([p[2], p[1], p[0], p[3]]);
                }
                _ => {
                    let p = &line[x * 3..x * 3 + 3];
                    px.extend([p[2], p[1], p[0], 255]);
                }
            }
        }
    }
    // Most apps leave the alpha byte of 32-bit DIBs at 0: that means opaque.
    if bits == 32 && px.chunks_exact(4).all(|p| p[3] == 0) {
        px.chunks_exact_mut(4).for_each(|p| p[3] = 255);
    }
    Some(Rgba { width: w as u32, height: h as u32, px })
}

/// RGBA to CF_DIB data: BITMAPINFOHEADER + bottom-up 32-bit BGRA rows.
pub fn to_dib(img: &Rgba) -> Vec<u8> {
    let (w, h) = (img.width as usize, img.height as usize);
    let mut out = Vec::with_capacity(40 + w * h * 4);
    out.extend(40u32.to_le_bytes());
    out.extend((w as i32).to_le_bytes());
    out.extend((h as i32).to_le_bytes());
    out.extend(1u16.to_le_bytes());
    out.extend(32u16.to_le_bytes());
    out.extend(0u32.to_le_bytes()); // BI_RGB
    out.extend(((w * h * 4) as u32).to_le_bytes());
    out.extend([0u8; 16]); // resolution, palette
    for row in (0..h).rev() {
        for p in img.px[row * w * 4..(row + 1) * w * 4].chunks_exact(4) {
            out.extend([p[2], p[1], p[0], p[3]]);
        }
    }
    out
}

/// A premultiplied thumbnail fitting in `size`×`size` (aspect kept, centered).
pub fn thumbnail(img: &Rgba, size: u32) -> slint::Image {
    let scale = (size as f64 / img.width.max(img.height) as f64).min(1.0);
    let tw = ((img.width as f64 * scale).round() as u32).max(1);
    let th = ((img.height as f64 * scale).round() as u32).max(1);
    let mut px = img.px.clone();
    crate::apps::icons::premultiply(&mut px);
    let small = crate::apps::icons::resample(&px, img.width as usize, img.height as usize, tw as usize, th as usize);
    // Center in a square so rows stay aligned.
    let mut square = vec![0u8; (size * size * 4) as usize];
    let (ox, oy) = ((size - tw) / 2, (size - th) / 2);
    for y in 0..th {
        let dst = (((oy + y) * size + ox) * 4) as usize;
        let src = (y * tw * 4) as usize;
        square[dst..dst + (tw * 4) as usize].copy_from_slice(&small[src..src + (tw * 4) as usize]);
    }
    let buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(&square, size, size);
    slint::Image::from_rgba8_premultiplied(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_and_dib_round_trip() {
        let img = Rgba { width: 3, height: 2, px: (0..24).map(|i| (i * 10) as u8).collect() };
        let png = encode_png(&img).unwrap();
        assert_eq!(png_size(&png), Some((3, 2)));
        let back = decode_png(&png).unwrap();
        assert_eq!(back.px, img.px);
        let dib = to_dib(&img);
        let back = from_dib(&dib).unwrap();
        assert_eq!((back.width, back.height), (3, 2));
        assert_eq!(back.px, img.px);
    }

    #[test]
    fn zero_alpha_dib_is_opaque() {
        let img = Rgba { width: 1, height: 1, px: vec![10, 20, 30, 0] };
        let back = from_dib(&to_dib(&img)).unwrap();
        assert_eq!(back.px, vec![10, 20, 30, 255]);
    }
}
