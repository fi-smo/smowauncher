//! Color emoji as bitmaps. Slint's software renderer draws fonts in one color, so emoji
//! are rendered with Direct2D + DirectWrite (which support color fonts) into a WIC bitmap.

use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT};
use windows::Win32::Graphics::Direct2D::{
    D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_RENDER_TARGET_PROPERTIES,
    D2D1CreateFactory, ID2D1Factory,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_NORMAL,
    DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_CENTER, DWriteCreateFactory,
    IDWriteFactory,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICImagingFactory, WICBitmapCacheOnLoad,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::core::w;

struct Renderer {
    d2d: ID2D1Factory,
    dwrite: IDWriteFactory,
    wic: IWICImagingFactory,
}

thread_local! {
    // Created on first use on the UI thread (COM is already initialized there by winit).
    // Leaked: releasing COM objects during thread-local teardown, after COM was shut
    // down, crashes the process on exit.
    static RENDERER: &'static Option<Renderer> = Box::leak(Box::new(unsafe {
        let d2d = D2D1CreateFactory::<ID2D1Factory>(D2D1_FACTORY_TYPE_SINGLE_THREADED, None);
        let dwrite = DWriteCreateFactory::<IDWriteFactory>(DWRITE_FACTORY_TYPE_SHARED);
        let wic = CoCreateInstance::<_, IWICImagingFactory>(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER);
        match (d2d, dwrite, wic) {
            (Ok(d2d), Ok(dwrite), Ok(wic)) => Some(Renderer { d2d, dwrite, wic }),
            (a, b, c) => {
                log::warn!("emoji renderer unavailable: {:?} {:?} {:?}", a.err(), b.err(), c.err());
                None
            }
        }
    }));
}

/// `size`×`size` premultiplied RGBA image of `text` in Segoe UI Emoji, or `None` on failure.
pub fn render(text: &str, size: u32) -> Option<slint::Image> {
    RENDERER.with(|r| {
        let r = r.as_ref()?;
        let rgba = unsafe { draw(r, text, size) }.map_err(|e| log::warn!("emoji {text}: {e}")).ok()?;
        let buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(&rgba, size, size);
        Some(slint::Image::from_rgba8_premultiplied(buf))
    })
}

unsafe fn draw(r: &Renderer, text: &str, size: u32) -> windows::core::Result<Vec<u8>> {
    unsafe {
        let bitmap = r.wic.CreateBitmap(size, size, &GUID_WICPixelFormat32bppPBGRA, WICBitmapCacheOnLoad)?;
        let props = D2D1_RENDER_TARGET_PROPERTIES {
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            ..Default::default()
        };
        let target = r.d2d.CreateWicBitmapRenderTarget(&bitmap, &props)?;
        // Glyphs fill most of their em box; leave a little room so nothing is clipped.
        let format = r.dwrite.CreateTextFormat(
            w!("Segoe UI Emoji"),
            None,
            DWRITE_FONT_WEIGHT_NORMAL,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            size as f32 * 0.78,
            w!("en-us"),
        )?;
        format.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
        format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        let brush = target.CreateSolidColorBrush(&D2D1_COLOR_F { r: 0.5, g: 0.5, b: 0.5, a: 1.0 }, None)?;
        let wide: Vec<u16> = text.encode_utf16().collect();
        let rect = D2D_RECT_F { left: 0.0, top: 0.0, right: size as f32, bottom: size as f32 };
        target.BeginDraw();
        target.Clear(Some(&D2D1_COLOR_F::default()));
        target.DrawText(&wide, &format, &rect, &brush, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT, DWRITE_MEASURING_MODE_NATURAL);
        target.EndDraw(None, None)?;

        let mut px = vec![0u8; (size * size * 4) as usize];
        bitmap.CopyPixels(std::ptr::null(), size * 4, &mut px)?;
        // BGRA -> RGBA (both premultiplied).
        for p in px.chunks_exact_mut(4) {
            p.swap(0, 2);
        }
        Ok(px)
    }
}
