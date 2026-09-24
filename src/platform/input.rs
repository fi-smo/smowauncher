//! The "input" thread: low-level keyboard hook (Win-key tap), global hotkey, tray icon,
//! foreground-change notifications and the single-instance message window.
//!
//! The hook callback runs for *every* keystroke system-wide, so it must stay tiny: it only
//! flips atomics and posts events. If it ever takes longer than `LowLevelHooksTimeout`,
//! Windows silently removes the hook.

use super::{UiEvent, pcwstr, wide};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{
    GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_HIGHEST,
};
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, HOT_KEY_MODIFIERS, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS,
    KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, MOD_NOREPEAT, RegisterHotKey, SendInput, UnregisterHotKey,
    VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW, QUNS_PRESENTATION_MODE,
    QUNS_RUNNING_D3D_FULL_SCREEN, SHQueryUserNotificationState, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::*;

pub const MSG_CLASS: &str = "Smowauncher.MessageWindow";

/// Toggled from the tray menu and config.
pub static WIN_KEY_ENABLED: AtomicBool = AtomicBool::new(true);
pub static FULLSCREEN_PASSTHROUGH: AtomicBool = AtomicBool::new(true);

static WIN_DOWN: AtomicBool = AtomicBool::new(false);
/// A Win press is being held back from Windows (see `keyboard_proc`).
static WIN_PENDING: AtomicBool = AtomicBool::new(false);
/// Which Win key (left/right) is held back, for replaying it.
static WIN_VK: AtomicU32 = AtomicU32::new(0);
static MSG_HWND: AtomicIsize = AtomicIsize::new(0);
/// `(mods << 16) | vk` per slot (0 = launcher, 1 = clipboard history); 0 = no hotkey.
static HOTKEYS: [AtomicU32; 2] = [AtomicU32::new(0), AtomicU32::new(0)];
/// Record clipboard changes for the history.
pub static CLIPBOARD_HISTORY: AtomicBool = AtomicBool::new(true);
static MSG_SHOW: AtomicU32 = AtomicU32::new(0);
static MSG_QUIT: AtomicU32 = AtomicU32::new(0);
static MSG_TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);
static SINK: OnceLock<Box<dyn Fn(UiEvent) + Send + Sync>> = OnceLock::new();

/// Marks input we inject ourselves so the hook ignores it.
const INJECT_TAG: usize = 0x534D_4F57; // "SMOW"
/// Unassigned virtual key (see `send_dummy_key`).
const VK_DUMMY: u16 = 0xE8;
const WM_TRAY: u32 = WM_APP + 1;
const WM_APPLY_HOTKEY: u32 = WM_APP + 2;
const HOTKEY_IDS: [i32; 2] = [1, 2];
const TRAY_ID: u32 = 1;

fn emit(ev: UiEvent) {
    if let Some(sink) = SINK.get() {
        sink(ev);
    }
}

pub fn registered_message(name: &str) -> u32 {
    let w = wide(name);
    unsafe { RegisterWindowMessageW(pcwstr(&w)) }
}

pub fn show_message() -> u32 {
    registered_message("Smowauncher.Show")
}

pub fn quit_message() -> u32 {
    registered_message("Smowauncher.Quit")
}

/// Starts the input thread. `sink` is invoked on that thread; it must hand work to the UI thread.
pub fn spawn(sink: impl Fn(UiEvent) + Send + Sync + 'static, hotkey: &str, clipboard_hotkey: &str) {
    let _ = SINK.set(Box::new(sink));
    store_hotkey(0, hotkey);
    store_hotkey(1, clipboard_hotkey);
    std::thread::Builder::new()
        .name("input".into())
        .stack_size(256 * 1024)
        .spawn(thread_main)
        .expect("spawn input thread");
}

fn store_hotkey(slot: usize, hotkey: &str) {
    let packed = match crate::config::parse_hotkey(hotkey) {
        Some((mods, vk)) => (mods << 16) | vk,
        None => {
            if !hotkey.trim().is_empty() {
                log::warn!("hotkey: cannot parse {hotkey:?}");
            }
            0
        }
    };
    HOTKEYS[slot].store(packed, Ordering::Relaxed);
}

/// Re-registers the hotkeys (e.g. after a config change).
pub fn set_hotkeys(hotkey: &str, clipboard_hotkey: &str) {
    store_hotkey(0, hotkey);
    store_hotkey(1, clipboard_hotkey);
    post(WM_APPLY_HOTKEY);
}

/// Removes the tray icon and stops the thread.
pub fn shutdown() {
    post(WM_CLOSE);
}

fn post(msg: u32) {
    let h = MSG_HWND.load(Ordering::Acquire);
    if h != 0 {
        unsafe {
            let _ = PostMessageW(Some(HWND(h as *mut _)), msg, WPARAM(0), LPARAM(0));
        }
    }
}

fn thread_main() {
    unsafe {
        let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST);
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let class = wide(MSG_CLASS);
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: pcwstr(&class),
            ..Default::default()
        };
        RegisterClassW(&wc);
        let title = wide("Smowauncher");
        let hwnd = match CreateWindowExW(
            WS_EX_TOOLWINDOW,
            pcwstr(&class),
            pcwstr(&title),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinst.into()),
            None,
        ) {
            Ok(h) => h,
            Err(e) => {
                log::error!("input: CreateWindowEx failed: {e}");
                return;
            }
        };
        MSG_HWND.store(hwnd.0 as isize, Ordering::Release);

        MSG_SHOW.store(show_message(), Ordering::Relaxed);
        MSG_QUIT.store(quit_message(), Ordering::Relaxed);
        MSG_TASKBAR_CREATED.store(registered_message("TaskbarCreated"), Ordering::Relaxed);
        // We usually run elevated; let non-elevated processes (second instance, Explorer) reach us.
        for msg in [&MSG_SHOW, &MSG_QUIT, &MSG_TASKBAR_CREATED] {
            let _ = ChangeWindowMessageFilterEx(hwnd, msg.load(Ordering::Relaxed), MSGFLT_ALLOW, None);
        }

        tray_add(hwnd);

        let hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), Some(hinst.into()), 0);
        if let Err(e) = &hook {
            log::error!("input: keyboard hook failed: {e}");
        }
        let fg_hook = SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_FOREGROUND,
            None,
            Some(foreground_proc),
            0,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        );
        apply_hotkey(hwnd);
        if let Err(e) = windows::Win32::System::DataExchange::AddClipboardFormatListener(hwnd) {
            log::warn!("input: clipboard listener failed: {e}");
        }
        log::info!("input: ready");

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        if let Ok(h) = hook {
            let _ = UnhookWindowsHookEx(h);
        }
        if !fg_hook.is_invalid() {
            let _ = UnhookWinEvent(fg_hook);
        }
    }
}

unsafe fn apply_hotkey(hwnd: HWND) {
    unsafe {
        for (slot, id) in HOTKEY_IDS.into_iter().enumerate() {
            let _ = UnregisterHotKey(Some(hwnd), id);
            let packed = HOTKEYS[slot].load(Ordering::Relaxed);
            if packed == 0 {
                continue;
            }
            let (mods, vk) = (packed >> 16, packed & 0xFFFF);
            if let Err(e) = RegisterHotKey(Some(hwnd), id, HOT_KEY_MODIFIERS(mods) | MOD_NOREPEAT, vk) {
                log::warn!("hotkey {id}: registration failed (already used by another app?): {e}");
            }
        }
    }
}

fn fullscreen_app_running() -> bool {
    match unsafe { SHQueryUserNotificationState() } {
        Ok(state) => state == QUNS_RUNNING_D3D_FULL_SCREEN || state == QUNS_PRESENTATION_MODE,
        Err(_) => false,
    }
}

fn key_held(vk: VIRTUAL_KEY) -> bool {
    unsafe { GetAsyncKeyState(vk.0 as i32) < 0 }
}

/// Sends Ctrl+V to whatever window has focus (pasting a clipboard-history entry).
pub fn send_paste() {
    let key = |vk: VIRTUAL_KEY, flags: KEYBD_EVENT_FLAGS| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: vk, wScan: 0, dwFlags: flags, time: 0, dwExtraInfo: INJECT_TAG } },
    };
    let v = VIRTUAL_KEY(b'V' as u16);
    let inputs = [
        key(VK_CONTROL, KEYBD_EVENT_FLAGS(0)),
        key(v, KEYBD_EVENT_FLAGS(0)),
        key(v, KEYEVENTF_KEYUP),
        key(VK_CONTROL, KEYEVENTF_KEYUP),
    ];
    unsafe {
        SendInput(&inputs, size_of::<INPUT>() as i32);
    }
}

fn key_input(vk: u16, scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: VIRTUAL_KEY(vk), wScan: scan, dwFlags: flags, time: 0, dwExtraInfo: INJECT_TAG } },
    }
}

/// Presses and releases an unassigned key. Being the source of the latest input event is
/// what lets our process bring the launcher to the foreground.
fn send_dummy_key() {
    let inputs = [key_input(VK_DUMMY, 0, KEYBD_EVENT_FLAGS(0)), key_input(VK_DUMMY, 0, KEYEVENTF_KEYUP)];
    unsafe {
        SendInput(&inputs, size_of::<INPUT>() as i32);
    }
}

/// A key was pressed while we held back a Win press: it's a shortcut (Win+E, Win+Shift+S…).
/// Replay the Win press followed by this key, so Windows gets the combo in the right order.
fn replay_combo(win_vk: u16, kb: &KBDLLHOOKSTRUCT, key_up: bool) {
    let mut flags = if kb.flags.0 & LLKHF_EXTENDED.0 != 0 { KEYEVENTF_EXTENDEDKEY } else { KEYBD_EVENT_FLAGS(0) };
    if key_up {
        flags |= KEYEVENTF_KEYUP;
    }
    let inputs = [key_input(win_vk, 0, KEYEVENTF_EXTENDEDKEY), key_input(kb.vkCode as u16, kb.scanCode as u16, flags)];
    unsafe {
        SendInput(&inputs, size_of::<INPUT>() as i32);
    }
}

/// Win key handling. A Win press is *held back* (not passed to Windows) until we know what
/// it is: released alone → toggle the launcher, and Windows never saw a Win press, so the
/// Start menu can't open; another key first → replay "Win down, key" so shortcuts work.
/// (Injecting a dummy key to cancel Start instead fails while an elevated window — like the
/// launcher itself — has focus: Explorer can't observe input sent to higher-integrity windows.)
unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let kb = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        let ours = kb.dwExtraInfo == INJECT_TAG;
        if !ours {
            let msg = wparam.0 as u32;
            let down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
            let vk = kb.vkCode as u16;
            if vk == VK_LWIN.0 || vk == VK_RWIN.0 {
                if down {
                    if WIN_DOWN.swap(true, Ordering::Relaxed) {
                        // Auto-repeat while held: keep swallowing if we're holding it back.
                        if WIN_PENDING.load(Ordering::Relaxed) {
                            return LRESULT(1);
                        }
                    } else {
                        let eligible = WIN_KEY_ENABLED.load(Ordering::Relaxed)
                            && !key_held(VK_CONTROL)
                            && !key_held(VK_MENU)
                            && !key_held(VK_SHIFT)
                            && !(FULLSCREEN_PASSTHROUGH.load(Ordering::Relaxed) && fullscreen_app_running());
                        WIN_PENDING.store(eligible, Ordering::Relaxed);
                        if eligible {
                            WIN_VK.store(vk as u32, Ordering::Relaxed);
                            return LRESULT(1);
                        }
                    }
                } else {
                    WIN_DOWN.store(false, Ordering::Relaxed);
                    if WIN_PENDING.swap(false, Ordering::Relaxed) {
                        // Released alone: Windows never saw this Win press.
                        send_dummy_key();
                        emit(UiEvent::Toggle);
                        return LRESULT(1);
                    }
                }
            } else if WIN_DOWN.load(Ordering::Relaxed) && WIN_PENDING.swap(false, Ordering::Relaxed) {
                // First other key while Win is held back: it's a shortcut, give it to Windows.
                // Key-ups (e.g. a modifier pressed before Win) are replayed the same way.
                replay_combo(WIN_VK.load(Ordering::Relaxed) as u16, kb, !down);
                return LRESULT(1);
            }
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

unsafe extern "system" fn foreground_proc(
    _hook: HWINEVENTHOOK,
    _event: u32,
    hwnd: HWND,
    _id_object: i32,
    _id_child: i32,
    _thread: u32,
    _time: u32,
) {
    emit(UiEvent::ForegroundChanged(hwnd.0 as isize));
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_TRAY => {
                match (lparam.0 & 0xFFFF) as u32 {
                    WM_LBUTTONUP => emit(UiEvent::Toggle),
                    WM_RBUTTONUP | WM_CONTEXTMENU => tray_menu(hwnd),
                    _ => {}
                }
                LRESULT(0)
            }
            WM_HOTKEY => {
                emit(if wparam.0 as i32 == HOTKEY_IDS[1] { UiEvent::ShowClipboard } else { UiEvent::Toggle });
                LRESULT(0)
            }
            WM_CLIPBOARDUPDATE => {
                // Reading can wait on the clipboard owner; never do it on the hook thread.
                if CLIPBOARD_HISTORY.load(Ordering::Relaxed) {
                    let _ = std::thread::Builder::new().name("clip-read".into()).stack_size(128 * 1024).spawn(|| {
                        if let Some((text, source)) = super::clipboard::read_for_history(crate::clip::MAX_TEXT_BYTES) {
                            emit(UiEvent::ClipboardText(text, source));
                        }
                    });
                }
                LRESULT(0)
            }
            WM_APPLY_HOTKEY => {
                apply_hotkey(hwnd);
                LRESULT(0)
            }
            WM_CLOSE => {
                tray_remove(hwnd);
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            m if m != 0 && m == MSG_SHOW.load(Ordering::Relaxed) => {
                emit(UiEvent::Show);
                LRESULT(0)
            }
            m if m != 0 && m == MSG_QUIT.load(Ordering::Relaxed) => {
                emit(UiEvent::Quit);
                LRESULT(0)
            }
            m if m != 0 && m == MSG_TASKBAR_CREATED.load(Ordering::Relaxed) => {
                // Explorer restarted: the tray icon is gone, add it again.
                tray_add(hwnd);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

fn tray_data(hwnd: HWND) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_ID,
        ..Default::default()
    }
}

unsafe fn tray_add(hwnd: HWND) {
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let icon = LoadImageW(
            Some(hinst.into()),
            windows::core::PCWSTR(1 as *const u16),
            IMAGE_ICON,
            GetSystemMetrics(SM_CXSMICON),
            GetSystemMetrics(SM_CYSMICON),
            LR_DEFAULTCOLOR,
        )
        .map(|h| HICON(h.0))
        .unwrap_or_default();
        let mut nid = tray_data(hwnd);
        nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
        nid.uCallbackMessage = WM_TRAY;
        nid.hIcon = icon;
        let tip: Vec<u16> = "Smowauncher".encode_utf16().collect();
        nid.szTip[..tip.len()].copy_from_slice(&tip);
        if !Shell_NotifyIconW(NIM_ADD, &nid).as_bool() {
            log::warn!("tray: NIM_ADD failed");
        }
    }
}

unsafe fn tray_remove(hwnd: HWND) {
    unsafe {
        let nid = tray_data(hwnd);
        let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
    }
}

unsafe fn tray_menu(hwnd: HWND) {
    const ID_OPEN: usize = 1;
    const ID_WINKEY: usize = 2;
    const ID_SETTINGS: usize = 3;
    const ID_REINDEX: usize = 4;
    const ID_QUIT: usize = 5;
    const ID_AUTOSTART: usize = 6;
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let add = |id: usize, text: &str, flags: MENU_ITEM_FLAGS| {
            let w = wide(text);
            let _ = AppendMenuW(menu, MF_STRING | flags, id, pcwstr(&w));
        };
        add(ID_OPEN, "Open Smowauncher", MF_ENABLED);
        let checked = if WIN_KEY_ENABLED.load(Ordering::Relaxed) { MF_CHECKED } else { MF_UNCHECKED };
        add(ID_WINKEY, "Open with Windows key", checked);
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        add(ID_SETTINGS, "Settings…", MF_ENABLED);
        add(ID_REINDEX, "Rebuild app index", MF_ENABLED);
        add(ID_AUTOSTART, "Start at sign-in (as admin)…", MF_ENABLED);
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        add(ID_QUIT, "Quit", MF_ENABLED);
        let _ = SetMenuDefaultItem(menu, ID_OPEN as u32, 0);

        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        // Required so the menu closes when clicking elsewhere.
        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY,
            pt.x,
            pt.y,
            None,
            hwnd,
            None,
        );
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);

        match cmd.0 as usize {
            ID_OPEN => emit(UiEvent::Show),
            ID_WINKEY => {
                let now = !WIN_KEY_ENABLED.load(Ordering::Relaxed);
                WIN_KEY_ENABLED.store(now, Ordering::Relaxed);
                log::info!("win key capture {}", if now { "enabled" } else { "disabled" });
            }
            ID_SETTINGS => emit(UiEvent::OpenSettings),
            ID_REINDEX => emit(UiEvent::Reindex),
            ID_AUTOSTART => emit(UiEvent::InstallAutostart),
            ID_QUIT => {
                tray_remove(hwnd);
                emit(UiEvent::Quit);
            }
            _ => {}
        }
    }
}
