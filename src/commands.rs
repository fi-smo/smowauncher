//! Built-in system commands and Windows Settings pages. They are injected into the app list
//! as `AppEntry`s, so they get fuzzy matching, frecency and the same UI for free.

use crate::apps::AppEntry;
use std::os::windows::process::CommandExt;

struct Command {
    id: &'static str,
    name: &'static str,
    keywords: &'static str,
    /// Segoe Fluent Icons glyph.
    glyph: &'static str,
    confirm: bool,
}

const COMMANDS: [Command; 10] = [
    Command { id: "settings", name: "Smowauncher Settings", keywords: "settings preferences options configure config smowauncher", glyph: "\u{E713}", confirm: false },
    // Open a view inside the launcher (see App::run_app_action).
    Command { id: "emoji", name: "Emoji Picker", keywords: "emoji emoticon smiley symbols", glyph: "\u{E76E}", confirm: false },
    Command { id: "clipboard", name: "Clipboard History", keywords: "clipboard history clip paste copied", glyph: "\u{E77F}", confirm: false },
    Command { id: "lock", name: "Lock", keywords: "lock screen computer pc", glyph: "\u{E72E}", confirm: false },
    Command { id: "sleep", name: "Sleep", keywords: "sleep suspend standby", glyph: "\u{E708}", confirm: false },
    Command { id: "hibernate", name: "Hibernate", keywords: "hibernate", glyph: "\u{E823}", confirm: false },
    Command { id: "shutdown", name: "Shut Down", keywords: "shutdown shut down power off turn off", glyph: "\u{E7E8}", confirm: true },
    Command { id: "restart", name: "Restart", keywords: "restart reboot", glyph: "\u{E777}", confirm: true },
    Command { id: "signout", name: "Sign Out", keywords: "sign out log off logout", glyph: "\u{F3B1}", confirm: true },
    Command { id: "emptybin", name: "Empty Recycle Bin", keywords: "empty recycle bin trash", glyph: "\u{E74D}", confirm: false },
];

/// (ms-settings page, title, extra keywords)
const SETTINGS: [(&str, &str, &str); 26] = [
    ("display", "Display Settings", "screen resolution monitor scale brightness hdr"),
    ("nightlight", "Night Light", "blue light warm"),
    ("sound", "Sound Settings", "audio volume speakers microphone output input"),
    ("bluetooth", "Bluetooth & Devices", "bluetooth devices pair headphones"),
    ("network-wifi", "Wi-Fi Settings", "wifi wireless network internet"),
    ("network-status", "Network & Internet", "network ethernet internet ip"),
    ("network-vpn", "VPN Settings", "vpn"),
    ("windowsupdate", "Windows Update", "update updates patch"),
    ("appsfeatures", "Installed Apps", "apps uninstall programs features"),
    ("defaultapps", "Default Apps", "default browser apps associations"),
    ("startupapps", "Startup Apps", "startup autostart boot"),
    ("optionalfeatures", "Optional Features", "optional features"),
    ("personalization-background", "Background", "wallpaper desktop background personalize"),
    ("colors", "Colors", "dark mode light mode theme accent color"),
    ("taskbar", "Taskbar Settings", "taskbar tray"),
    ("mousetouchpad", "Mouse Settings", "mouse pointer cursor speed"),
    ("devices-touchpad", "Touchpad Settings", "touchpad trackpad gestures"),
    ("typing", "Typing Settings", "keyboard typing autocorrect"),
    ("powersleep", "Power & Battery", "power battery sleep energy"),
    ("storagesense", "Storage Settings", "storage disk space cleanup"),
    ("privacy", "Privacy & Security", "privacy security permissions"),
    ("dateandtime", "Date & Time", "date time clock timezone"),
    ("regionlanguage", "Language & Region", "language region locale format"),
    ("notifications", "Notifications", "notifications focus do not disturb"),
    ("printers", "Printers & Scanners", "printer scanner print"),
    ("about", "About This PC", "about system specs pc name version"),
];

pub fn entries() -> Vec<AppEntry> {
    let commands = COMMANDS.iter().map(|c| AppEntry {
        id: format!("cmd:{}", c.id),
        name: c.name.to_owned(),
        launch: format!("cmd:{}", c.id),
        path: None,
        keywords: c.keywords.to_owned(),
        packaged: false,
        icon: None,
    });
    let settings = SETTINGS.iter().map(|(page, name, kw)| AppEntry {
        id: format!("ms-settings:{page}"),
        name: (*name).to_owned(),
        launch: format!("ms-settings:{page}"),
        path: None,
        keywords: format!("settings {kw}"),
        packaged: false,
        icon: None,
    });
    commands.chain(settings).collect()
}

pub fn is_command(launch: &str) -> bool {
    launch.starts_with("cmd:")
}

pub fn is_settings(launch: &str) -> bool {
    launch.starts_with("ms-settings:")
}

fn find(launch: &str) -> Option<&'static Command> {
    let id = launch.strip_prefix("cmd:")?;
    COMMANDS.iter().find(|c| c.id == id)
}

pub fn glyph(launch: &str) -> Option<&'static str> {
    if is_settings(launch) {
        return Some("\u{E713}");
    }
    find(launch).map(|c| c.glyph)
}

pub fn needs_confirm(launch: &str) -> bool {
    find(launch).is_some_and(|c| c.confirm)
}

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn shutdown_exe(args: &[&str]) {
    let _ = std::process::Command::new("shutdown.exe").args(args).creation_flags(CREATE_NO_WINDOW).spawn();
}

/// Suspend/hibernate need SeShutdownPrivilege enabled on our token.
fn suspend(hibernate: bool) {
    use windows::Win32::Foundation::{CloseHandle, HANDLE, LUID};
    use windows::Win32::Security::{
        AdjustTokenPrivileges, LUID_AND_ATTRIBUTES, LookupPrivilegeValueW, SE_PRIVILEGE_ENABLED, SE_SHUTDOWN_NAME,
        TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
    };
    use windows::Win32::System::Power::SetSuspendState;
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    std::thread::spawn(move || unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY, &mut token).is_ok() {
            let mut luid = LUID::default();
            if LookupPrivilegeValueW(None, SE_SHUTDOWN_NAME, &mut luid).is_ok() {
                let tp = TOKEN_PRIVILEGES {
                    PrivilegeCount: 1,
                    Privileges: [LUID_AND_ATTRIBUTES { Luid: luid, Attributes: SE_PRIVILEGE_ENABLED }],
                };
                let _ = AdjustTokenPrivileges(token, false, Some(&tp), 0, None, None);
            }
            let _ = CloseHandle(token);
        }
        // Give the launcher a moment to hide before the machine goes down.
        std::thread::sleep(std::time::Duration::from_millis(300));
        if !SetSuspendState(hibernate, false, false) {
            log::error!("suspend failed: {}", windows::core::Error::from_thread());
        }
    });
}

pub fn run(launch: &str) {
    let Some(cmd) = find(launch) else { return };
    log::info!("command: {}", cmd.id);
    match cmd.id {
        "lock" => unsafe {
            let _ = windows::Win32::System::Shutdown::LockWorkStation();
        },
        "sleep" => suspend(false),
        "hibernate" => suspend(true),
        "shutdown" => shutdown_exe(&["/s", "/t", "0"]),
        "restart" => shutdown_exe(&["/r", "/t", "0"]),
        "signout" => shutdown_exe(&["/l"]),
        "emptybin" => {
            // Shows the system's own confirmation dialog; keep it off the UI thread.
            std::thread::spawn(|| unsafe {
                let _ = windows::Win32::UI::Shell::SHEmptyRecycleBinW(None, None, 0);
            });
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_are_well_formed() {
        let e = entries();
        assert_eq!(e.len(), COMMANDS.len() + SETTINGS.len());
        assert!(e.iter().all(|a| is_command(&a.launch) || is_settings(&a.launch)));
        assert!(needs_confirm("cmd:shutdown"));
        assert!(!needs_confirm("cmd:lock"));
        assert_eq!(glyph("ms-settings:display"), Some("\u{E713}"));
    }
}
