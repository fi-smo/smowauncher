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
/// Open on a double tap of Win; a single tap is replayed to Windows (Start).
pub static WIN_DOUBLE_TAP: AtomicBool = AtomicBool::new(false);
/// Time (GetTickCount) of a lone Win tap waiting for a second one; 0 = none.
static FIRST_TAP: AtomicU32 = AtomicU32::new(0);
const DOUBLE_TAP_MS: u32 = 300;
const DOUBLE_TAP_TIMER: usize = 2;

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
static MSG_SETTINGS: AtomicU32 = AtomicU32::new(0);
static MSG_TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);
static SINK: OnceLock<Box<dyn Fn(UiEvent) + Send + Sync>> = OnceLock::new();

/// Recent keyboard-hook events (diagnostics), read by `trace_dump` on the UI thread. The hook
/// only `try_lock`s, so it never waits. Character keys are recorded as "char", never their value.
struct TraceEvent {
    /// Event time (GetTickCount ms).
    time: u32,
    /// How late the hook was called (ms) — large values mean the hook thread was busy.
    delay: u32,
    vk: u16,
    down: bool,
    injected: bool,
    action: &'static str,
}
static TRACE: std::sync::Mutex<std::collections::VecDeque<TraceEvent>> = std::sync::Mutex::new(std::collections::VecDeque::new());
const T_PASS: &str = "pass";
const T_PASS_OFF: &str = "pass (win key off)";
const T_PASS_MOD: &str = "pass (modifier held)";
const T_PASS_FS: &str = "pass (fullscreen)";
const T_SWALLOW: &str = "swallow";
const T_REPLAY: &str = "replay";
const T_TOGGLE: &str = "toggle";
const T_WAIT: &str = "first tap";
const T_OTHER: &str = "-";
/// Set by the UI while the launcher is visible: then every key is traced, not only Win.
pub static LAUNCHER_VISIBLE: AtomicBool = AtomicBool::new(false);

fn is_char_key(vk: u32) -> bool {
    matches!(vk, 0x20 | 0x30..=0x39 | 0x41..=0x5A | 0x60..=0x6F | 0xBA..=0xC0 | 0xDB..=0xDF | 0xE2)
}

fn trace(kb: &KBDLLHOOKSTRUCT, down: bool, action: &'static str) {
    let Ok(mut t) = TRACE.try_lock() else { return };
    if t.len() >= 48 {
        t.pop_front();
    }
    let now = unsafe { windows::Win32::System::SystemInformation::GetTickCount() };
    t.push_back(TraceEvent {
        time: kb.time,
        delay: now.wrapping_sub(kb.time),
        vk: if is_char_key(kb.vkCode) { 0 } else { kb.vkCode as u16 },
        down,
        injected: kb.flags.0 & LLKHF_INJECTED.0 != 0,
        action,
    });
}

/// Logs the recent hook events (oldest first) and clears the trace.
pub fn trace_dump(why: &str) {
    let events: Vec<TraceEvent> = TRACE.lock().map(|mut t| t.drain(..).collect()).unwrap_or_default();
    if events.is_empty() {
        return;
    }
    let lines: Vec<String> = events
        .iter()
        .map(|e| {
            let key = if e.vk == 0 { "char".to_string() } else { format!("vk={:#04x}", e.vk) };
            format!(
                "t={} +{}ms {key} {}{} {}",
                e.time,
                e.delay,
                if e.down { "down" } else { "up" },
                if e.injected { " injected" } else { "" },
                e.action
            )
        })
        .collect();
    log::info!("keys before {why}: {}", lines.join(" | "));
}

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

pub fn settings_message() -> u32 {
    registered_message("Smowauncher.Settings")
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
        MSG_SETTINGS.store(settings_message(), Ordering::Relaxed);
        MSG_TASKBAR_CREATED.store(registered_message("TaskbarCreated"), Ordering::Relaxed);
        // We usually run elevated; let non-elevated processes (second instance, Explorer) reach us.
        for msg in [&MSG_SHOW, &MSG_QUIT, &MSG_SETTINGS, &MSG_TASKBAR_CREATED] {
            let _ = ChangeWindowMessageFilterEx(hwnd, msg.load(Ordering::Relaxed), MSGFLT_ALLOW, None);
        }

        tray_add(hwnd);

        watch_fullscreen();
        install_hook();
        SetTimer(Some(hwnd), HOOK_REFRESH_TIMER, HOOK_REFRESH_MS, None);
        let _ = windows::Win32::System::RemoteDesktop::WTSRegisterSessionNotification(
            hwnd,
            windows::Win32::System::RemoteDesktop::NOTIFY_FOR_THIS_SESSION,
        );
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

        let h = HOOK.swap(0, Ordering::AcqRel);
        if h != 0 {
            let _ = UnhookWindowsHookEx(HHOOK(h as *mut _));
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

/// Exclusive-mode fullscreen (D3D), presentation mode, or a borderless window covering its
/// whole monitor (how most games run today). Our own window and the desktop don't count.
/// Cached "a fullscreen app is in front" (see `watch_fullscreen`). The hook must not call
/// the shell itself: a slow hook gets skipped (the key reaches Windows) or removed.
static FULLSCREEN_NOW: AtomicBool = AtomicBool::new(false);
static FULLSCREEN_WAKE: (std::sync::Mutex<bool>, std::sync::Condvar) = (std::sync::Mutex::new(false), std::sync::Condvar::new());

/// Re-evaluates the fullscreen state when the foreground window changes (signalled by the
/// WinEvent hook) and every 10 s (a game can go fullscreen without changing windows).
fn watch_fullscreen() {
    let _ = std::thread::Builder::new().name("fullscreen-watch".into()).stack_size(128 * 1024).spawn(|| loop {
        let (now, why) = fullscreen_app_running();
        if FULLSCREEN_NOW.swap(now, Ordering::Relaxed) != now {
            log::info!("fullscreen: {now} ({why})");
        }
        let (lock, cvar) = &FULLSCREEN_WAKE;
        let guard = lock.lock().unwrap();
        let (mut guard, _) = cvar.wait_timeout_while(guard, std::time::Duration::from_secs(10), |woken| !*woken).unwrap();
        *guard = false;
    });
}

/// Returns the state and what decided it (for the log).
fn fullscreen_app_running() -> (bool, String) {
    if let Ok(state) = unsafe { SHQueryUserNotificationState() }
        && (state == QUNS_RUNNING_D3D_FULL_SCREEN || state == QUNS_PRESENTATION_MODE)
    {
        return (true, format!("notification state {}", state.0));
    }
    unsafe {
        use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONULL, MONITORINFO, MonitorFromWindow};
        let fg = GetForegroundWindow();
        if fg.is_invalid() {
            return (false, "no foreground window".into());
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(fg, Some(&mut pid));
        if pid == std::process::id() {
            return (false, "Smowauncher".into());
        }
        let mut class = [0u16; 32];
        let n = GetClassNameW(fg, &mut class).max(0) as usize;
        let class = String::from_utf16_lossy(&class[..n]);
        if matches!(class.as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd") || IsZoomed(fg).as_bool() {
            // A maximized window isn't fullscreen, even where it covers the monitor.
            return (false, class);
        }
        let mon = MonitorFromWindow(fg, MONITOR_DEFAULTTONULL);
        let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        let mut r = windows::Win32::Foundation::RECT::default();
        if mon.is_invalid() || !GetMonitorInfoW(mon, &mut mi).as_bool() || GetWindowRect(fg, &mut r).is_err() {
            return (false, class);
        }
        let m = mi.rcMonitor;
        (r.left <= m.left && r.top <= m.top && r.right >= m.right && r.bottom >= m.bottom, format!("{class} covering its monitor"))
    }
}

static HOOK: AtomicIsize = AtomicIsize::new(0);
const HOOK_REFRESH_TIMER: usize = 1;
const HOOK_REFRESH_MS: u32 = 10 * 60 * 1000;

/// (Re)installs the keyboard hook. Windows silently removes low-level hooks that were slow
/// even once (e.g. while the machine was swapping) and sometimes after sleep; re-installing
/// periodically and on unlock/resume keeps the Win key working. Also resets the Win state,
/// which can be left dangling when a key-up happened on the secure desktop (Win+L).
unsafe fn install_hook() {
    unsafe {
        let old = HOOK.swap(0, Ordering::AcqRel);
        if old != 0 {
            let _ = UnhookWindowsHookEx(HHOOK(old as *mut _));
        }
        WIN_DOWN.store(false, Ordering::Relaxed);
        WIN_PENDING.store(false, Ordering::Relaxed);
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        match SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), Some(hinst.into()), 0) {
            Ok(h) => HOOK.store(h.0 as isize, Ordering::Release),
            Err(e) => log::error!("input: keyboard hook failed: {e}"),
        }
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

/// Double-tap mode: no second tap came, so the first one was meant for Windows (Start).
/// 	hen is a key that arrived meanwhile; it's replayed after the tap to keep the order.
fn replay_win_tap(then: Option<(&KBDLLHOOKSTRUCT, bool)>) {
    let vk = WIN_VK.load(Ordering::Relaxed) as u16;
    let mut inputs = vec![key_input(vk, 0, KEYEVENTF_EXTENDEDKEY), key_input(vk, 0, KEYEVENTF_EXTENDEDKEY | KEYEVENTF_KEYUP)];
    if let Some((kb, key_up)) = then {
        let mut flags = if kb.flags.0 & LLKHF_EXTENDED.0 != 0 { KEYEVENTF_EXTENDEDKEY } else { KEYBD_EVENT_FLAGS(0) };
        if key_up {
            flags |= KEYEVENTF_KEYUP;
        }
        inputs.push(key_input(kb.vkCode as u16, kb.scanCode as u16, flags));
    }
    unsafe {
        SendInput(&inputs, size_of::<INPUT>() as i32);
    }
}

fn cancel_first_tap() -> bool {
    let pending = FIRST_TAP.swap(0, Ordering::Relaxed) != 0;
    if pending {
        let h = MSG_HWND.load(Ordering::Acquire);
        unsafe {
            let _ = KillTimer(Some(HWND(h as *mut _)), DOUBLE_TAP_TIMER);
        }
    }
    pending
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
                        trace(kb, down, T_PASS);
                    } else {
                        let why = if !WIN_KEY_ENABLED.load(Ordering::Relaxed) {
                            Some(T_PASS_OFF)
                        } else if key_held(VK_CONTROL) || key_held(VK_MENU) || key_held(VK_SHIFT) {
                            Some(T_PASS_MOD)
                        } else if FULLSCREEN_PASSTHROUGH.load(Ordering::Relaxed) && FULLSCREEN_NOW.load(Ordering::Relaxed) {
                            Some(T_PASS_FS)
                        } else {
                            None
                        };
                        WIN_PENDING.store(why.is_none(), Ordering::Relaxed);
                        let Some(why) = why else {
                            WIN_VK.store(vk as u32, Ordering::Relaxed);
                            trace(kb, down, T_SWALLOW);
                            return LRESULT(1);
                        };
                        trace(kb, down, why);
                    }
                } else {
                    WIN_DOWN.store(false, Ordering::Relaxed);
                    if WIN_PENDING.swap(false, Ordering::Relaxed) {
                        // Released alone: Windows never saw this Win press.
                        if WIN_DOUBLE_TAP.load(Ordering::Relaxed) && !LAUNCHER_VISIBLE.load(Ordering::Relaxed) && !cancel_first_tap() {
                            // First tap: wait for a second one (see DOUBLE_TAP_TIMER in wndproc).
                            FIRST_TAP.store(kb.time.max(1), Ordering::Relaxed);
                            let h = MSG_HWND.load(Ordering::Acquire);
                            unsafe {
                                SetTimer(Some(HWND(h as *mut _)), DOUBLE_TAP_TIMER, DOUBLE_TAP_MS, None);
                            }
                            trace(kb, down, T_WAIT);
                            return LRESULT(1);
                        }
                        send_dummy_key();
                        trace(kb, down, T_TOGGLE);
                        emit(UiEvent::Toggle);
                        return LRESULT(1);
                    }
                    trace(kb, down, T_PASS);
                }
            } else if down && !WIN_DOWN.load(Ordering::Relaxed) && cancel_first_tap() {
                // Typing right after a single tap (double-tap mode): Start, then this key.
                replay_win_tap(Some((kb, false)));
                trace(kb, down, T_REPLAY);
                return LRESULT(1);
            } else if WIN_DOWN.load(Ordering::Relaxed) && WIN_PENDING.swap(false, Ordering::Relaxed) {
                // First other key while Win is held back: it's a shortcut, give it to Windows.
                // Key-ups (e.g. a modifier pressed before Win) are replayed the same way.
                // A pending first tap (double-tap mode) was just the start of this shortcut.
                cancel_first_tap();
                replay_combo(WIN_VK.load(Ordering::Relaxed) as u16, kb, !down);
                trace(kb, down, T_REPLAY);
                return LRESULT(1);
            } else if LAUNCHER_VISIBLE.load(Ordering::Relaxed) {
                trace(kb, down, T_OTHER);
            }
        } else if LAUNCHER_VISIBLE.load(Ordering::Relaxed) {
            trace(kb, wparam.0 as u32 == WM_KEYDOWN || wparam.0 as u32 == WM_SYSKEYDOWN, "ours");
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
    let (lock, cvar) = &FULLSCREEN_WAKE;
    if let Ok(mut woken) = lock.try_lock() {
        *woken = true;
        cvar.notify_one();
    }
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
            WM_TIMER if wparam.0 == DOUBLE_TAP_TIMER => {
                // No second tap: the single tap was for Windows. If Win is down again, it's a
                // slow second press; its release starts over as a first tap.
                if cancel_first_tap() && !WIN_DOWN.load(Ordering::Relaxed) {
                    replay_win_tap(None);
                }
                LRESULT(0)
            }
            WM_TIMER if wparam.0 == HOOK_REFRESH_TIMER => {
                install_hook();
                LRESULT(0)
            }
            // Unlock / resume from sleep: the hook may have been dropped, and key-ups may
            // have gone to the secure desktop.
            WM_WTSSESSION_CHANGE if wparam.0 as u32 == WTS_SESSION_UNLOCK => {
                log::info!("input: session unlocked, refreshing keyboard hook");
                install_hook();
                LRESULT(0)
            }
            WM_POWERBROADCAST if wparam.0 as u32 == PBT_APMRESUMEAUTOMATIC => {
                log::info!("input: resumed, refreshing keyboard hook");
                install_hook();
                LRESULT(1)
            }
            WM_SETTINGCHANGE => {
                // Broadcast with lParam = "ImmersiveColorSet" when light/dark mode changes.
                if lparam.0 != 0
                    && windows::core::PCWSTR(lparam.0 as *const u16).to_string().is_ok_and(|s| s == "ImmersiveColorSet")
                {
                    emit(UiEvent::ThemeChanged);
                }
                DefWindowProcW(hwnd, msg, wparam, lparam)
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
            m if m != 0 && m == MSG_SETTINGS.load(Ordering::Relaxed) => {
                emit(UiEvent::OpenSettings);
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
    const ID_UPDATES: usize = 7;
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
        add(ID_UPDATES, "Check for updates", MF_ENABLED);
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
            ID_UPDATES => emit(UiEvent::CheckUpdates),
            ID_QUIT => {
                tray_remove(hwnd);
                emit(UiEvent::Quit);
            }
            _ => {}
        }
    }
}
