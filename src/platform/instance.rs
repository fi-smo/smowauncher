//! Single instance: a second launch asks the running one to show itself (or quit).

use super::{input, pcwstr, wide};
use windows::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, GetLastError, LPARAM, WPARAM};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, FindWindowW, GetWindowThreadProcessId, PostMessageW,
};

/// Returns false if another instance already runs.
pub fn acquire() -> bool {
    let name = wide("Local\\Smowauncher.Instance");
    unsafe {
        let handle = CreateMutexW(None, false, pcwstr(&name));
        let err = GetLastError();
        // An elevated instance's mutex may not even be openable by a non-elevated process.
        match handle {
            Ok(h) => {
                // Intentionally never closed: the mutex lives as long as the process.
                let _ = h;
                err != ERROR_ALREADY_EXISTS
            }
            Err(_) => err != ERROR_ACCESS_DENIED && err != ERROR_ALREADY_EXISTS,
        }
    }
}

pub fn is_running() -> bool {
    let class = wide(input::MSG_CLASS);
    unsafe { FindWindowW(pcwstr(&class), None) }.is_ok_and(|h| !h.is_invalid())
}

/// Posts `message` (a registered message id) to the running instance.
pub fn signal(message: u32) -> bool {
    let class = wide(input::MSG_CLASS);
    unsafe {
        let Ok(hwnd) = FindWindowW(pcwstr(&class), None) else { return false };
        let mut pid = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let _ = AllowSetForegroundWindow(pid);
        PostMessageW(Some(hwnd), message, WPARAM(0), LPARAM(0)).is_ok()
    }
}
