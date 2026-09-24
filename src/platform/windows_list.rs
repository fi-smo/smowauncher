//! Open top-level windows for the window switcher.

use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, PWSTR};

#[derive(Debug, Clone, PartialEq)]
pub struct WindowInfo {
    pub hwnd: isize,
    pub title: String,
    /// Full path of the owning executable (for the icon), may be empty.
    pub exe: String,
    /// "Code", "firefox" … (exe file stem).
    pub process: String,
}

pub(crate) fn exe_path(pid: u32) -> String {
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return String::new() };
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
        let _ = CloseHandle(h);
        if ok { String::from_utf16_lossy(&buf[..len as usize]) } else { String::new() }
    }
}

fn is_switchable(hwnd: HWND) -> bool {
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() || GetWindowTextLengthW(hwnd) == 0 {
            return false;
        }
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        if ex & WS_EX_TOOLWINDOW.0 != 0 && ex & WS_EX_APPWINDOW.0 == 0 {
            return false;
        }
        if GetWindow(hwnd, GW_OWNER).is_ok_and(|o| !o.is_invalid()) && ex & WS_EX_APPWINDOW.0 == 0 {
            return false;
        }
        // Windows on other virtual desktops and suspended UWP frames are cloaked.
        let mut cloaked = 0u32;
        let _ = DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, &mut cloaked as *mut u32 as *mut _, 4);
        cloaked == 0
    }
}

unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let out = unsafe { &mut *(lparam.0 as *mut Vec<WindowInfo>) };
    if !is_switchable(hwnd) {
        return true.into();
    }
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == std::process::id() {
            return true.into();
        }
        let mut buf = [0u16; 512];
        let n = GetWindowTextW(hwnd, &mut buf);
        let title = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
        let exe = exe_path(pid);
        let process = std::path::Path::new(&exe).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        // The desktop and shell surfaces aren't windows anyone switches to.
        if matches!(process.to_lowercase().as_str(), "textinputhost" | "shellexperiencehost" | "searchhost" | "startmenuexperiencehost")
            || title == "Program Manager"
        {
            return true.into();
        }
        out.push(WindowInfo { hwnd: hwnd.0 as isize, title, exe, process });
    }
    true.into()
}

/// Open windows in Z order (most recently used first).
pub fn list() -> Vec<WindowInfo> {
    let mut out: Vec<WindowInfo> = Vec::with_capacity(32);
    unsafe {
        let _ = EnumWindows(Some(collect), LPARAM(&mut out as *mut _ as isize));
    }
    out
}

pub fn activate(hwnd: isize) {
    let hwnd = HWND(hwnd as *mut _);
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
    }
    super::window::activate(hwnd);
}

pub fn close(hwnd: isize) {
    unsafe {
        let _ = PostMessageW(Some(HWND(hwnd as *mut _)), WM_CLOSE, WPARAM(0), LPARAM(0));
    }
}
