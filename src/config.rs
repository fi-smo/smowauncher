//! `config.toml` schema, defaults and hot reload.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const DEFAULT_CONFIG: &str = r#"# Smowauncher settings. Changes are applied automatically when you save this file
# (except [appearance].renderer, which needs a restart).

[general]
# Tap the Windows key (alone) to open Smowauncher instead of the Start menu.
win_key = true
# Open with a quick double tap of Win instead; a single tap then opens Start as usual.
win_double_tap = false
# Secondary hotkey. Examples: "Alt+Space", "Ctrl+Shift+Space", "Win+Alt+S". Empty = disabled.
hotkey = "Alt+Space"
# Hide the launcher when it loses focus.
hide_on_blur = true
# Don't capture the Win key while a fullscreen game / presentation is running.
fullscreen_passthrough = true
max_results = 30

[appearance]
# "system" (follow Windows' app mode), "dark" or "light".
theme = "system"
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

[shortcuts]
# Typing an alias exactly puts its app first, e.g. { ff = "Firefox" }. Values are app names.
# Ctrl+K on an app also adds or removes aliases and pins.
aliases = {}
# Apps listed first when the search box is empty.
pinned = []

[files]
# File & folder search through Everything (voidtools.com). Type "f " or "/" to search files only.
enabled = true
# Without Everything, search the Windows Search index instead (indexed folders only).
windows_search = true
# Show files under the apps once the query has at least this many characters.
min_chars = 3
# How many files to show under the apps / in files-only mode.
max_mixed = 8
max_files_only = 30
# Show a preview panel for the selected file (thumbnail or text), clipboard entry or snippet.
preview = true
# Locations left out of results (Everything path terms).
exclude = ['C:\Windows\', '\$Recycle.Bin\', '\node_modules\', '\.git\', '\AppData\Local\Temp\', '\AppData\Local\Microsoft\', '\AppData\Local\Packages\', '\WindowsApps\']

[calc]
# Currency that "100 usd" is converted to. Empty = the currency of your Windows region.
default_currency = ""

[clipboard]
# Remember copied text. Open the history with the hotkey or by typing "clip".
enabled = true
# Remember copied images (screenshots…) too, as PNG files next to the history.
images = true
hotkey = "Ctrl+Alt+V"
max_items = 200
# Never record text copied from these apps (process names, case-insensitive).
# Password managers that mark secrets as private are skipped automatically.
ignore_apps = ["KeePass", "KeePassXC", "1Password", "Bitwarden", "Dashlane", "LastPass"]

[snippets]
# Saved texts. Search them in the launcher by name or keyword ("snip" lists them all), or turn
# on expand_anywhere to replace a keyword with its text as you type it in any app.
# Placeholders: {date}, {time}, {clipboard}. Example:
# items = [{ keyword = ";sig", name = "Signature", text = "Best regards,\nJane" }]
expand_anywhere = false
items = []

[ai]
# Ask Claude from the launcher: "ask <question>" or "? <question>" (Enter asks, type again for a
# follow-up). AI commands below work on the copied text. Needs an Anthropic API key: Settings → AI
# stores it in Windows Credential Manager (or set the ANTHROPIC_API_KEY environment variable).
enabled = true
# "claude-opus-5", "claude-sonnet-5" or "claude-haiku-4-5".
model = "claude-opus-5"
# How hard the model thinks: "low", "medium" or "high" (slower and more expensive).
effort = "medium"
commands = [
    { name = "Fix spelling and grammar", prompt = "Fix the spelling and grammar of the text below. Keep its language, meaning, tone and formatting. Reply with only the corrected text." },
    { name = "Summarize", prompt = "Summarize the text below in a few short bullet points, in the text's own language." },
    { name = "Translate to English", prompt = "Translate the text below to English. Reply with only the translation." },
    { name = "Explain", prompt = "Explain the text below simply and briefly." },
]

[web]
# Engine used for "Search ... for" when nothing else matches (a keyword below).
fallback = "g"
# Type "<keyword> <query>" to search with an engine, e.g. "yt lofi beats".
engines = [
    { keyword = "g", name = "Google", url = "https://www.google.com/search?q={q}" },
    { keyword = "ddg", name = "DuckDuckGo", url = "https://duckduckgo.com/?q={q}" },
    { keyword = "yt", name = "YouTube", url = "https://www.youtube.com/results?search_query={q}" },
    { keyword = "gh", name = "GitHub", url = "https://github.com/search?q={q}" },
    { keyword = "w", name = "Wikipedia", url = "https://en.wikipedia.org/w/index.php?search={q}" },
    { keyword = "maps", name = "Google Maps", url = "https://www.google.com/maps/search/{q}" },
    { keyword = "tr", name = "Google Translate", url = "https://translate.google.com/?sl=auto&tl=en&text={q}" },
]

[updates]
# Check GitHub for new releases (at startup and every 12 hours) and install them while the
# launcher is hidden. The tray menu also has "Check for updates".
enabled = true
"#;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    pub general: General,
    pub appearance: Appearance,
    pub apps: Apps,
    pub shortcuts: Shortcuts,
    pub files: Files,
    pub calc: Calc,
    pub clipboard: Clipboard,
    pub snippets: Snippets,
    pub ai: crate::ai::Config,
    pub web: Web,
    pub updates: Updates,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct Snippets {
    /// Expand keywords typed in any app (the keyboard hook keeps the last few characters).
    pub expand_anywhere: bool,
    pub items: Vec<crate::snippets::Snippet>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct General {
    pub win_key: bool,
    pub win_double_tap: bool,
    pub hotkey: String,
    pub hide_on_blur: bool,
    pub fullscreen_passthrough: bool,
    pub max_results: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Appearance {
    pub theme: String,
    pub backdrop: String,
    pub renderer: String,
    pub trim_memory_on_hide: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Clipboard {
    pub enabled: bool,
    pub images: bool,
    pub hotkey: String,
    pub max_items: usize,
    pub ignore_apps: Vec<String>,
}

impl Default for Clipboard {
    fn default() -> Self {
        Self {
            enabled: true,
            images: true,
            hotkey: "Ctrl+Alt+V".into(),
            max_items: 200,
            ignore_apps: ["KeePass", "KeePassXC", "1Password", "Bitwarden", "Dashlane", "LastPass"].map(String::from).to_vec(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Web {
    pub fallback: String,
    pub engines: Vec<crate::web::Engine>,
}

impl Default for Web {
    fn default() -> Self {
        Self { fallback: "g".into(), engines: crate::web::default_engines() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Updates {
    /// Check GitHub Releases and install new versions automatically (installed copy only).
    pub enabled: bool,
}

impl Default for Updates {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct Calc {
    /// ISO code a bare "100 usd" converts to; empty = the Windows region's currency.
    pub default_currency: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Files {
    pub enabled: bool,
    pub windows_search: bool,
    pub min_chars: usize,
    pub max_mixed: usize,
    pub max_files_only: usize,
    pub preview: bool,
    pub exclude: Vec<String>,
}

impl Default for Files {
    fn default() -> Self {
        Self {
            enabled: true,
            windows_search: true,
            min_chars: 3,
            max_mixed: 8,
            max_files_only: 30,
            preview: true,
            exclude: [
                r"C:\Windows\",
                r"\$Recycle.Bin\",
                r"\node_modules\",
                r"\.git\",
                r"\AppData\Local\Temp\",
                r"\AppData\Local\Microsoft\",
                r"\AppData\Local\Packages\",
                r"\WindowsApps\",
            ]
            .map(String::from)
            .to_vec(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct Shortcuts {
    /// alias (lowercase) -> app name or id
    pub aliases: std::collections::BTreeMap<String, String>,
    /// App names or ids, in order.
    pub pinned: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Apps {
    pub extra_folders: Vec<String>,
    pub exclude: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            general: General::default(),
            appearance: Appearance::default(),
            apps: Apps::default(),
            shortcuts: Shortcuts::default(),
            files: Files::default(),
            calc: Calc::default(),
            clipboard: Clipboard::default(),
            snippets: Snippets::default(),
            ai: crate::ai::Config::default(),
            web: Web::default(),
            updates: Updates::default(),
        }
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
            win_double_tap: false,
            hotkey: "Alt+Space".into(),
            hide_on_blur: true,
            fullscreen_passthrough: true,
            max_results: 30,
        }
    }
}

impl Default for Appearance {
    fn default() -> Self {
        Self { theme: "system".into(), backdrop: "acrylic".into(), renderer: "software".into(), trim_memory_on_hide: true }
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
    append_missing_sections(&path);
    match read(&path) {
        Ok(cfg) => cfg,
        Err(e) => {
            log::error!("config: {e} — using defaults");
            Config::default()
        }
    }
}

/// Writes `cfg` to config.toml, editing values in place so the user's comments and layout
/// survive (the settings window saves through this). The watcher then applies it.
pub fn save(cfg: &Config) -> Result<(), String> {
    let path = crate::paths::config_file();
    let fresh: toml_edit::DocumentMut =
        toml::to_string(cfg).map_err(|e| e.to_string())?.parse().map_err(|e: toml_edit::TomlError| e.to_string())?;
    let text = std::fs::read_to_string(&path).unwrap_or_else(|_| DEFAULT_CONFIG.to_owned());
    let mut doc: toml_edit::DocumentMut = text.parse().unwrap_or_else(|_| DEFAULT_CONFIG.parse().expect("default config parses"));
    merge_table(doc.as_table_mut(), fresh.as_table());
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, doc.to_string()).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
}

/// Copies values from `src` into `dst`, keeping `dst`'s comments/whitespace around each key.
fn merge_table(dst: &mut toml_edit::Table, src: &toml_edit::Table) {
    use toml_edit::Item;
    for (key, item) in src.iter() {
        // Keep arrays of tables (web engines) as the inline arrays the default config uses.
        let item = match item {
            Item::ArrayOfTables(aot) => {
                let mut array = aot.clone().into_array();
                for value in array.iter_mut() {
                    value.decor_mut().set_prefix("\n    ");
                }
                array.set_trailing("\n");
                array.set_trailing_comma(true);
                Item::Value(toml_edit::Value::Array(array))
            }
            other => other.clone(),
        };
        match (dst.get_mut(key), &item) {
            (Some(Item::Table(d)), Item::Table(s)) => merge_table(d, s),
            // Small maps (aliases) stay the inline tables the default config uses.
            (Some(Item::Value(toml_edit::Value::InlineTable(d))), Item::Table(s)) => {
                let decor = d.decor().clone();
                *d = s.clone().into_inline_table();
                *d.decor_mut() = decor;
            }
            (Some(existing), Item::Value(new)) => match existing.as_value_mut() {
                Some(old) => {
                    let decor = old.decor().clone();
                    *old = new.clone();
                    *old.decor_mut() = decor;
                }
                None => *existing = item.clone(),
            },
            (Some(existing), _) => *existing = item.clone(),
            (None, _) => {
                dst.insert(key, item.clone());
            }
        }
    }
}

/// Top-level `[section]` blocks of the default config, with their comments.
fn default_sections() -> Vec<(&'static str, &'static str)> {
    let mut out = Vec::new();
    let mut starts: Vec<usize> = DEFAULT_CONFIG.match_indices("\n[").map(|(i, _)| i + 1).collect();
    starts.push(DEFAULT_CONFIG.len());
    for w in starts.windows(2) {
        let block = &DEFAULT_CONFIG[w[0]..w[1]];
        let name = block[1..].split(']').next().unwrap_or_default();
        out.push((name, block.trim_end()));
    }
    out
}

/// Settings added in newer versions show up (documented) in an existing config file.
fn append_missing_sections(path: &PathBuf) {
    let Ok(text) = std::fs::read_to_string(path) else { return };
    let missing: Vec<&str> = default_sections()
        .into_iter()
        .filter(|(name, _)| !text.lines().any(|l| l.trim() == format!("[{name}]")))
        .map(|(_, block)| block)
        .collect();
    if missing.is_empty() {
        return;
    }
    let mut new_text = text.trim_end().to_owned();
    for block in missing {
        new_text.push_str("\n\n");
        new_text.push_str(block);
    }
    new_text.push('\n');
    let _ = std::fs::write(path, new_text);
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
    fn merge_keeps_comments_and_updates_values() {
        let mut doc: toml_edit::DocumentMut = DEFAULT_CONFIG.parse().unwrap();
        let mut cfg = Config::default();
        cfg.general.win_key = false;
        cfg.general.hotkey = "Ctrl+Space".into();
        cfg.files.exclude.push(r"D:\Junk\".into());
        cfg.web.engines.pop();
        cfg.shortcuts.aliases.insert("ff".into(), "Firefox".into());
        cfg.shortcuts.pinned.push("Visual Studio Code".into());
        cfg.snippets.items.push(crate::snippets::Snippet {
            keyword: ";sig".into(),
            name: "Signature".into(),
            text: "Best regards,\nJane \"J\" Doe".into(),
        });
        let fresh: toml_edit::DocumentMut = toml::to_string(&cfg).unwrap().parse().unwrap();
        merge_table(doc.as_table_mut(), fresh.as_table());
        let text = doc.to_string();
        // Comments survive, values change, and the result still round-trips.
        assert!(text.contains("# Tap the Windows key (alone) to open Smowauncher instead of the Start menu."));
        assert!(text.contains("win_key = false"));
        assert!(text.contains("aliases = { ff = \"Firefox\" }"), "{text}");
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back, cfg);
    }

    #[test]
    fn sections_split_cleanly() {
        let names: Vec<&str> = default_sections().iter().map(|(n, _)| *n).collect();
        assert_eq!(names, ["general", "appearance", "apps", "shortcuts", "files", "calc", "clipboard", "snippets", "ai", "web", "updates"]);
        // Every block parses on its own (they get appended to older config files).
        for (name, block) in default_sections() {
            assert!(toml::from_str::<Config>(block).is_ok(), "{name}");
            assert!(block.starts_with(&format!("[{name}]")));
        }
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
