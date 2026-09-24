//! `config.toml` schema, defaults and hot reload.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const DEFAULT_CONFIG: &str = r#"# Smowauncher settings. Changes are applied automatically when you save this file
# (except [appearance].renderer, which needs a restart).

[general]
# Tap the Windows key (alone) to open Smowauncher instead of the Start menu.
win_key = true
# Secondary hotkey. Examples: "Alt+Space", "Ctrl+Shift+Space", "Win+Alt+S". Empty = disabled.
hotkey = "Alt+Space"
# Hide the launcher when it loses focus.
hide_on_blur = true
# Don't capture the Win key while a fullscreen game / presentation is running.
fullscreen_passthrough = true
max_results = 30

[appearance]
# "acrylic" (translucent blur) or "solid".
backdrop = "acrylic"
# "software" (CPU, ~10 MB RAM) or "femtovg" (OpenGL; GPU drivers add 50-150 MB RAM). Restart required.
renderer = "software"
# Release memory back to Windows shortly after the launcher is hidden.
trim_memory_on_hide = true

[apps]
# Extra folders to scan for .exe / .lnk files (scanned 3 levels deep).
extra_folders = []
# Hide apps whose name contains any of these (case-insensitive).
exclude = ["uninstall", "uninstaller"]
"#;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    pub general: General,
    pub appearance: Appearance,
    pub apps: Apps,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct General {
    pub win_key: bool,
    pub hotkey: String,
    pub hide_on_blur: bool,
    pub fullscreen_passthrough: bool,
    pub max_results: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Appearance {
    pub backdrop: String,
    pub renderer: String,
    pub trim_memory_on_hide: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Apps {
    pub extra_folders: Vec<String>,
    pub exclude: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self { general: General::default(), appearance: Appearance::default(), apps: Apps::default() }
    }
}

impl Default for Apps {
    fn default() -> Self {
        Self { extra_folders: Vec::new(), exclude: vec!["uninstall".into(), "uninstaller".into()] }
    }
}

impl Default for General {
    fn default() -> Self {
        Self {
            win_key: true,
            hotkey: "Alt+Space".into(),
            hide_on_blur: true,
            fullscreen_passthrough: true,
            max_results: 30,
        }
    }
}

impl Default for Appearance {
    fn default() -> Self {
        Self { backdrop: "acrylic".into(), renderer: "software".into(), trim_memory_on_hide: true }
    }
}

impl Config {
    pub fn acrylic(&self) -> bool {
        self.appearance.backdrop.eq_ignore_ascii_case("acrylic")
    }

    pub fn software_renderer(&self) -> bool {
        self.appearance.renderer.eq_ignore_ascii_case("software")
    }
}

/// Loads the config, writing the commented default file on first run.
pub fn load() -> Config {
    let path = crate::paths::config_file();
    if !path.exists() {
        let _ = std::fs::write(&path, DEFAULT_CONFIG);
        return Config::default();
    }
    match read(&path) {
        Ok(cfg) => cfg,
        Err(e) => {
            log::error!("config: {e} — using defaults");
            Config::default()
        }
    }
}

fn read(path: &PathBuf) -> Result<Config, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    toml::from_str(&text).map_err(|e| e.to_string())
}

/// Watches the config folder and calls `on_change` with each successfully parsed new config.
pub fn watch(on_change: impl Fn(Config) + Send + 'static) {
    use windows::Win32::Foundation::WAIT_OBJECT_0;
    use windows::Win32::Storage::FileSystem::{
        FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE, FindFirstChangeNotificationW,
        FindNextChangeNotification,
    };
    use windows::Win32::System::Threading::{INFINITE, WaitForSingleObject};
    use windows::core::HSTRING;

    std::thread::Builder::new()
        .name("config-watch".into())
        .stack_size(64 * 1024)
        .spawn(move || {
            let path = crate::paths::config_file();
            let dir = HSTRING::from(crate::paths::config_dir().as_os_str());
            let handle = unsafe {
                FindFirstChangeNotificationW(
                    &dir,
                    false,
                    FILE_NOTIFY_CHANGE_LAST_WRITE | FILE_NOTIFY_CHANGE_FILE_NAME,
                )
            };
            let Ok(handle) = handle else {
                log::warn!("config: cannot watch folder");
                return;
            };
            let mtime = |p: &PathBuf| std::fs::metadata(p).and_then(|m| m.modified()).ok();
            let mut last = mtime(&path);
            loop {
                if unsafe { WaitForSingleObject(handle, INFINITE) } != WAIT_OBJECT_0 {
                    return;
                }
                // Editors often write in several steps; let them finish.
                std::thread::sleep(std::time::Duration::from_millis(150));
                let now = mtime(&path);
                if now != last {
                    last = now;
                    match read(&path) {
                        Ok(cfg) => {
                            log::info!("config: reloaded");
                            on_change(cfg);
                        }
                        Err(e) => log::error!("config: {e} — keeping previous settings"),
                    }
                }
                if unsafe { FindNextChangeNotification(handle) }.is_err() {
                    return;
                }
            }
        })
        .expect("spawn config watcher");
}

/// Parses "Ctrl+Alt+Space" style hotkeys into (MOD_* flags, virtual key).
pub fn parse_hotkey(s: &str) -> Option<(u32, u32)> {
    const MOD_ALT: u32 = 0x1;
    const MOD_CONTROL: u32 = 0x2;
    const MOD_SHIFT: u32 = 0x4;
    const MOD_WIN: u32 = 0x8;
    let mut mods = 0;
    let mut vk = None;
    for part in s.split('+').map(|p| p.trim().to_ascii_lowercase()) {
        match part.as_str() {
            "" => return None,
            "ctrl" | "control" => mods |= MOD_CONTROL,
            "alt" => mods |= MOD_ALT,
            "shift" => mods |= MOD_SHIFT,
            "win" | "super" | "meta" => mods |= MOD_WIN,
            key => {
                vk = Some(match key {
                    "space" => 0x20,
                    "enter" | "return" => 0x0D,
                    "tab" => 0x09,
                    "esc" | "escape" => 0x1B,
                    "`" | "backtick" | "grave" => 0xC0,
                    k if k.len() == 1 && k.as_bytes()[0].is_ascii_alphanumeric() => {
                        k.as_bytes()[0].to_ascii_uppercase() as u32
                    }
                    k if k.starts_with('f') => {
                        let n: u32 = k[1..].parse().ok()?;
                        if !(1..=24).contains(&n) {
                            return None;
                        }
                        0x70 + n - 1
                    }
                    _ => return None,
                });
            }
        }
    }
    vk.map(|vk| (mods, vk))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_matches_defaults() {
        let parsed: Config = toml::from_str(DEFAULT_CONFIG).unwrap();
        assert_eq!(parsed, Config::default());
        let partial: Config = toml::from_str("[general]
win_key = false").unwrap();
        assert!(!partial.general.win_key);
        assert_eq!(partial.general.hotkey, "Alt+Space");
    }

    #[test]
    fn hotkeys() {
        assert_eq!(parse_hotkey("Alt+Space"), Some((0x1, 0x20)));
        assert_eq!(parse_hotkey("ctrl+shift+k"), Some((0x6, b'K' as u32)));
        assert_eq!(parse_hotkey("Win+F12"), Some((0x8, 0x7B)));
        assert_eq!(parse_hotkey("Alt+"), None);
        assert_eq!(parse_hotkey("Alt+Nope"), None);
    }
}
