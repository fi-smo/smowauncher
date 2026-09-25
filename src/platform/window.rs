//! Launcher window effects and visibility.
//!
//! The window is never destroyed or hidden with SW_HIDE. It is *cloaked* by DWM instead:
//! a cloaked window keeps rendering off-screen, so its contents are already up to date
//! the moment we uncloak it. SW_HIDE/SW_SHOW would briefly show the last frame.

use slint::winit_030::WinitWindowAccessor;
use slint::winit_030::winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Dwm::{
    DWM_SYSTEMBACKDROP_TYPE, DWM_WINDOW_CORNER_PREFERENCE, DWMSBT_NONE, DWMSBT_TRANSIENTWINDOW,
    DWMWA_BORDER_COLOR, DWMWA_CLOAK, DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_TRANSITIONS_FORCEDISABLED,
    DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    DwmExtendFrameIntoClientArea, DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
};
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, GetDpiForWindow, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::BOOL;

pub fn hwnd_of(window: &slint::Window) -> Option<HWND> {
    window
        .with_winit_window(|w| match w.window_handle().ok()?.as_raw() {
            RawWindowHandle::Win32(h) => Some(HWND(h.hwnd.get() as *mut _)),
            _ => None,
        })
        .flatten()
}

fn set_attr<T>(hwnd: HWND, attr: windows::Win32::Graphics::Dwm::DWMWINDOWATTRIBUTE, value: &T) {
    unsafe {
        let _ = DwmSetWindowAttribute(hwnd, attr, value as *const T as *const _, size_of::<T>() as u32);
    }
}

/// One-time setup: tool window (no taskbar/Alt+Tab), rounded corners, frame colors, backdrop.
pub fn init(hwnd: HWND, acrylic: bool, dark: bool) {
    cloak(hwnd, true);
    unsafe {
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let ex = (ex | WS_EX_TOOLWINDOW.0 as isize) & !(WS_EX_APPWINDOW.0 as isize);
        // Style changes only reach the taskbar after a hide/show cycle; it's cloaked, so invisible.
        let _ = ShowWindow(hwnd, SW_HIDE);
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex);
        // With the frame extended into the client area (acrylic), DWM would draw the
        // system caption buttons for any window that has a system menu.
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
        let buttons = (WS_SYSMENU.0 | WS_MINIMIZEBOX.0 | WS_MAXIMIZEBOX.0) as isize;
        SetWindowLongPtrW(hwnd, GWL_STYLE, style & !buttons);
        let _ = ShowWindow(hwnd, SW_SHOWNA);
    }
    set_attr(hwnd, DWMWA_TRANSITIONS_FORCEDISABLED, &BOOL(1));
    set_attr(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &DWM_WINDOW_CORNER_PREFERENCE(DWMWCP_ROUND.0));
    set_dark(hwnd, dark);
    set_backdrop(hwnd, acrylic);
}

/// Dark or light acrylic tint and a matching subtle border (COLORREF is 0x00BBGGRR).
pub fn set_dark(hwnd: HWND, dark: bool) {
    set_attr(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &BOOL(dark as i32));
    set_attr(hwnd, DWMWA_BORDER_COLOR, &if dark { 0x003A3A3Au32 } else { 0x00D6D6D6u32 });
}

/// Settings window: dark or light title bar to match the theme, and the app icon.
pub fn style_settings_window(hwnd: HWND, dark: bool) {
    set_attr(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &BOOL(dark as i32));
    unsafe {
        // Centre it on the monitor under the cursor (winit may place it off-screen).
        use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint};
        let mut cursor = POINT::default();
        let _ = GetCursorPos(&mut cursor);
        let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        let mut r = RECT::default();
        if GetMonitorInfoW(MonitorFromPoint(cursor, MONITOR_DEFAULTTOPRIMARY), &mut info).as_bool() && GetWindowRect(hwnd, &mut r).is_ok() {
            let work = info.rcWork;
            let (w, h) = ((r.right - r.left).min(work.right - work.left), (r.bottom - r.top).min(work.bottom - work.top));
            let x = work.left + (work.right - work.left - w) / 2;
            let y = work.top + (work.bottom - work.top - h) / 2;
            let _ = SetWindowPos(hwnd, None, x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
        }
        use windows::Win32::Foundation::{LPARAM, WPARAM};
        use windows::Win32::System::LibraryLoader::GetModuleHandleW;
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        for (which, size) in [(ICON_SMALL, SM_CXSMICON), (ICON_BIG, SM_CXICON)] {
            let px = GetSystemMetrics(size);
            if let Ok(icon) = LoadImageW(Some(hinst.into()), windows::core::PCWSTR(1 as *const u16), IMAGE_ICON, px, px, LR_DEFAULTCOLOR) {
                SendMessageW(hwnd, WM_SETICON, Some(WPARAM(which as usize)), Some(LPARAM(icon.0 as isize)));
            }
        }
    }
}

/// Windows' "app mode" (Settings → Personalization → Colors).
pub fn system_prefers_dark() -> bool {
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
    let mut value = 0u32;
    let mut len = 4u32;
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            windows::core::w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            windows::core::w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut _),
            Some(&mut len),
        )
    };
    // Missing value = Windows default (dark apps unless the user chose light).
    r.is_err() || value == 0
}

pub fn set_backdrop(hwnd: HWND, acrylic: bool) {
    unsafe {
        let m = if acrylic { -1 } else { 0 };
        let margins = MARGINS { cxLeftWidth: m, cxRightWidth: m, cyTopHeight: m, cyBottomHeight: m };
        let _ = DwmExtendFrameIntoClientArea(hwnd, &margins);
    }
    let kind = if acrylic { DWMSBT_TRANSIENTWINDOW } else { DWMSBT_NONE };
    set_attr(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, &DWM_SYSTEMBACKDROP_TYPE(kind.0));
}

/// winit registers the process for raw keyboard and mouse input (for device events, which we
/// don't use). Raw input still reports the Win key that our keyboard hook swallows, and on
/// Windows 11 26200.9550+ that makes Start open as soon as one of our windows takes the focus
/// after a Win tap. Registration is per process and winit only does it once, at startup.
pub fn unregister_raw_input() -> bool {
    use windows::Win32::UI::Input::{RAWINPUTDEVICE, RIDEV_REMOVE, RegisterRawInputDevices};
    // Generic desktop page: 2 = mouse, 6 = keyboard.
    let device = |usage| RAWINPUTDEVICE { usUsagePage: 1, usUsage: usage, dwFlags: RIDEV_REMOVE, hwndTarget: HWND::default() };
    unsafe { RegisterRawInputDevices(&[device(2), device(6)], size_of::<RAWINPUTDEVICE>() as u32).is_ok() }
}

pub fn cloak(hwnd: HWND, cloaked: bool) {
    set_attr(hwnd, DWMWA_CLOAK, &BOOL(cloaked as i32));
}

/// Centers the window horizontally in the upper part of the monitor under the mouse.
/// Returns false if the window had to change DPI (its size will settle on the next frame).
pub fn place(hwnd: HWND, logical_w: f64, logical_h: f64) -> bool {
    unsafe {
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let mon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(mon, &mut mi);
        let (mut dpi_x, mut dpi_y) = (96u32, 96u32);
        let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
        let same_dpi = GetDpiForWindow(hwnd) == dpi_x;

        let scale = dpi_x as f64 / 96.0;
        let w = (logical_w * scale).round() as i32;
        let h = (logical_h * scale).round() as i32;
        let wa: RECT = mi.rcWork;
        let x = wa.left + ((wa.right - wa.left) - w) / 2;
        let y = wa.top + ((wa.bottom - wa.top) as f64 * 0.2) as i32;
        let flags = if same_dpi { SWP_NOSIZE | SWP_NOACTIVATE } else { SWP_NOACTIVATE };
        let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), x, y, w, h, flags);
        same_dpi
    }
}

/// Brings the window to the foreground, working around the foreground lock if needed.
pub fn activate(hwnd: HWND) -> bool {
    unsafe {
        if GetForegroundWindow() == hwnd {
            return true;
        }
        if SetForegroundWindow(hwnd).as_bool() && GetForegroundWindow() == hwnd {
            return true;
        }
        let fg = GetForegroundWindow();
        let fg_thread = GetWindowThreadProcessId(fg, None);
        let me = GetCurrentThreadId();
        let attached = fg_thread != 0 && fg_thread != me && AttachThreadInput(me, fg_thread, true).as_bool();
        let _ = BringWindowToTop(hwnd);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(Some(hwnd));
        if attached {
            let _ = AttachThreadInput(me, fg_thread, false);
        }
        let ok = GetForegroundWindow() == hwnd;
        if !ok {
            log::warn!("window: could not take foreground");
        }
        ok
    }
}

/// Saves what's on screen inside `hwnd`'s rectangle as a 24-bit BMP (developer aid).
pub fn capture(hwnd: HWND, out: &std::path::Path) -> Result<(), String> {
    use windows::Win32::Graphics::Gdi::*;
    unsafe {
        let mut r = RECT::default();
        GetWindowRect(hwnd, &mut r).map_err(|e| e.to_string())?;
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        let screen = GetDC(None);
        let mem = CreateCompatibleDC(Some(screen));
        let bmp = CreateCompatibleBitmap(screen, w, h);
        let old = SelectObject(mem, HGDIOBJ(bmp.0));
        let _ = BitBlt(mem, 0, 0, w, h, Some(screen), r.left, r.top, SRCCOPY);
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: h, // bottom-up, as BMP files store it
                biPlanes: 1,
                biBitCount: 24,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let stride = ((w * 3 + 3) & !3) as usize;
        let mut px = vec![0u8; stride * h as usize];
        GetDIBits(mem, bmp, 0, h as u32, Some(px.as_mut_ptr() as *mut _), &mut info, DIB_RGB_COLORS);
        SelectObject(mem, old);
        let _ = DeleteObject(HGDIOBJ(bmp.0));
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);

        let mut file = Vec::with_capacity(54 + px.len());
        file.extend(b"BM");
        file.extend(((54 + px.len()) as u32).to_le_bytes());
        file.extend(0u32.to_le_bytes());
        file.extend(54u32.to_le_bytes());
        let header = std::slice::from_raw_parts(&info.bmiHeader as *const _ as *const u8, 40);
        file.extend(header);
        file.extend(px);
        std::fs::write(out, file).map_err(|e| e.to_string())
    }
}

pub fn foreground() -> HWND {
    unsafe { GetForegroundWindow() }
}

pub fn restore_foreground(hwnd: HWND) {
    unsafe {
        if !hwnd.is_invalid() && IsWindow(Some(hwnd)).as_bool() {
            let _ = SetForegroundWindow(hwnd);
        }
    }
}

/// "ClassName (process.exe)" of a window, for diagnostics.
pub fn describe(hwnd: HWND) -> String {
    unsafe {
        let mut class = [0u16; 128];
        let n = GetClassNameW(hwnd, &mut class);
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let exe = super::windows_list::exe_path(pid);
        let exe = exe.rsplit('\\').next().unwrap_or_default().to_owned();
        format!("{} ({exe})", String::from_utf16_lossy(&class[..n.max(0) as usize]))
    }
}

/// True if `hwnd` belongs to this process (e.g. a popup of ours).
pub fn is_own(hwnd: HWND) -> bool {
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        pid == std::process::id()
    }
}
