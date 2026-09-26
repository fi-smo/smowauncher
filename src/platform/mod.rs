//! Win32 integration.

pub mod autostart;
pub mod clipboard;
pub mod credentials;
pub mod emoji_render;
pub mod http;
pub mod input;
pub mod instance;
pub mod memory;
pub mod shell;
pub mod window;
pub mod windows_list;

use windows::core::PCWSTR;

/// NUL-terminated UTF-16 for Win32 calls.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn pcwstr(w: &[u16]) -> PCWSTR {
    PCWSTR(w.as_ptr())
}

/// Events delivered from background threads to the UI thread.
#[derive(Debug, Clone)]
pub enum UiEvent {
    /// Win tap / hotkey / tray click: show if hidden, hide if shown.
    Toggle,
    Show,
    /// Clipboard hotkey: open the launcher in clipboard-history mode.
    ShowClipboard,
    /// New clipboard text (text, source process).
    ClipboardText(String, String),
    /// New clipboard image (PNG bytes, width, height, source process).
    ClipboardImage(Vec<u8>, u32, u32, String),
    /// Another window got focus.
    ForegroundChanged(isize),
    OpenSettings,
    /// Windows switched between light and dark app mode.
    ThemeChanged,
    InstallAutostart,
    /// Tray: check GitHub for a new version now.
    CheckUpdates,
    Reindex,
    Quit,
}
