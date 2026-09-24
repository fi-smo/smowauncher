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

/// One-time setup: tool window (no taskbar/Alt+Tab), rounded corners, dark frame, backdrop.
pub fn init(hwnd: HWND, acrylic: bool) {
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
    set_attr(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &BOOL(1));
    set_attr(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &DWM_WINDOW_CORNER_PREFERENCE(DWMWCP_ROUND.0));
    // Subtle light border like Raycast (COLORREF is 0x00BBGGRR).
    set_attr(hwnd, DWMWA_BORDER_COLOR, &0x003A3A3Au32);
    set_backdrop(hwnd, acrylic);
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

/// True if `hwnd` belongs to this process (e.g. a popup of ours).
pub fn is_own(hwnd: HWND) -> bool {
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        pid == std::process::id()
    }
}
