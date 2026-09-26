//! The settings window: created on demand, filled from the config, and every edit is saved
//! straight to config.toml (comments preserved) and applied.

use super::{App, later, on_ui, with_app};
use crate::calc;
use crate::config::{self, Config};
use crate::files::{self, everything};
use crate::platform::{autostart, input, shell, window};
use crate::update;
use crate::snippets::{self, Snippet};
use crate::ai;
use crate::{AiCommandItem, Engine, SettingsWindow, SnippetItem};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Tells the winit window-attributes hook that the window being created is the settings
/// window, which gets normal decorations and a taskbar button (unlike the launcher).
pub static CREATING: AtomicBool = AtomicBool::new(false);

/// Page indices in settings.slint (the sidebar order differs; indices stay stable).
pub const PAGE_SNIPPETS: i32 = 8;
pub const PAGE_AI: i32 = 9;

fn snippet_item(s: &Snippet) -> SnippetItem {
    SnippetItem {
        keyword: s.keyword.as_str().into(),
        name: s.name.as_str().into(),
        text: s.text.as_str().into(),
        preview: snippets::preview(&s.text).into(),
    }
}

pub struct Settings {
    pub ui: SettingsWindow,
    app_folders: Rc<VecModel<SharedString>>,
    app_excludes: Rc<VecModel<SharedString>>,
    file_excludes: Rc<VecModel<SharedString>>,
    clip_ignore: Rc<VecModel<SharedString>>,
    engines: Rc<VecModel<Engine>>,
    engine_names: Rc<VecModel<SharedString>>,
    snippets: Rc<VecModel<SnippetItem>>,
    ai_commands: Rc<VecModel<AiCommandItem>>,
}

fn key_status() -> (String, bool) {
    let saved = crate::platform::credentials::read(ai::CREDENTIAL).is_some();
    let text = if saved {
        "A key is saved in Windows Credential Manager (encrypted for your Windows account)."
    } else if std::env::var("ANTHROPIC_API_KEY").is_ok_and(|k| !k.trim().is_empty()) {
        "Using the ANTHROPIC_API_KEY environment variable. Save a key here to use that instead."
    } else {
        "No key yet. Create one in the Anthropic Console and paste it here — it's stored in Windows Credential Manager, not in config.toml."
    };
    (text.into(), saved)
}

fn strings(v: &[String]) -> Rc<VecModel<SharedString>> {
    Rc::new(VecModel::from(v.iter().map(|s| SharedString::from(s.as_str())).collect::<Vec<_>>()))
}

fn collect(m: &VecModel<SharedString>) -> Vec<String> {
    m.iter().map(|s| s.to_string()).collect()
}

/// "Windows region (USD)", then every currency with rates.
fn currency_options() -> (Vec<SharedString>, Vec<String>) {
    let region = calc::locale::currency_code();
    let mut labels = vec![SharedString::from(format!("Windows region ({region})"))];
    let mut codes = vec![String::new()];
    let mut all: Vec<&str> = vec![
        "AUD", "BGN", "BRL", "CAD", "CHF", "CNY", "CZK", "DKK", "EUR", "GBP", "HKD", "HUF", "IDR", "ILS", "INR", "ISK",
        "JPY", "KRW", "MXN", "MYR", "NOK", "NZD", "PHP", "PLN", "RON", "SEK", "SGD", "THB", "TRY", "USD", "ZAR",
    ];
    all.sort();
    for code in all {
        if let Some((symbol, name)) = calc::rates::info(code) {
            labels.push(format!("{code} — {name} ({symbol})").into());
            codes.push(code.to_owned());
        }
    }
    (labels, codes)
}

fn everything_status(cfg: &Config) -> (String, bool, bool) {
    let running = everything::is_running();
    let installed = files::everything_exe().is_some();
    let text = if running {
        "Using Everything: instant search across all drives."
    } else if installed {
        "Everything is installed but not running."
    } else if cfg.files.windows_search {
        "Everything isn't installed: searching the Windows index (indexed folders only)."
    } else {
        "Everything isn't installed."
    };
    (text.to_owned(), running, installed)
}

fn rates_status() -> String {
    match calc::rates::date() {
        Some(d) => format!("European Central Bank rates from {d}, refreshed every 12 hours."),
        None => "Rates haven't been downloaded yet.".to_owned(),
    }
}

impl App {
    pub(super) fn open_settings(&mut self) {
        if let Some(s) = &self.settings {
            let _ = s.ui.show();
            if let Some(hwnd) = window::hwnd_of(s.ui.window()) {
                window::activate(hwnd);
            }
            return;
        }
        let ui = match SettingsWindow::new() {
            Ok(ui) => ui,
            Err(e) => {
                log::error!("settings: {e}");
                return;
            }
        };
        let cfg = &self.cfg;
        let s = Settings {
            app_folders: strings(&cfg.apps.extra_folders),
            app_excludes: strings(&cfg.apps.exclude),
            file_excludes: strings(&cfg.files.exclude),
            clip_ignore: strings(&cfg.clipboard.ignore_apps),
            engines: Rc::new(VecModel::from(
                cfg.web
                    .engines
                    .iter()
                    .map(|e| Engine { keyword: e.keyword.as_str().into(), name: e.name.as_str().into(), url: e.url.as_str().into() })
                    .collect::<Vec<_>>(),
            )),
            engine_names: Rc::new(VecModel::default()),
            snippets: Rc::new(VecModel::from(cfg.snippets.items.iter().map(snippet_item).collect::<Vec<_>>())),
            ai_commands: Rc::new(VecModel::from(
                cfg.ai
                    .commands
                    .iter()
                    .map(|c| AiCommandItem { name: c.name.as_str().into(), prompt: c.prompt.as_str().into() })
                    .collect::<Vec<_>>(),
            )),
            ui,
        };
        let ui = &s.ui;
        ui.set_dark(self.dark());
        ui.set_win_key(cfg.general.win_key);
        ui.set_win_double_tap(cfg.general.win_double_tap);
        ui.set_hotkey(cfg.general.hotkey.as_str().into());
        ui.set_hide_on_blur(cfg.general.hide_on_blur);
        ui.set_fullscreen_passthrough(cfg.general.fullscreen_passthrough);
        ui.set_max_results(cfg.general.max_results as i32);
        ui.set_autostart_status("Checking…".into());
        ui.set_theme_index(match cfg.appearance.theme.to_lowercase().as_str() {
            "dark" => 1,
            "light" => 2,
            _ => 0,
        });
        ui.set_backdrop_index(if cfg.acrylic() { 0 } else { 1 });
        ui.set_renderer_index(if cfg.software_renderer() { 0 } else { 1 });
        ui.set_trim_memory(cfg.appearance.trim_memory_on_hide);
        ui.set_app_folders(ModelRc::from(s.app_folders.clone()));
        ui.set_app_excludes(ModelRc::from(s.app_excludes.clone()));
        ui.set_file_excludes(ModelRc::from(s.file_excludes.clone()));
        ui.set_clip_ignore(ModelRc::from(s.clip_ignore.clone()));
        ui.set_engines(ModelRc::from(s.engines.clone()));
        ui.set_engine_names(ModelRc::from(s.engine_names.clone()));
        ui.set_snippets(ModelRc::from(s.snippets.clone()));
        ui.set_snip_expand(cfg.snippets.expand_anywhere);
        ui.set_ai_enabled(cfg.ai.enabled);
        let (status, saved) = key_status();
        ui.set_ai_key_status(status.into());
        ui.set_ai_key_saved(saved);
        ui.set_ai_models(ModelRc::from(Rc::new(VecModel::from(
            ai::MODELS.iter().map(|(_, l)| SharedString::from(*l)).collect::<Vec<_>>(),
        ))));
        ui.set_ai_model_index(ai::MODELS.iter().position(|(m, _)| *m == cfg.ai.model).unwrap_or(0) as i32);
        ui.set_ai_effort_index(ai::EFFORTS.iter().position(|e| *e == cfg.ai.effort).unwrap_or(1) as i32);
        ui.set_ai_commands(ModelRc::from(s.ai_commands.clone()));
        ui.set_files_enabled(cfg.files.enabled);
        ui.set_windows_search(cfg.files.windows_search);
        ui.set_min_chars(cfg.files.min_chars as i32);
        ui.set_max_mixed(cfg.files.max_mixed as i32);
        ui.set_max_files_only(cfg.files.max_files_only as i32);
        ui.set_file_preview(cfg.files.preview);
        let (status, running, installed) = everything_status(cfg);
        ui.set_everything_status(status.into());
        ui.set_everything_running(running);
        ui.set_everything_installed(installed);
        let (labels, codes) = currency_options();
        let wanted = cfg.calc.default_currency.trim().to_uppercase();
        ui.set_currency_options(ModelRc::from(Rc::new(VecModel::from(labels))));
        ui.set_currency_index(codes.iter().position(|c| *c == wanted).unwrap_or(0) as i32);
        ui.set_rates_status(rates_status().into());
        ui.set_clip_enabled(cfg.clipboard.enabled);
        ui.set_clip_images(cfg.clipboard.images);
        ui.set_clip_hotkey(cfg.clipboard.hotkey.as_str().into());
        ui.set_clip_max(cfg.clipboard.max_items as i32);
        ui.set_clip_count(self.clip.entries.len() as i32);
        ui.set_updates_enabled(cfg.updates.enabled);
        ui.set_version(update::current_version().into());
        ui.set_update_status(if update::is_installed_copy() {
            "".into()
        } else {
            "This copy isn't the installed one (run --install), so it doesn't update itself.".into()
        });
        Self::refresh_engine_names(&s, &cfg.web.fallback);

        ui.on_changed(|| later(|a| a.settings_changed()));
        ui.on_list_add(|list, text| later(move |a| a.settings_list_edit(&list, Some(text.to_string()), None)));
        ui.on_list_remove(|list, index| later(move |a| a.settings_list_edit(&list, None, Some(index as usize))));
        ui.on_browse_folder(|| later(|a| a.settings_browse_folder()));
        ui.on_engine_add(|k, n, u| later(move |a| a.settings_engine_add(k.trim(), n.trim(), u.trim())));
        ui.on_engine_remove(|i| later(move |a| a.settings_engine_remove(i as usize)));
        ui.on_action(|id| later(move |a| a.settings_action(&id)));
        ui.on_snippet_save(|| later(|a| a.settings_snippet_save()));
        ui.on_snippet_remove(|i| later(move |a| a.settings_snippet_remove(i as usize)));
        ui.on_ai_key_save(|k| later(move |a| a.settings_ai_key(Some(k.trim().to_string()))));
        ui.on_ai_key_remove(|| later(|a| a.settings_ai_key(None)));
        ui.on_ai_command_save(|| later(|a| a.settings_ai_command_save()));
        ui.on_ai_command_remove(|i| later(move |a| a.settings_ai_command_remove(i as usize)));
        ui.on_hotkey_recording(|recording| {
            later(move |a| {
                // The global shortcuts would swallow the keys being recorded.
                if recording {
                    input::set_hotkeys("", "");
                    input::WIN_KEY_ENABLED.store(false, Ordering::Relaxed);
                } else {
                    input::set_hotkeys(&a.cfg.general.hotkey, &a.clip_hotkey());
                    input::WIN_KEY_ENABLED.store(a.cfg.general.win_key, Ordering::Relaxed);
                }
            })
        });
        ui.window().on_close_requested(|| {
            later(|a| {
                // Destroy the window: settings cost no memory while closed.
                a.settings = None;
            });
            slint::CloseRequestResponse::HideWindow
        });

        CREATING.store(true, Ordering::SeqCst);
        if let Err(e) = s.ui.show() {
            log::error!("settings: {e}");
        }
        self.settings = Some(s);
        self.refresh_autostart_status();
        // The native window appears a few event-loop turns later: style it once it exists.
        let tries = std::cell::Cell::new(0);
        let timer = Rc::new(slint::Timer::default());
        let t = timer.clone();
        timer.start(slint::TimerMode::Repeated, std::time::Duration::from_millis(10), move || {
            tries.set(tries.get() + 1);
            let done = with_app(|a| {
                let dark = a.dark();
                let Some(hwnd) = a.settings.as_ref().and_then(|s| window::hwnd_of(s.ui.window())) else {
                    return a.settings.is_none();
                };
                window::style_settings_window(hwnd, dark);
                if !super::is_preview() {
                    window::activate(hwnd);
                }
                log::info!("settings: opened");
                true
            })
            .unwrap_or(true);
            if done || tries.get() > 200 {
                if !done {
                    log::warn!("settings: window never appeared");
                    CREATING.store(false, Ordering::SeqCst);
                }
                t.stop();
            }
        });
    }

    fn refresh_engine_names(s: &Settings, fallback: &str) {
        let names: Vec<SharedString> = s.engines.iter().map(|e| format!("{} ({})", e.name, e.keyword).into()).collect();
        let index = s.engines.iter().position(|e| e.keyword.eq_ignore_ascii_case(fallback)).unwrap_or(0);
        s.engine_names.set_vec(names);
        s.ui.set_fallback_index(index as i32);
    }

    fn refresh_autostart_status(&mut self) {
        std::thread::spawn(|| {
            let installed = autostart::is_installed();
            on_ui(move |a| {
                if let Some(s) = &a.settings {
                    s.ui.set_autostart_installed(installed);
                    s.ui.set_autostart_status(
                        if installed {
                            "Starts with admin rights when you sign in, so the Windows key works over admin windows too."
                        } else {
                            "Not set up. Needs one admin prompt."
                        }
                        .into(),
                    );
                }
            });
        });
    }

    /// Reads every field back from the window, validates, saves and applies.
    fn settings_changed(&mut self) {
        let Some(s) = &self.settings else { return };
        let ui = &s.ui;
        let mut cfg = self.cfg.clone();
        let mut problems = Vec::new();

        let hotkey = ui.get_hotkey().to_string();
        if hotkey.is_empty() || config::parse_hotkey(&hotkey).is_some() {
            cfg.general.hotkey = hotkey;
        } else {
            problems.push(format!("\"{hotkey}\" can't be used as a shortcut; use letters, digits, F-keys, Space, Tab or Enter."));
            ui.set_hotkey(self.cfg.general.hotkey.as_str().into());
        }
        let clip_hotkey = ui.get_clip_hotkey().to_string();
        if clip_hotkey.is_empty() || config::parse_hotkey(&clip_hotkey).is_some() {
            cfg.clipboard.hotkey = clip_hotkey;
        } else {
            problems.push(format!("\"{clip_hotkey}\" can't be used as a shortcut."));
            ui.set_clip_hotkey(self.cfg.clipboard.hotkey.as_str().into());
        }
        if !cfg.general.hotkey.is_empty() && cfg.general.hotkey.eq_ignore_ascii_case(&cfg.clipboard.hotkey) {
            problems.push("The launcher and clipboard shortcuts must be different.".into());
            cfg.clipboard.hotkey = self.cfg.clipboard.hotkey.clone();
            ui.set_clip_hotkey(cfg.clipboard.hotkey.as_str().into());
        }

        cfg.general.win_key = ui.get_win_key();
        cfg.general.win_double_tap = ui.get_win_double_tap();
        cfg.general.hide_on_blur = ui.get_hide_on_blur();
        cfg.general.fullscreen_passthrough = ui.get_fullscreen_passthrough();
        cfg.general.max_results = ui.get_max_results().max(1) as usize;
        cfg.appearance.theme = ["system", "dark", "light"][ui.get_theme_index().clamp(0, 2) as usize].into();
        cfg.appearance.backdrop = if ui.get_backdrop_index() == 0 { "acrylic" } else { "solid" }.into();
        let renderer = if ui.get_renderer_index() == 0 { "software" } else { "femtovg" };
        if renderer != cfg.appearance.renderer {
            problems.push("The renderer change takes effect after Smowauncher restarts.".into());
        }
        cfg.appearance.renderer = renderer.into();
        cfg.appearance.trim_memory_on_hide = ui.get_trim_memory();
        cfg.apps.extra_folders = collect(&s.app_folders);
        cfg.apps.exclude = collect(&s.app_excludes);
        cfg.files.enabled = ui.get_files_enabled();
        cfg.files.windows_search = ui.get_windows_search();
        cfg.files.min_chars = ui.get_min_chars().max(1) as usize;
        cfg.files.max_mixed = ui.get_max_mixed().max(1) as usize;
        cfg.files.max_files_only = ui.get_max_files_only().max(1) as usize;
        cfg.files.preview = ui.get_file_preview();
        cfg.files.exclude = collect(&s.file_excludes);
        let (_, codes) = currency_options();
        cfg.calc.default_currency = codes.get(ui.get_currency_index() as usize).cloned().unwrap_or_default();
        cfg.clipboard.enabled = ui.get_clip_enabled();
        cfg.clipboard.images = ui.get_clip_images();
        cfg.clipboard.max_items = ui.get_clip_max().max(1) as usize;
        cfg.clipboard.ignore_apps = collect(&s.clip_ignore);
        cfg.web.engines = s
            .engines
            .iter()
            .map(|e| crate::web::Engine { keyword: e.keyword.to_string(), name: e.name.to_string(), url: e.url.to_string() })
            .collect();
        cfg.web.fallback = s.engines.row_data(ui.get_fallback_index().max(0) as usize).map(|e| e.keyword.to_string()).unwrap_or_default();
        cfg.snippets.expand_anywhere = ui.get_snip_expand();
        cfg.snippets.items = s
            .snippets
            .iter()
            .map(|i| Snippet { keyword: i.keyword.to_string(), name: i.name.to_string(), text: i.text.to_string() })
            .collect();
        cfg.ai.enabled = ui.get_ai_enabled();
        cfg.ai.model = ai::MODELS.get(ui.get_ai_model_index().max(0) as usize).map(|(m, _)| m.to_string()).unwrap_or_default();
        cfg.ai.effort = ai::EFFORTS.get(ui.get_ai_effort_index().max(0) as usize).unwrap_or(&"medium").to_string();
        cfg.ai.commands = s
            .ai_commands
            .iter()
            .map(|c| ai::Command { name: c.name.to_string(), prompt: c.prompt.to_string() })
            .collect();
        cfg.updates.enabled = ui.get_updates_enabled();

        ui.set_message(problems.join("  ").into());
        if cfg == self.cfg {
            return;
        }
        if let Err(e) = config::save(&cfg) {
            ui.set_message(format!("Couldn't save config.toml: {e}").into());
            return;
        }
        let (status, running, installed) = everything_status(&cfg);
        ui.set_everything_status(status.into());
        ui.set_everything_running(running);
        ui.set_everything_installed(installed);
        self.apply_config(cfg);
    }

    fn settings_list_edit(&mut self, list: &str, add: Option<String>, remove: Option<usize>) {
        let Some(s) = &self.settings else { return };
        let model = match list {
            "app-folders" => &s.app_folders,
            "app-excludes" => &s.app_excludes,
            "file-excludes" => &s.file_excludes,
            "clip-ignore" => &s.clip_ignore,
            _ => return,
        };
        if let Some(text) = add.map(|t| t.trim().to_owned()).filter(|t| !t.is_empty()) {
            if !model.iter().any(|x| x.eq_ignore_ascii_case(&text)) {
                model.push(text.into());
            }
        }
        if let Some(i) = remove.filter(|&i| i < model.row_count()) {
            model.remove(i);
        }
        self.settings_changed();
    }

    fn settings_browse_folder(&mut self) {
        let owner = self.settings.as_ref().and_then(|s| window::hwnd_of(s.ui.window()));
        if let Some(folder) = shell::pick_folder(owner) {
            self.settings_list_edit("app-folders", Some(folder), None);
        }
    }

    fn settings_engine_add(&mut self, keyword: &str, name: &str, url: &str) {
        let Some(s) = &self.settings else { return };
        if keyword.contains(' ') || !url.starts_with("http") || !url.contains("{q}") {
            s.ui.set_message("Keywords can't contain spaces, and the URL must start with http and contain {q}.".into());
            return;
        }
        if let Some(i) = s.engines.iter().position(|e| e.keyword.eq_ignore_ascii_case(keyword)) {
            s.engines.remove(i);
        }
        s.engines.push(Engine { keyword: keyword.into(), name: name.into(), url: url.into() });
        let fallback = self.cfg.web.fallback.clone();
        Self::refresh_engine_names(s, &fallback);
        self.settings_changed();
    }

    fn settings_engine_remove(&mut self, index: usize) {
        let Some(s) = &self.settings else { return };
        if index < s.engines.row_count() && s.engines.row_count() > 1 {
            s.engines.remove(index);
        }
        let fallback = self.cfg.web.fallback.clone();
        Self::refresh_engine_names(s, &fallback);
        self.settings_changed();
    }

    /// Adds the snippet in the form, or replaces the one being edited.
    fn settings_snippet_save(&mut self) {
        let Some(s) = &self.settings else { return };
        let ui = &s.ui;
        let keyword = ui.get_snip_keyword().trim().to_string();
        let name = ui.get_snip_name().trim().to_string();
        let text = ui.get_snip_text().to_string();
        let editing = ui.get_snip_editing().to_string();
        if !snippets::valid_keyword(&keyword) {
            ui.set_message("A keyword needs at least 2 characters and no spaces (e.g. ;sig).".into());
            return;
        }
        if text.trim().is_empty() {
            ui.set_message("The snippet text is empty.".into());
            return;
        }
        if keyword != editing && s.snippets.iter().any(|i| i.keyword == keyword.as_str()) {
            ui.set_message(format!("There's already a snippet with the keyword \"{keyword}\".").into());
            return;
        }
        let item = snippet_item(&Snippet { keyword, name, text });
        match s.snippets.iter().position(|i| !editing.is_empty() && i.keyword == editing.as_str()) {
            Some(i) => s.snippets.set_row_data(i, item),
            None => s.snippets.push(item),
        }
        for set in [SettingsWindow::set_snip_editing, SettingsWindow::set_snip_keyword, SettingsWindow::set_snip_name, SettingsWindow::set_snip_text] {
            set(ui, SharedString::new());
        }
        self.settings_changed();
    }

    fn settings_snippet_remove(&mut self, index: usize) {
        let Some(s) = &self.settings else { return };
        if index < s.snippets.row_count() {
            s.snippets.remove(index);
        }
        self.settings_changed();
    }

    /// Saves (Some) or removes (None) the API key in Credential Manager.
    fn settings_ai_key(&mut self, key: Option<String>) {
        let Some(s) = &self.settings else { return };
        match key {
            Some(k) if k.is_empty() => {
                s.ui.set_message("Paste the key into the field first.".into());
                return;
            }
            Some(k) if !k.starts_with("sk-ant-") || k.chars().any(char::is_whitespace) => {
                s.ui.set_message("That doesn't look like an Anthropic API key (they start with sk-ant-).".into());
                return;
            }
            Some(k) => {
                if let Err(e) = crate::platform::credentials::write(ai::CREDENTIAL, &k) {
                    s.ui.set_message(format!("Couldn't save the key: {e}").into());
                    return;
                }
                s.ui.set_message("API key saved.".into());
            }
            None => {
                crate::platform::credentials::delete(ai::CREDENTIAL);
                s.ui.set_message("API key removed.".into());
            }
        }
        let (status, saved) = key_status();
        s.ui.set_ai_key_status(status.into());
        s.ui.set_ai_key_saved(saved);
    }

    fn settings_ai_command_save(&mut self) {
        let Some(s) = &self.settings else { return };
        let ui = &s.ui;
        let name = ui.get_ai_cmd_name().trim().to_string();
        let prompt = ui.get_ai_cmd_prompt().trim().to_string();
        let editing = ui.get_ai_cmd_editing().to_string();
        if name.is_empty() || prompt.is_empty() {
            ui.set_message("A command needs a name and instructions.".into());
            return;
        }
        let item = AiCommandItem { name: name.into(), prompt: prompt.into() };
        match s.ai_commands.iter().position(|c| !editing.is_empty() && c.name == editing.as_str()) {
            Some(i) => s.ai_commands.set_row_data(i, item),
            None => s.ai_commands.push(item),
        }
        for set in [SettingsWindow::set_ai_cmd_editing, SettingsWindow::set_ai_cmd_name, SettingsWindow::set_ai_cmd_prompt] {
            set(ui, SharedString::new());
        }
        self.settings_changed();
    }

    fn settings_ai_command_remove(&mut self, index: usize) {
        let Some(s) = &self.settings else { return };
        if index < s.ai_commands.row_count() {
            s.ai_commands.remove(index);
        }
        self.settings_changed();
    }

    /// Opens the settings window on a page (e.g. from a launcher action).
    pub(super) fn open_settings_page(&mut self, page: i32) {
        self.open_settings();
        if let Some(s) = &self.settings {
            s.ui.set_page(page);
        }
    }

    fn settings_message(&self, text: &str) {
        if let Some(s) = &self.settings {
            s.ui.set_message(text.into());
        }
    }

    fn settings_action(&mut self, id: &str) {
        match id {
            "autostart-install" | "autostart-remove" => {
                let install = id == "autostart-install";
                std::thread::spawn(move || {
                    let result = if install { autostart::install() } else { autostart::uninstall() };
                    on_ui(move |a| {
                        a.settings_message(&result.unwrap_or_else(|e| format!("Failed: {e}")));
                        a.refresh_autostart_status();
                    });
                });
            }
            "reindex" => {
                crate::apps::icons::clear_cache();
                self.icons.clear();
                self.start_index();
                self.settings_message("Rebuilding the app index…");
            }
            "start-everything" | "install-everything" => {
                if id == "start-everything" {
                    if let Some(exe) = files::everything_exe() {
                        shell::launch(exe, Some("-startup".into()), shell::Verb::Open);
                    }
                } else {
                    self.install_everything();
                }
                self.settings_message("Starting… this page updates when you reopen it.");
            }
            "refresh-rates" => {
                self.settings_message("Downloading exchange rates…");
                std::thread::spawn(|| {
                    calc::rates::refresh_blocking();
                    on_ui(|a| {
                        if let Some(s) = &a.settings {
                            s.ui.set_rates_status(rates_status().into());
                        }
                        a.settings_message("");
                    });
                });
            }
            "clear-clipboard" => {
                self.clip.clear();
                self.clip.save();
                if let Some(s) = &self.settings {
                    s.ui.set_clip_count(0);
                }
                self.settings_message("Clipboard history cleared.");
            }
            "check-updates" => {
                if !update::is_installed_copy() {
                    return;
                }
                if let Some(s) = &self.settings {
                    s.ui.set_update_status("Checking…".into());
                }
                std::thread::spawn(|| {
                    let result = update::check_and_stage();
                    on_ui(move |a| {
                        let text = match &result {
                            Ok(update::Outcome::UpToDate(v)) => format!("You're up to date ({v})."),
                            Ok(update::Outcome::Staged(v)) => {
                                format!("Version {v} downloaded. It installs when you close the launcher.")
                            }
                            Err(e) => format!("Couldn't check for updates: {e}"),
                        };
                        if let Some(s) = &a.settings {
                            s.ui.set_update_status(text.into());
                        }
                        if let Ok(update::Outcome::Staged(v)) = result {
                            a.on_update_staged(v);
                        }
                    });
                });
            }
            "open-config" => shell::launch(crate::paths::config_file().to_string_lossy().into_owned(), None, shell::Verb::Open),
            "open-logs" => shell::launch(crate::paths::data_dir().to_string_lossy().into_owned(), None, shell::Verb::Open),
            "open-releases" => {
                shell::launch(format!("https://github.com/{}/releases", update::REPO), None, shell::Verb::Open)
            }
            "open-console" => shell::launch("https://console.anthropic.com/settings/keys".into(), None, shell::Verb::Open),
            "open-github" =>shell::launch(format!("https://github.com/{}", update::REPO), None, shell::Verb::Open),
            _ => {}
        }
    }
}
