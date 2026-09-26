//! UI thread: owns the Slint window and all launcher state.

mod settings_ui;

use crate::apps::{self, AppEntry, IndexEvent, icons};
use crate::calc::{self, CalcResult, Calculator};
use crate::clip::{self, History};
use crate::commands;
use crate::emoji;
use crate::config::{self, Config};
use crate::files::{self, FileHit, Mode, everything};
use crate::platform::windows_list::{self, WindowInfo};
use crate::platform::{UiEvent, autostart, clipboard, emoji_render, input, memory, shell, window};
use crate::search::{self, Searcher};
use crate::snippets;
use crate::update;
use crate::usage::Usage;
use crate::web::{self, WebItem};
use crate::{ActionItem, LauncherWindow, ResultItem, Theme};
use slint::{ComponentHandle, Model, ModelRc, SharedString, Timer, TimerMode, VecModel};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::HWND;

const WIDTH: f64 = 760.0;
const HEIGHT: f64 = 474.0;
const ROW_H: f32 = 46.0;
const HERO_H: f32 = 72.0;
const HEADER_H: f32 = 30.0;
const RECENT_COUNT: usize = 8;
/// Apps shown above file results (mixed mode).
const APPS_ABOVE_FILES: usize = 5;
/// Open windows shown between apps and files (mixed mode).
const WINDOWS_MIXED: usize = 3;
/// Everything results fetched per search, before local ranking.
const FILE_FETCH: u32 = 60;
/// Wait for a pause in typing before asking Everything.
const FILE_DEBOUNCE: Duration = Duration::from_millis(25);
const REINDEX_AFTER: Duration = Duration::from_secs(5 * 60);
const TRIM_AFTER: Duration = Duration::from_millis(1500);
/// Second Enter within this time confirms shutdown/restart/sign out.
const CONFIRM_WINDOW: Duration = Duration::from_secs(4);
const ICON_LOGICAL: f64 = 26.0;
const PLACEHOLDER: &str = "Search apps, files and commands…";
/// Snippets shown among apps and files (mixed mode).
const SNIPPETS_MIXED: usize = 3;
/// Emoji shown for ":" before typing (recently picked ones first).
const EMOJI_DEFAULT: usize = 40;

#[derive(Clone)]
enum Row {
    App(usize),
    File(FileHit),
    Hint(Hint),
    Calc(CalcResult),
    Web(WebItem),
    Window(WindowInfo),
    /// Index into the clipboard history.
    Clip(usize),
    /// Index into `emoji::all()`.
    Emoji(usize),
    /// Index into `cfg.snippets.items`.
    Snippet(usize),
    /// "Set alias <text>" while App::alias_for is set.
    AliasPrompt(String),
}

#[derive(Clone, Copy)]
enum Hint {
    StartEverything,
    InstallEverything,
}

/// Which list the query selects.
#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Normal,
    /// "clip …" or the clipboard hotkey.
    Clipboard,
    /// "<…": open windows only.
    Windows,
    /// ":…" or "emoji …".
    Emoji,
    /// Typing an alias for App::alias_for.
    Alias,
    /// "snip …": saved snippets.
    Snippets,
}

struct App {
    ui: LauncherWindow,
    hwnd: Option<HWND>,
    cfg: Config,
    apps: Vec<AppEntry>,
    prepared: Vec<search::Prepared>,
    by_id: HashMap<String, usize>,
    /// Index of the Windows Settings app (its icon is reused for settings pages).
    settings_app: Option<usize>,
    usage: Usage,
    searcher: Searcher,
    calculator: Calculator,
    clip: History,
    clip_save_timer: Timer,
    model: Rc<VecModel<ResultItem>>,
    icons: HashMap<usize, Option<slint::Image>>,
    icon_size: u32,

    /// Bumped on every query change; async results for older generations are dropped.
    generation: u32,
    view: View,
    rows: Vec<Row>,
    app_hits: Vec<usize>,
    /// Empty query shows recently used apps instead of search results.
    showing_recent: bool,
    calc_hit: Option<CalcResult>,
    /// A keyword search ("yt …") or URL: shown first.
    web_top: Option<WebItem>,
    /// "Search Google for …" at the bottom.
    web_fallback: Option<WebItem>,
    windows: Vec<WindowInfo>,
    window_hits: Vec<usize>,
    clip_hits: Vec<usize>,
    emoji_hits: Vec<usize>,
    snippet_hits: Vec<usize>,
    /// What the preview panel shows (a path, or "clip:…"/"snip:…"); None = hidden.
    preview_key: Option<String>,
    /// Thumbnails of clipboard images by file name.
    clip_thumbs: HashMap<String, Option<slint::Image>>,
    /// Color renderings of emoji shown so far (see platform::emoji_render).
    emoji_icons: HashMap<usize, Option<slint::Image>>,
    /// App matched through an alias (shown with a hint).
    alias_hit: Option<usize>,
    /// App waiting for an alias to be typed (Ctrl+K → Add alias).
    alias_for: Option<usize>,
    /// Lowercase app name → first app with that name (aliases and pins may use names).
    name_index: HashMap<String, usize>,
    file_hits: Vec<FileHit>,
    /// The search text `file_hits` belong to.
    files_for: String,
    file_hint: Option<Hint>,
    file_timer: Timer,
    /// Keyed by `files::icons::key_for`; `None` = requested, not arrived (or failed).
    file_icons: HashMap<String, Option<slint::Image>>,
    refresh_timer: Timer,
    /// Command waiting for a confirming second Enter.
    armed: Option<(String, Instant)>,
    /// A downloaded, verified update waiting for the launcher to be idle.
    pending_update: Option<String>,
    update_timer: Timer,
    /// The settings window while it's open (destroyed on close).
    settings: Option<settings_ui::Settings>,

    visible: bool,
    prev_foreground: HWND,
    shown_at: Instant,
    last_index: Option<Instant>,
    indexing: bool,
    trim_timer: Timer,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|a| a.try_borrow_mut().ok()?.as_mut().map(f))
}

/// Queues `f` on the UI thread from any thread.
fn on_ui(f: impl FnOnce(&mut App) + Send + 'static) {
    let _ = slint::invoke_from_event_loop(move || {
        with_app(f);
    });
}

/// Runs `f` after the current Slint callback returns (it may hide the window or touch models).
fn later(f: impl FnOnce(&mut App) + 'static) {
    Timer::single_shot(Duration::ZERO, move || {
        with_app(f);
    });
}

/// `--preview`: render the launcher for a query and save a screenshot (developer aid).
pub struct Preview {
    pub query: String,
    pub out: std::path::PathBuf,
}

thread_local! {
    static PREVIEW: RefCell<Option<Preview>> = const { RefCell::new(None) };
}

/// Shows the window without taking focus, fills in the query, captures it and quits.
fn run_preview_step(a: &mut App) {
    let Some(hwnd) = a.hwnd else { return };
    let query = PREVIEW.with(|p| p.borrow().as_ref().map(|p| p.query.clone())).unwrap_or_default();
    // "::settings:<page>": capture a settings page instead of the launcher.
    if let Some(page) = query.strip_prefix("::settings:") {
        a.open_settings();
        if let Some(s) = &a.settings {
            s.ui.set_page(page.parse().unwrap_or(0));
        }
        Timer::single_shot(Duration::from_millis(1200), || {
            let shot = with_app(|a| a.settings.as_ref().and_then(|s| s.ui.window().take_snapshot().ok())).flatten();
            if let (Some(shot), Some(p)) = (shot, PREVIEW.with(|p| p.borrow_mut().take())) {
                // 32-bit top-down BMP from the RGBA snapshot.
                let (w, h) = (shot.width(), shot.height());
                let mut file = Vec::with_capacity(54 + (w * h * 4) as usize);
                file.extend(b"BM");
                file.extend((54 + w * h * 4).to_le_bytes());
                file.extend(0u32.to_le_bytes());
                file.extend(54u32.to_le_bytes());
                for v in [40u32, w, (h as i32).wrapping_neg() as u32] {
                    file.extend(v.to_le_bytes());
                }
                file.extend(1u16.to_le_bytes());
                file.extend(32u16.to_le_bytes());
                file.extend([0u8; 24]);
                for px in shot.as_slice() {
                    file.extend([px.b, px.g, px.r, 255]);
                }
                if let Err(e) = std::fs::write(&p.out, file) {
                    log::error!("capture failed: {e}");
                }
            }
            let _ = slint::quit_event_loop();
        });
        return;
    }
    a.windows = windows_list::list();
    a.ui.set_query(query.as_str().into());
    // Measure the synchronous part of a keystroke (apps, calc, windows, web; files are async).
    let t = Instant::now();
    for _ in 0..20 {
        a.update_results(&query);
    }
    println!("update_results({query:?}): {:?} per keystroke", t.elapsed() / 20);
    window::place(hwnd, WIDTH, HEIGHT);
    window::cloak(hwnd, false);
    a.ui.set_shown(true);
    a.visible = true;
    // Let async results (Everything, icons) and the entrance animation settle.
    Timer::single_shot(Duration::from_millis(1200), move || {
        if let Some(p) = PREVIEW.with(|p| p.borrow_mut().take()) {
            match window::capture(hwnd, &p.out) {
                Ok(()) => println!("saved {}", p.out.display()),
                Err(e) => println!("capture failed: {e}"),
            }
        }
        let _ = slint::quit_event_loop();
    });
}

pub fn run_preview(cfg: Config, preview: Preview) -> Result<(), slint::PlatformError> {
    PREVIEW.with(|p| *p.borrow_mut() = Some(preview));
    run(cfg)
}

fn is_preview() -> bool {
    PREVIEW.with(|p| p.borrow().is_some())
}

pub fn run(cfg: Config) -> Result<(), slint::PlatformError> {
    let software = cfg.software_renderer();
    slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name(if software { "software" } else { "femtovg" }.into())
        .with_winit_window_attributes_hook(move |attrs| {
            // The settings window is an ordinary window; everything below is launcher-only.
            // Consumed here: winit creates the native window some turns after show().
            if settings_ui::CREATING.swap(false, Ordering::SeqCst) {
                return attrs;
            }
            use slint::winit_030::winit::dpi::PhysicalPosition;
            use slint::winit_030::winit::platform::windows::WindowAttributesExtWindows;
            attrs
                // Only OpenGL needs winit's transparency; the software renderer's premultiplied
                // alpha reaches DWM directly through the extended frame (see window::set_backdrop).
                .with_transparent(!software)
                .with_decorations(false)
                .with_resizable(false)
                .with_active(false)
                .with_skip_taskbar(true)
                // Created off-screen; `window::init` cloaks it before it is ever placed on screen.
                .with_position(PhysicalPosition::new(-32000, -32000))
        })
        .select()?;

    let ui = LauncherWindow::new()?;
    ui.global::<Theme>().set_acrylic(cfg.acrylic());
    let model = Rc::new(VecModel::<ResultItem>::default());
    ui.set_results(ModelRc::from(model.clone()));

    ui.on_query_edited(|q| {
        with_app(|a| a.update_results(&q));
    });
    ui.on_activate(|index, id| {
        later(move |a| a.run_action(index as usize, &id));
    });
    ui.on_selection_changed(|_| {
        with_app(|a| a.update_preview());
    });
    ui.on_open_actions(|index| {
        with_app(|a| a.open_actions(index as usize));
    });
    ui.on_open_settings(|| {
        later(|a| {
            a.hide(true);
            a.open_settings();
        });
    });
    ui.on_escape(|| {
        later(|a| {
            if a.alias_for.is_some() {
                a.end_alias_prompt();
            } else if a.ui.get_query().is_empty() {
                a.hide(true);
            } else {
                a.ui.set_query(SharedString::new());
                a.update_results("");
            }
        });
    });

    input::WIN_KEY_ENABLED.store(cfg.general.win_key, Ordering::Relaxed);
    input::WIN_DOUBLE_TAP.store(cfg.general.win_double_tap, Ordering::Relaxed);
    input::FULLSCREEN_PASSTHROUGH.store(cfg.general.fullscreen_passthrough, Ordering::Relaxed);
    input::CLIPBOARD_HISTORY.store(cfg.clipboard.enabled, Ordering::Relaxed);
    input::CLIPBOARD_IMAGES.store(cfg.clipboard.images, Ordering::Relaxed);
    sync_snippets(&cfg);
    calc::rates::load_cache();
    calc::rates::refresh_if_stale();

    let icon_size = {
        let dpi = unsafe { windows::Win32::UI::HiDpi::GetDpiForSystem() };
        (ICON_LOGICAL * dpi as f64 / 96.0).round() as u32
    };

    let app = App {
        ui: ui.clone_strong(),
        hwnd: None,
        calculator: Calculator::new(&cfg.calc.default_currency),
        clip: if cfg.clipboard.enabled { History::load() } else { History::default() },
        cfg,
        apps: Vec::new(),
        prepared: Vec::new(),
        by_id: HashMap::new(),
        settings_app: None,
        usage: Usage::load(),
        searcher: Searcher::new(),
        clip_save_timer: Timer::default(),
        model,
        icons: HashMap::new(),
        icon_size,
        generation: 0,
        view: View::Normal,
        rows: Vec::new(),
        app_hits: Vec::new(),
        showing_recent: true,
        calc_hit: None,
        web_top: None,
        web_fallback: None,
        windows: Vec::new(),
        window_hits: Vec::new(),
        clip_hits: Vec::new(),
        emoji_hits: Vec::new(),
        snippet_hits: Vec::new(),
        clip_thumbs: HashMap::new(),
        preview_key: None,
        emoji_icons: HashMap::new(),
        alias_hit: None,
        alias_for: None,
        name_index: HashMap::new(),
        file_hits: Vec::new(),
        files_for: String::new(),
        file_hint: None,
        file_timer: Timer::default(),
        file_icons: HashMap::new(),
        refresh_timer: Timer::default(),
        armed: None,
        pending_update: None,
        update_timer: Timer::default(),
        settings: None,
        visible: false,
        prev_foreground: HWND::default(),
        shown_at: Instant::now(),
        last_index: None,
        indexing: false,
        trim_timer: Timer::default(),
    };
    APP.with(|a| *a.borrow_mut() = Some(app));

    ui.show()?;
    wait_for_window(0);

    if !is_preview() {
        let (hotkey, clip_hotkey) =
            with_app(|a| (a.cfg.general.hotkey.clone(), a.clip_hotkey())).unwrap_or_default();
        input::spawn(
            |ev| {
                let _ = slint::invoke_from_event_loop(move || handle(ev));
            },
            &hotkey,
            &clip_hotkey,
        );
        config::watch(|cfg| on_ui(move |a| a.apply_config(cfg)));
        apps::watch_start_menu(|| on_ui(|a| a.start_index()));
    }
    everything::spawn(|ev| on_ui(move |a| a.on_files_event(ev)));
    files::wsearch::set_sink(|ev| on_ui(move |a| a.on_wsearch_event(ev)));
    files::icons::spawn(icon_size, |icon| on_ui(move |a| a.on_file_icon(icon)));
    files::preview::spawn(|p| on_ui(move |a| a.on_preview(p)));
    with_app(|a| a.start_index());
    if !is_preview() {
        with_app(|a| a.schedule_update_checks());
    }

    slint::run_event_loop_until_quit()?;
    input::shutdown();
    with_app(|a| {
        a.usage.save();
        if a.cfg.clipboard.enabled {
            a.clip.save();
        }
    });
    Ok(())
}

/// The native window only exists once the event loop is running.
fn wait_for_window(attempt: u32) {
    Timer::single_shot(Duration::from_millis(if attempt == 0 { 0 } else { 10 }), move || {
        let ready = with_app(|a| {
            let Some(hwnd) = window::hwnd_of(a.ui.window()) else { return false };
            window::init(hwnd, a.cfg.acrylic(), a.dark());
            if !window::unregister_raw_input() {
                log::warn!("could not unregister raw input");
            }
            a.ui.global::<Theme>().set_dark(a.dark());
            a.hwnd = Some(hwnd);
            a.update_results("");
            log::info!("window ready ({})", memory::usage_string());
            true
        })
        .unwrap_or(false);
        if !ready && attempt < 200 {
            wait_for_window(attempt + 1);
        }
    });
}

fn handle(ev: UiEvent) {
    match ev {
        UiEvent::Toggle => {
            input::trace_dump("toggle");
            with_app(|a| if a.visible { a.hide(true) } else { a.show() });
        }
        UiEvent::Show => {
            with_app(|a| a.show());
        }
        UiEvent::ShowClipboard => {
            with_app(|a| {
                if !a.cfg.clipboard.enabled {
                    return;
                }
                a.show();
                a.ui.set_query("clip ".into());
                a.update_results("clip ");
            });
        }
        UiEvent::ClipboardText(text, source) => {
            with_app(|a| a.on_clipboard_text(text, source));
        }
        UiEvent::ClipboardImage(png, width, height, source) => {
            with_app(|a| a.on_clipboard_image(png, width, height, source));
        }
        UiEvent::ForegroundChanged(h) => {
            with_app(|a| a.on_foreground_changed(HWND(h as *mut _)));
        }
        UiEvent::CheckUpdates => {
            with_app(|a| a.check_for_update(true));
        }
        UiEvent::ThemeChanged => {
            with_app(|a| a.apply_theme());
        }
        UiEvent::OpenSettings => {
            with_app(|a| a.open_settings());
        }
        UiEvent::Reindex => {
            icons::clear_cache();
            with_app(|a| {
                a.icons.clear();
                a.start_index();
            });
        }
        UiEvent::InstallAutostart => {
            std::thread::spawn(|| {
                let result = autostart::install();
                crate::message_box(&result.unwrap_or_else(|e| format!("Autostart setup failed:\n{e}")));
            });
        }
        UiEvent::Quit => {
            log::info!("quit");
            let _ = slint::quit_event_loop();
        }
    }
}

/// After a winget install: start Everything once its exe appears, stop when it runs.
/// Checks every 3 s for up to 5 minutes (the installer may wait on a UAC prompt).
fn watch_for_everything(attempt: u32, launched: bool) {
    if attempt >= 100 || everything::is_running() {
        if everything::is_running() {
            log::info!("Everything is running");
        }
        return;
    }
    Timer::single_shot(Duration::from_secs(3), move || {
        let mut launched = launched;
        if !launched && !everything::is_running() && let Some(exe) = files::everything_exe() {
            // Give the installer a moment to finish writing files.
            std::thread::sleep(Duration::from_millis(500));
            shell::launch(exe, Some("-startup".into()), shell::Verb::Open);
            launched = true;
        }
        watch_for_everything(attempt + 1, launched);
    });
}

/// Hands the keyboard hook the snippets it may expand (valid keywords only).
fn sync_snippets(cfg: &Config) {
    input::SNIPPET_EXPAND.store(cfg.snippets.expand_anywhere, Ordering::Relaxed);
    let items = if cfg.snippets.expand_anywhere {
        cfg.snippets
            .items
            .iter()
            .filter(|s| snippets::valid_keyword(&s.keyword))
            .map(|s| (s.keyword.clone(), s.text.clone()))
            .collect()
    } else {
        Vec::new()
    };
    input::set_snippets(items);
}

/// Usage history also records picked emoji and snippets; "recent" lists only apps.
fn is_app_usage_id(id: &str) -> bool {
    !id.starts_with("emoji:") && !id.starts_with("snip:")
}

fn badge(app: &AppEntry) -> &'static str {
    if commands::is_command(&app.launch) {
        "Command"
    } else if commands::is_settings(&app.launch) {
        "Settings"
    } else if app.launch.contains("steam://") {
        "Steam game"
    } else if app.launch.contains("com.epicgames.launcher://") {
        "Epic game"
    } else if app.packaged {
        "Store app"
    } else {
        "Application"
    }
}

fn action(title: &str, shortcut: &str, id: &str) -> ActionItem {
    ActionItem { title: title.into(), shortcut: shortcut.into(), id: id.into() }
}

fn parent_dir(path: &str) -> &str {
    match path.trim_end_matches('\\').rfind('\\') {
        Some(i) if i <= 2 => &path[..i + 1],
        Some(i) => &path[..i],
        None => path,
    }
}

/// Splits the query into the list it selects and the text to filter with.
fn view_of(query: &str) -> (View, &str) {
    let t = query.trim_start();
    if t.eq_ignore_ascii_case("clip") {
        return (View::Clipboard, "");
    }
    if t.len() > 5 && t[..5].eq_ignore_ascii_case("clip ") {
        return (View::Clipboard, t[5..].trim());
    }
    if let Some(rest) = t.strip_prefix('<') {
        return (View::Windows, rest.trim());
    }
    if let Some(rest) = t.strip_prefix(':') {
        return (View::Emoji, rest.trim());
    }
    if t.eq_ignore_ascii_case("snip") {
        return (View::Snippets, "");
    }
    if t.len() > 5 && t[..5].eq_ignore_ascii_case("snip ") {
        return (View::Snippets, t[5..].trim());
    }
    if t.eq_ignore_ascii_case("emoji") {
        return (View::Emoji, "");
    }
    if t.len() > 6 && t[..6].eq_ignore_ascii_case("emoji ") {
        return (View::Emoji, t[6..].trim());
    }
    (View::Normal, query)
}

impl App {
    fn dark(&self) -> bool {
        // SMOW_THEME overrides the config (developer aid for previews).
        let theme = std::env::var("SMOW_THEME").unwrap_or_else(|_| self.cfg.appearance.theme.clone());
        match theme.to_lowercase().as_str() {
            "dark" => true,
            "light" => false,
            _ => window::system_prefers_dark(),
        }
    }

    fn apply_theme(&mut self) {
        let dark = self.dark();
        self.ui.global::<Theme>().set_dark(dark);
        if let Some(hwnd) = self.hwnd {
            window::set_dark(hwnd, dark);
        }
        if let Some(s) = &self.settings {
            s.ui.set_dark(dark);
            if let Some(hwnd) = window::hwnd_of(s.ui.window()) {
                window::style_settings_window(hwnd, dark);
            }
        }
    }

    // ---------------------------------------------------------------- updates

    /// First check a minute after startup, then every 12 hours (installed copy only).
    fn schedule_update_checks(&mut self) {
        if !self.cfg.updates.enabled || !update::is_installed_copy() {
            return;
        }
        Timer::single_shot(Duration::from_secs(60), || {
            with_app(|a| a.check_for_update(false));
        });
        self.update_timer.start(TimerMode::Repeated, Duration::from_secs(12 * 60 * 60), || {
            with_app(|a| a.check_for_update(false));
        });
    }

    /// manual = from the tray: report the outcome in a message box.
    fn check_for_update(&mut self, manual: bool) {
        if self.pending_update.is_some() {
            return;
        }
        if !update::is_installed_copy() {
            if manual {
                std::thread::spawn(|| {
                    crate::message_box(&format!(
                        "Smowauncher {} isn't the installed copy, so it doesn't update itself.\nRun it with --install first.",
                        update::current_version()
                    ))
                });
            }
            return;
        }
        std::thread::Builder::new()
            .name("update".into())
            .spawn(move || {
                let result = update::check_and_stage();
                match &result {
                    Ok(update::Outcome::UpToDate(v)) if manual => crate::message_box(&format!("Smowauncher {v} is up to date.")),
                    Ok(update::Outcome::Staged(v)) if manual => crate::message_box(&format!(
                        "Smowauncher {v} has been downloaded. It will be installed the next time the launcher closes."
                    )),
                    Err(e) if manual => crate::message_box(&format!("Checking for updates failed:\n{e}")),
                    Err(e) => log::warn!("update check failed: {e}"),
                    _ => {}
                }
                if let Ok(update::Outcome::Staged(v)) = result {
                    on_ui(move |a| a.on_update_staged(v));
                }
            })
            .ok();
    }

    fn on_update_staged(&mut self, version: String) {
        self.ui.set_status(format!("Update {version} ready · installs when closed").into());
        self.pending_update = Some(version);
        if !self.visible {
            self.apply_update();
        }
    }

    /// Swaps in the new version and exits; the new process takes over (same privileges).
    fn apply_update(&mut self) {
        match update::apply() {
            Ok(()) => {
                let _ = slint::quit_event_loop();
            }
            Err(e) => {
                log::error!("update: {e}");
                self.pending_update = None;
            }
        }
    }

    fn clip_hotkey(&self) -> String {
        if self.cfg.clipboard.enabled { self.cfg.clipboard.hotkey.clone() } else { String::new() }
    }

    // ---------------------------------------------------------------- visibility

    fn show(&mut self) {
        if self.visible {
            return;
        }
        let Some(hwnd) = self.hwnd else { return };
        let t = Instant::now();
        self.trim_timer.stop();
        self.prev_foreground = window::foreground();
        self.windows = windows_list::list();
        if window::place(hwnd, WIDTH, HEIGHT) {
            self.reveal(hwnd);
        } else {
            // Moved to a monitor with another DPI: let the window resize first, then re-center.
            Timer::single_shot(Duration::from_millis(30), move || {
                with_app(|a| {
                    window::place(hwnd, WIDTH, HEIGHT);
                    a.reveal(hwnd);
                });
            });
        }
        log::info!("show in {:?}", t.elapsed());
        if self.last_index.is_none_or(|t| t.elapsed() > REINDEX_AFTER) {
            self.start_index();
        }
        calc::rates::refresh_if_stale();
    }

    fn reveal(&mut self, hwnd: HWND) {
        window::cloak(hwnd, false);
        window::activate(hwnd);
        self.ui.invoke_focus_input();
        self.ui.set_shown(true);
        self.visible = true;
        input::LAUNCHER_VISIBLE.store(true, Ordering::Relaxed);
        self.shown_at = Instant::now();
    }

    fn hide(&mut self, restore_focus: bool) {
        if !self.visible {
            return;
        }
        let Some(hwnd) = self.hwnd else { return };
        window::cloak(hwnd, true);
        self.visible = false;
        input::LAUNCHER_VISIBLE.store(false, Ordering::Relaxed);
        self.armed = None;
        if self.alias_for.take().is_some() {
            self.ui.set_placeholder(PLACEHOLDER.into());
        }
        // Reset while cloaked so the next reveal shows a fresh frame immediately.
        self.ui.set_shown(false);
        self.ui.set_actions_open(false);
        self.ui.set_query(SharedString::new());
        self.update_results("");
        if restore_focus {
            window::restore_foreground(self.prev_foreground);
        }
        if self.cfg.appearance.trim_memory_on_hide {
            self.trim_timer.start(TimerMode::SingleShot, TRIM_AFTER, || {
                with_app(|a| a.trim());
            });
        }
    }

    fn trim(&mut self) {
        if self.visible {
            return;
        }
        // Keep icons of the "recent" list; they're what the next show displays first.
        let keep: Vec<usize> = self.app_hits.clone();
        self.icons.retain(|k, _| keep.contains(k));
        // Per-extension icons are few and reused constantly; per-file ones are not.
        self.file_icons.retain(|k, _| !k.starts_with("file:") && !k.starts_with("drive:"));
        self.emoji_icons.clear();
        self.clip_thumbs.clear();
        self.windows.clear();
        if self.pending_update.is_some() {
            self.apply_update();
            return;
        }
        // The Windows Search helper only lives while the launcher is in use.
        files::wsearch::stop();
        memory::trim();
        log::info!("trimmed: {}", memory::usage_string());
    }

    fn on_foreground_changed(&mut self, hwnd: HWND) {
        if self.visible || self.shown_at.elapsed() < Duration::from_secs(3) {
            log::info!("foreground -> {} (launcher visible: {})", window::describe(hwnd), self.visible);
        }
        if !self.visible || !self.cfg.general.hide_on_blur || window::is_own(hwnd) {
            return;
        }
        // Late notification about the window that was active before we appeared.
        if hwnd == self.prev_foreground && self.shown_at.elapsed() < Duration::from_millis(300) {
            return;
        }
        log::info!("hide on blur: foreground is now {}", window::describe(hwnd));
        input::trace_dump("blur");
        self.hide(false);
    }

    // ---------------------------------------------------------------- app index

    fn start_index(&mut self) {
        if self.indexing {
            return;
        }
        self.indexing = true;
        self.last_index = Some(Instant::now());
        if self.apps.is_empty() {
            self.ui.set_status("Indexing apps…".into());
        }
        apps::index_async(self.icon_size, |ev| on_ui(move |a| a.on_index_event(ev)));
    }

    fn on_index_event(&mut self, ev: IndexEvent) {
        match ev {
            IndexEvent::Apps(mut apps) => {
                log::info!("index: {} apps", apps.len());
                let app_count = apps.len();
                apps.extend(commands::entries());
                self.prepared = apps.iter().map(search::prepare).collect();
                self.by_id = apps.iter().enumerate().map(|(i, a)| (a.id.clone(), i)).collect();
                self.name_index.clear();
                for (i, a) in apps.iter().enumerate() {
                    self.name_index.entry(a.name.to_lowercase()).or_insert(i);
                }
                // Indices change with a new index.
                self.alias_for = None;
                self.settings_app = apps.iter().position(|a| a.id.to_lowercase().contains("windows.immersivecontrolpanel"));
                self.apps = apps;
                self.icons.clear();
                self.ui.set_status(format!("{app_count} apps").into());
                let q = self.ui.get_query();
                self.update_results(&q);
            }
            IndexEvent::IconsReady => {
                self.indexing = false;
                self.icons.retain(|_, v| v.is_some());
                self.rebuild_rows(true);
                if is_preview() {
                    run_preview_step(self);
                }
            }
            IndexEvent::Failed(e) => {
                self.indexing = false;
                log::error!("index failed: {e}");
                if self.apps.is_empty() {
                    self.ui.set_status("Indexing failed — see log".into());
                }
            }
        }
    }

    fn app_icon(&mut self, i: usize) -> Option<slint::Image> {
        let launch = &self.apps[i].launch;
        // Settings pages use the Settings app's icon; commands use a glyph.
        let source = if commands::is_settings(launch) {
            self.settings_app?
        } else if commands::is_command(launch) {
            return None;
        } else {
            i
        };
        let size = self.icon_size;
        let id = &self.apps[source].id;
        self.icons.entry(source).or_insert_with(|| icons::load(id, size)).clone()
    }

    // ---------------------------------------------------------------- aliases & pins

    /// An app from a config value: its id, or its (case-insensitive) name.
    fn resolve_app(&self, key: &str) -> Option<usize> {
        self.by_id.get(key).copied().or_else(|| self.name_index.get(&key.to_lowercase()).copied())
    }

    /// How the config refers to an app: its name when that's unambiguous, else its id.
    fn app_key(&self, i: usize) -> String {
        let app = &self.apps[i];
        let lower = app.name.to_lowercase();
        let unique = self.apps.iter().filter(|a| a.name.to_lowercase() == lower).count() == 1;
        if unique { app.name.clone() } else { app.id.clone() }
    }

    fn is_pinned(&self, i: usize) -> bool {
        self.cfg.shortcuts.pinned.iter().any(|k| self.resolve_app(k) == Some(i))
    }

    fn aliases_of(&self, i: usize) -> Vec<String> {
        self.cfg.shortcuts.aliases.iter().filter(|(_, k)| self.resolve_app(k) == Some(i)).map(|(a, _)| a.clone()).collect()
    }

    /// Edits [shortcuts] and writes config.toml (comments kept); applied right away.
    fn save_shortcuts(&mut self, edit: impl FnOnce(&mut config::Shortcuts)) {
        let mut cfg = self.cfg.clone();
        edit(&mut cfg.shortcuts);
        if let Err(e) = config::save(&cfg) {
            log::error!("saving shortcuts: {e}");
        }
        self.cfg = cfg;
    }

    fn end_alias_prompt(&mut self) {
        self.alias_for = None;
        self.ui.set_placeholder(PLACEHOLDER.into());
        self.ui.set_query(SharedString::new());
        self.update_results("");
    }

    /// Recently picked emoji, then the first ones in Unicode order (smileys).
    fn default_emoji(&self) -> Vec<usize> {
        let all = emoji::all();
        let mut v: Vec<usize> = self
            .usage
            .recent_matching(EMOJI_DEFAULT, |id| id.starts_with("emoji:"))
            .into_iter()
            .filter_map(|id| all.iter().position(|e| id[6..] == *e.glyph))
            .collect();
        for i in 0..all.len() {
            if v.len() >= EMOJI_DEFAULT {
                break;
            }
            if !v.contains(&i) {
                v.push(i);
            }
        }
        v
    }

    /// Pastes into the window that was active before the launcher opened.
    fn paste_text(&mut self, text: &str) {
        clipboard::set_text(text);
        self.paste_clipboard();
    }

    /// Hides the launcher and presses Ctrl+V in the window that was active before.
    fn paste_clipboard(&mut self) {
        self.hide(true);
        std::thread::spawn(|| {
            std::thread::sleep(Duration::from_millis(120));
            input::send_paste();
        });
    }

    // ---------------------------------------------------------------- clipboard history

    fn on_clipboard_text(&mut self, text: String, source: String) {
        if !self.cfg.clipboard.enabled || self.cfg.clipboard.ignore_apps.iter().any(|a| a.eq_ignore_ascii_case(&source)) {
            return;
        }
        if !self.clip.add(text, source, self.cfg.clipboard.max_items) {
            return;
        }
        self.clip_changed();
    }

    /// Stores a copied image (PNG) and lists it in the history.
    fn on_clipboard_image(&mut self, png: Vec<u8>, width: u32, height: u32, source: String) {
        let c = &self.cfg.clipboard;
        if !c.enabled || !c.images || c.ignore_apps.iter().any(|a| a.eq_ignore_ascii_case(&source)) {
            return;
        }
        if width as u64 * height as u64 > crate::imaging::MAX_PIXELS {
            return;
        }
        let img = clip::ClipImage { file: clip::image_file_name(&png), width, height };
        let path = clip::image_path(&img);
        if !path.exists() {
            let _ = std::fs::create_dir_all(clip::images_dir());
            if let Err(e) = std::fs::write(&path, &png) {
                log::warn!("clipboard image: {e}");
                return;
            }
        }
        if self.clip.add_image(img, source, self.cfg.clipboard.max_items) {
            self.clip_changed();
        }
    }

    fn clip_changed(&mut self) {
        // Saving is cheap, but copies often come in bursts.
        self.clip_save_timer.start(TimerMode::SingleShot, Duration::from_secs(2), || {
            with_app(|a| a.clip.save());
        });
        if self.visible && self.view == View::Clipboard {
            let q = self.ui.get_query();
            self.update_results(&q);
        }
    }

    /// Thumbnail of a clipboard image (decoded once, until the next trim).
    fn clip_thumb(&mut self, img: &clip::ClipImage) -> Option<slint::Image> {
        let size = self.icon_size;
        self.clip_thumbs
            .entry(img.file.clone())
            .or_insert_with(|| {
                let png = std::fs::read(clip::image_path(img)).ok()?;
                crate::imaging::decode_png(&png).map(|rgba| crate::imaging::thumbnail(&rgba, size))
            })
            .clone()
    }

    // ---------------------------------------------------------------- searching

    fn update_results(&mut self, query: &str) {
        self.generation = self.generation.wrapping_add(1);
        self.armed = None;
        let (view, text) = view_of(query);
        self.view = if self.alias_for.is_some() {
            View::Alias
        } else if view == View::Clipboard && !self.cfg.clipboard.enabled {
            View::Normal
        } else {
            view
        };

        self.app_hits.clear();
        self.calc_hit = None;
        self.web_top = None;
        self.web_fallback = None;
        self.window_hits.clear();
        self.clip_hits.clear();
        self.emoji_hits.clear();
        self.snippet_hits.clear();
        self.alias_hit = None;
        self.showing_recent = false;

        match self.view {
            View::Clipboard => {
                self.clip_hits = self.clip.matching(text);
                self.clear_files();
            }
            View::Windows => {
                self.window_hits = self.match_windows(text, usize::MAX);
                self.clear_files();
            }
            View::Emoji => {
                self.emoji_hits = if text.is_empty() { self.default_emoji() } else { emoji::search(text, 60) };
                self.clear_files();
            }
            View::Snippets => {
                self.snippet_hits = if text.is_empty() {
                    (0..self.cfg.snippets.items.len()).collect()
                } else {
                    self.match_snippets(text, usize::MAX)
                };
                self.clear_files();
            }
            View::Alias => self.clear_files(),
            View::Normal => self.search_normal(query),
        }

        let empty = match self.view {
            View::Clipboard if self.clip.entries.is_empty() => "Clipboard history is empty — copy some text".to_string(),
            View::Windows if self.windows.is_empty() => "No open windows".to_string(),
            View::Emoji => format!("No emoji for \u{201C}{text}\u{201D}"),
            View::Snippets if self.cfg.snippets.items.is_empty() => {
                "No snippets yet — add them in Settings → Snippets (Ctrl+,)".to_string()
            }
            View::Snippets => format!("No snippets match \u{201C}{text}\u{201D}"),
            View::Alias => {
                let name = self.alias_for.map(|i| self.apps[i].name.clone()).unwrap_or_default();
                format!("Type a short alias for {name}, then press Enter · Esc cancels")
            }
            _ if query.trim().is_empty() => {
                if self.apps.is_empty() { "Indexing apps…".into() } else { "Type to search apps, files and commands".into() }
            }
            _ => format!("No results for \u{201C}{}\u{201D}", query.trim()),
        };
        self.ui.set_empty_text(empty.into());
        self.rebuild_rows(false);
    }

    fn search_normal(&mut self, query: &str) {
        let (app_text, file_text, files_only) = match files::parse_query(query) {
            Mode::Mixed(t) => (t, t, false),
            Mode::FilesOnly(t) => ("", t, true),
        };

        if !files_only {
            self.web_top = web::prefixed(query, &self.cfg.web.engines).or_else(|| web::url_like(query));
            self.calc_hit = self.calculator.evaluate(query);
        }

        self.showing_recent = !files_only && app_text.is_empty();
        self.app_hits = if files_only {
            Vec::new()
        } else if app_text.is_empty() {
            // Pinned apps first, then the most used ones.
            let mut hits: Vec<usize> = self.cfg.shortcuts.pinned.iter().filter_map(|k| self.resolve_app(k)).collect();
            hits.dedup();
            let recent: Vec<usize> = self
                .usage
                .recent_matching(RECENT_COUNT + hits.len(), is_app_usage_id)
                .into_iter()
                .filter_map(|id| self.by_id.get(id).copied())
                .collect();
            hits.extend(recent.into_iter().filter(|i| !self.is_pinned(*i)).take(RECENT_COUNT));
            hits
        } else {
            let limit = self.cfg.general.max_results.max(1);
            let mut hits = self.searcher.search(app_text, &self.apps, &self.prepared, &self.usage, limit);
            let alias = app_text.trim().to_lowercase();
            if let Some(target) = self.cfg.shortcuts.aliases.get(&alias).and_then(|k| self.resolve_app(k)) {
                hits.retain(|&i| i != target);
                hits.insert(0, target);
                self.alias_hit = Some(target);
            }
            hits
        };
        if !files_only && !app_text.is_empty() {
            self.window_hits = self.match_windows(app_text, WINDOWS_MIXED);
            self.snippet_hits = self.match_snippets(app_text, SNIPPETS_MIXED);
        }
        if !files_only && app_text.chars().count() >= 2 && self.web_top.is_none() && self.calc_hit.is_none() {
            self.web_fallback = web::fallback(app_text, &self.cfg.web.engines, &self.cfg.web.fallback);
        }

        let min_chars = if files_only { 1 } else { self.cfg.files.min_chars.max(1) };
        // Keyword web searches and calculations aren't file names.
        let wants_files = self.web_top.as_ref().is_none_or(|w| !w.title.starts_with("Search")) && self.calc_hit.is_none();
        if self.cfg.files.enabled && wants_files && file_text.chars().count() >= min_chars {
            // While refining a query ("repo" → "repor"), keep the previous files visible
            // until Everything answers, instead of flashing an empty section.
            let refining = !self.files_for.is_empty() && file_text.to_lowercase().starts_with(&self.files_for.to_lowercase());
            if !refining {
                self.file_hits.clear();
            }
            let generation = self.generation;
            self.file_timer.start(TimerMode::SingleShot, FILE_DEBOUNCE, move || {
                with_app(|a| a.send_file_query(generation));
            });
        } else {
            self.clear_files();
        }
    }

    fn clear_files(&mut self) {
        self.file_timer.stop();
        self.file_hits.clear();
        self.files_for.clear();
        self.file_hint = None;
    }

    /// Snippets matching `text` by name or keyword, best first.
    fn match_snippets(&mut self, text: &str, limit: usize) -> Vec<usize> {
        let q = text.trim().to_lowercase();
        let mut scored: Vec<(usize, u32)> = Vec::new();
        for (i, s) in self.cfg.snippets.items.iter().enumerate() {
            let keyword_hit = s.keyword.to_lowercase() == q;
            let score = self.searcher.score_text(text, &format!("{} {}", s.name, s.keyword));
            if let Some(score) = score.map(|s| s + if keyword_hit { 500 } else { 0 }) {
                scored.push((i, score));
            }
        }
        // Mixed mode only shows confident matches.
        let min = if limit == usize::MAX { 0 } else { 16 * text.chars().count() as u32 };
        scored.retain(|(_, s)| *s >= min);
        scored.sort_by(|a, b| b.1.cmp(&a.1));
        scored.into_iter().take(limit).map(|(i, _)| i).collect()
    }

    /// Open windows matching `text` by title or process name (Z order when empty).
    fn match_windows(&mut self, text: &str, limit: usize) -> Vec<usize> {
        if text.is_empty() {
            return (0..self.windows.len()).take(limit).collect();
        }
        let mut scored: Vec<(usize, u32)> = Vec::new();
        for (i, w) in self.windows.iter().enumerate() {
            let hay = format!("{} {}", w.title, w.process);
            if let Some(s) = self.searcher.score_text(text, &hay) {
                scored.push((i, s));
            }
        }
        // Mixed mode only shows confident matches; fuzzy noise would push files down.
        let min = if limit == usize::MAX { 0 } else { 16 * text.chars().count() as u32 };
        scored.retain(|(_, s)| *s >= min);
        scored.sort_by(|a, b| b.1.cmp(&a.1));
        scored.into_iter().take(limit).map(|(i, _)| i).collect()
    }

    fn file_text(&self) -> String {
        match files::parse_query(&self.ui.get_query()) {
            Mode::Mixed(t) | Mode::FilesOnly(t) => t.to_owned(),
        }
    }

    fn files_only(&self) -> bool {
        matches!(files::parse_query(&self.ui.get_query()), Mode::FilesOnly(_))
    }

    fn send_file_query(&mut self, generation: u32) {
        if generation != self.generation {
            return;
        }
        let search = files::search_string(&self.file_text(), &self.cfg.files.exclude);
        everything::query(generation, search, FILE_FETCH);
    }

    fn on_files_event(&mut self, ev: everything::Event) {
        match ev {
            everything::Event::Results { generation, hits } if generation == self.generation => {
                let text = self.file_text();
                let limit = if self.files_only() { self.cfg.files.max_files_only } else { self.cfg.files.max_mixed };
                self.file_hits = files::rank(&text, hits, limit);
                self.files_for = text;
                self.file_hint = None;
                self.rebuild_rows(true);
            }
            everything::Event::Unavailable { generation } if generation == self.generation => {
                self.file_hint =
                    Some(if files::everything_exe().is_some() { Hint::StartEverything } else { Hint::InstallEverything });
                if self.cfg.files.windows_search {
                    // Keep the previous (refined) hits until the index answers.
                    files::wsearch::query(generation, &self.file_text(), FILE_FETCH);
                } else {
                    self.file_hits.clear();
                    self.files_for.clear();
                }
                self.rebuild_rows(true);
            }
            _ => {} // superseded by a newer query
        }
    }

    /// Results from the Windows Search fallback: shown like Everything's, with the
    /// Everything hint kept underneath.
    fn on_wsearch_event(&mut self, ev: everything::Event) {
        if let everything::Event::Results { generation, hits } = ev
            && generation == self.generation
        {
            let text = self.file_text();
            let limit = if self.files_only() { self.cfg.files.max_files_only } else { self.cfg.files.max_mixed };
            let hits = files::without_excluded(hits, &self.cfg.files.exclude);
            self.file_hits = files::rank(&text, hits, limit);
            self.files_for = text;
            self.rebuild_rows(true);
        }
    }

    /// Installs Everything with winget (or opens its website), then watches for it to start.
    fn install_everything(&mut self) {
        match files::winget_exe() {
            Some(winget) => {
                log::info!("installing Everything with winget");
                shell::launch(
                    winget,
                    Some("install --id voidtools.Everything --exact --source winget --accept-package-agreements --accept-source-agreements".into()),
                    shell::Verb::Open,
                );
                watch_for_everything(0, false);
            }
            None => shell::launch("https://www.voidtools.com/downloads/".into(), None, shell::Verb::Open),
        }
    }

    fn path_icon(&mut self, path: &str, folder: bool) -> Option<slint::Image> {
        if path.is_empty() {
            return None;
        }
        let key = files::icons::key_for(path, folder);
        match self.file_icons.get(&key) {
            Some(icon) => icon.clone(),
            None => {
                self.file_icons.insert(key.clone(), None);
                files::icons::request(key, path.to_owned(), folder);
                None
            }
        }
    }

    fn on_file_icon(&mut self, icon: files::icons::Icon) {
        let buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(&icon.rgba, icon.width, icon.height);
        self.file_icons.insert(icon.key, Some(slint::Image::from_rgba8_premultiplied(buf)));
        // Icons arrive in bursts; redraw once per frame at most.
        if !self.refresh_timer.running() {
            self.refresh_timer.start(TimerMode::SingleShot, Duration::from_millis(16), || {
                with_app(|a| a.rebuild_rows(true));
            });
        }
    }

    /// Rebuilds the visible list. `keep_selection` is used when async results arrive,
    /// so the highlighted row doesn't jump while the user looks at it.
    fn rebuild_rows(&mut self, keep_selection: bool) {
        let mut rows: Vec<Row> = Vec::new();
        match self.view {
            View::Clipboard => rows.extend(self.clip_hits.iter().map(|&i| Row::Clip(i))),
            View::Windows => rows.extend(self.window_hits.iter().map(|&i| Row::Window(self.windows[i].clone()))),
            View::Emoji => rows.extend(self.emoji_hits.iter().map(|&i| Row::Emoji(i))),
            View::Snippets => rows.extend(self.snippet_hits.iter().map(|&i| Row::Snippet(i))),
            View::Alias => {
                let text = self.ui.get_query().trim().to_lowercase();
                if !text.is_empty() {
                    rows.push(Row::AliasPrompt(text));
                }
            }
            View::Normal => {
                if let Some(w) = &self.web_top {
                    rows.push(Row::Web(w.clone()));
                }
                if let Some(c) = &self.calc_hit {
                    rows.push(Row::Calc(c.clone()));
                }
                let has_files = !self.file_hits.is_empty() || self.file_hint.is_some();
                let app_limit = if has_files && !self.showing_recent { APPS_ABOVE_FILES } else { usize::MAX };
                rows.extend(self.app_hits.iter().take(app_limit).map(|&i| Row::App(i)));
                rows.extend(self.window_hits.iter().map(|&i| Row::Window(self.windows[i].clone())));
                rows.extend(self.snippet_hits.iter().map(|&i| Row::Snippet(i)));
                rows.extend(self.file_hits.iter().cloned().map(Row::File));
                if let Some(hint) = self.file_hint {
                    rows.push(Row::Hint(hint));
                }
                if let Some(w) = &self.web_fallback {
                    rows.push(Row::Web(w.clone()));
                }
            }
        }

        let mut y = 4.0f32;
        let mut items = Vec::with_capacity(rows.len());
        let mut prev_section = String::new();
        let currency_header = match calc::rates::date() {
            Some(d) => format!("CURRENCY · ECB RATES {d}"),
            None => "CURRENCY · RATES NOT DOWNLOADED YET".into(),
        };
        for row in &rows {
            let section = match row {
                Row::App(i) if self.showing_recent && self.is_pinned(*i) => "PINNED",
                Row::App(_) if self.showing_recent => "RECENT",
                Row::App(_) => "APPLICATIONS",
                Row::File(_) | Row::Hint(_) => "FILES",
                Row::Calc(c) if c.kind == calc::Kind::Currency => currency_header.as_str(),
                Row::Calc(_) => "CALCULATOR",
                Row::Web(_) => "WEB",
                Row::Window(_) => "OPEN WINDOWS",
                Row::Clip(_) => "CLIPBOARD HISTORY",
                Row::Emoji(_) => "EMOJI",
                Row::Snippet(_) => "SNIPPETS",
                Row::AliasPrompt(_) => "ALIAS",
            };
            let header = section != prev_section;
            prev_section = section.to_owned();
            let base = if matches!(row, Row::Calc(_)) { HERO_H } else { ROW_H };
            let h = base + if header { HEADER_H } else { 0.0 };
            let mut item = self.row_item(row);
            item.section = if header { section.into() } else { SharedString::new() };
            item.y = y;
            item.h = h;
            items.push(item);
            y += h;
        }

        if self.view == View::Clipboard {
            self.ui.set_status(format!("{} clipboard items", self.clip.entries.len()).into());
        } else if self.view == View::Emoji {
            self.ui.set_status(format!("{} emoji", emoji::all().len()).into());
        } else if self.view == View::Snippets {
            self.ui.set_status(format!("{} snippets", self.cfg.snippets.items.len()).into());
        } else if !self.apps.is_empty() {
            let count = self.apps.iter().filter(|a| !commands::is_command(&a.launch) && !commands::is_settings(&a.launch)).count();
            self.ui.set_status(format!("{count} apps").into());
        }

        let selected = self.ui.get_selected();
        let keep = keep_selection && selected >= 0 && (selected as usize) < rows.len();
        self.rows = rows;
        if self.model.row_count() > 0 || !items.is_empty() {
            self.model.set_vec(items);
        }
        if !keep {
            self.ui.set_selected(0);
            self.ui.invoke_reset_scroll();
            self.ui.set_actions_open(false);
        }
        self.update_preview();
    }

    // ---------------------------------------------------------------- preview panel

    /// Shows the preview for the selected row (files, clipboard entries, snippets) or hides it.
    fn update_preview(&mut self) {
        let selected = self.ui.get_selected();
        let row = if self.cfg.files.preview && selected >= 0 { self.rows.get(selected as usize).cloned() } else { None };
        match row {
            Some(Row::File(hit)) => {
                let name = hit.name.clone();
                self.preview_file(&hit.path, hit.folder, name);
            }
            Some(Row::Clip(i)) => {
                let e = self.clip.entries[i].clone();
                match &e.image {
                    Some(img) => self.preview_file(&clip::image_path(img).to_string_lossy(), false, clip::title(&e)),
                    None => {
                        let key = format!("clip:{i}:{}", e.time);
                        let lines: Vec<&str> = e.text.lines().take(60).collect();
                        self.preview_text(key, "Copied text".into(), clip::describe(&e, clip::now()), lines.join("\n"));
                    }
                }
            }
            Some(Row::Snippet(i)) => {
                let s = self.cfg.snippets.items[i].clone();
                let title = if s.name.is_empty() { s.keyword.clone() } else { s.name.clone() };
                self.preview_text(format!("snip:{}", s.keyword), title, format!("Keyword {}", s.keyword), s.text);
            }
            _ => self.hide_preview(),
        }
    }

    fn preview_text(&mut self, key: String, title: String, meta: String, text: String) {
        if self.preview_key.as_deref() == Some(key.as_str()) {
            return;
        }
        self.preview_key = Some(key);
        self.ui.set_preview_has_image(false);
        self.ui.set_preview_title(title.into());
        self.ui.set_preview_meta(meta.into());
        self.ui.set_preview_text(text.into());
        self.ui.set_preview_visible(true);
    }

    /// Shows the name right away; the thumbnail, text and details arrive from the worker.
    fn preview_file(&mut self, path: &str, folder: bool, title: String) {
        if self.preview_key.as_deref() == Some(path) {
            return;
        }
        self.preview_key = Some(path.to_owned());
        self.ui.set_preview_has_image(false);
        self.ui.set_preview_title(title.into());
        self.ui.set_preview_meta(SharedString::new());
        self.ui.set_preview_text(SharedString::new());
        self.ui.set_preview_visible(true);
        // 268 logical pixels wide (the panel's content width), at the display's scale.
        let size = (268.0 * self.icon_size as f64 / ICON_LOGICAL).round() as u32;
        files::preview::request(files::preview::Request { path: path.to_owned(), folder, size });
    }

    fn on_preview(&mut self, p: files::preview::Preview) {
        if self.preview_key.as_deref() != Some(p.path.as_str()) {
            return; // the selection moved on
        }
        match p.image {
            Some((w, h, rgba)) => {
                let buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(&rgba, w, h);
                self.ui.set_preview_image(slint::Image::from_rgba8_premultiplied(buf));
                self.ui.set_preview_has_image(true);
                self.ui.set_preview_thumbnail(p.thumbnail);
            }
            None => self.ui.set_preview_has_image(false),
        }
        self.ui.set_preview_meta(p.meta.into());
        self.ui.set_preview_text(p.text.unwrap_or_default().into());
    }

    fn hide_preview(&mut self) {
        if self.preview_key.take().is_some() {
            self.ui.set_preview_visible(false);
            // Drop the (possibly large) image.
            self.ui.set_preview_image(slint::Image::default());
        }
    }

    fn row_item(&mut self, row: &Row) -> ResultItem {
        match row {
            Row::App(i) => {
                let icon = self.app_icon(*i);
                let app = &self.apps[*i];
                let glyph = commands::glyph(&app.launch);
                let armed = self.armed.as_ref().is_some_and(|(l, t)| *l == app.launch && t.elapsed() < CONFIRM_WINDOW);
                let subtitle = if armed {
                    "Press Enter again to confirm".into()
                } else if self.alias_hit == Some(*i) {
                    format!("alias \u{201C}{}\u{201D}", self.ui.get_query().trim().to_lowercase())
                } else {
                    String::new()
                };
                ResultItem {
                    title: app.name.as_str().into(),
                    subtitle: subtitle.into(),
                    warning: armed,
                    badge: badge(app).into(),
                    has_icon: icon.is_some(),
                    icon: icon.unwrap_or_default(),
                    icon_font: glyph.is_some(),
                    glyph: match glyph {
                        Some(g) => g.into(),
                        None => app.name.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default().into(),
                    },
                    action: if commands::is_command(&app.launch) {
                        "Run Command"
                    } else if commands::is_settings(&app.launch) {
                        "Open Settings"
                    } else {
                        "Open Application"
                    }
                    .into(),
                    ..Default::default()
                }
            }
            Row::File(hit) => {
                let icon = self.path_icon(&hit.path, hit.folder);
                ResultItem {
                    title: hit.name.as_str().into(),
                    subtitle: files::display_parent(&hit.path).into(),
                    badge: files::badge(hit).into(),
                    has_icon: icon.is_some(),
                    icon: icon.unwrap_or_default(),
                    action: if hit.folder { "Open Folder" } else { "Open File" }.into(),
                    ..Default::default()
                }
            }
            Row::Hint(Hint::StartEverything) => ResultItem {
                title: "Start Everything for full-drive search".into(),
                subtitle: if self.cfg.files.windows_search {
                    "Showing indexed folders only · Enter starts Everything"
                } else {
                    "File search uses Everything · Enter starts it"
                }
                .into(),
                glyph: "\u{E7BA}".into(),
                icon_font: true,
                action: "Start Everything".into(),
                ..Default::default()
            },
            Row::Hint(Hint::InstallEverything) => ResultItem {
                title: "Install Everything for full-drive file search".into(),
                subtitle: match (self.cfg.files.windows_search, files::winget_exe().is_some()) {
                    (true, true) => "Showing indexed folders only · Enter installs Everything (free) with winget",
                    (true, false) => "Showing indexed folders only · Enter opens voidtools.com",
                    (false, true) => "Free, instant file search · Enter installs it with winget",
                    (false, false) => "Free, instant file search · Enter opens voidtools.com",
                }
                .into(),
                glyph: "\u{E896}".into(),
                icon_font: true,
                action: if files::winget_exe().is_some() { "Install Everything" } else { "Open Website" }.into(),
                ..Default::default()
            },
            Row::Calc(c) => {
                let mut item = ResultItem {
                    title: c.result.as_str().into(),
                    subtitle: c.expression.as_str().into(),
                    badge: if c.kind == calc::Kind::Units { "Unit conversion" } else { "Calculator" }.into(),
                    hero: true,
                    action: "Copy Result".into(),
                    ..Default::default()
                };
                if let Some((amount, from, to)) = calc::currency_sides(c) {
                    let (from_sym, from_name) = calc::rates::info(&from).unwrap_or(("", ""));
                    let (to_sym, to_name) = calc::rates::info(&to).unwrap_or(("", ""));
                    item.subtitle = amount.into();
                    item.chip_left = from_sym.into();
                    item.chip_right = to_sym.into();
                    item.badge = from_name.into();
                    item.label_right = to_name.into();
                } else if c.kind == calc::Kind::Currency {
                    item.badge = "Currency".into();
                }
                item
            }
            Row::Web(w) => ResultItem {
                title: w.title.as_str().into(),
                subtitle: w.subtitle.as_str().into(),
                badge: "Web".into(),
                glyph: "\u{E774}".into(),
                icon_font: true,
                action: "Open in Browser".into(),
                ..Default::default()
            },
            Row::Window(w) => {
                let icon = self.path_icon(&w.exe, false);
                ResultItem {
                    title: w.title.as_str().into(),
                    subtitle: w.process.as_str().into(),
                    badge: "Window".into(),
                    has_icon: icon.is_some(),
                    icon: icon.unwrap_or_default(),
                    glyph: "\u{E737}".into(),
                    icon_font: true,
                    action: "Switch to Window".into(),
                    ..Default::default()
                }
            }
            Row::Emoji(i) => {
                let e = &emoji::all()[*i];
                let size = self.icon_size;
                let icon = self.emoji_icons.entry(*i).or_insert_with(|| emoji_render::render(e.glyph, size)).clone();
                let mut name = e.name.to_owned();
                if let Some(first) = name.get(..1) {
                    name = first.to_uppercase() + &name[1..];
                }
                ResultItem {
                    title: name.into(),
                    badge: "Emoji".into(),
                    has_icon: icon.is_some(),
                    icon: icon.unwrap_or_default(),
                    glyph: e.glyph.into(),
                    big_glyph: true,
                    action: "Paste Emoji".into(),
                    ..Default::default()
                }
            }
            Row::Snippet(i) => {
                let s = &self.cfg.snippets.items[*i];
                ResultItem {
                    title: if s.name.is_empty() { s.keyword.as_str() } else { s.name.as_str() }.into(),
                    subtitle: snippets::preview(&s.text).into(),
                    badge: s.keyword.as_str().into(),
                    glyph: "\u{E8C8}".into(),
                    icon_font: true,
                    action: "Paste Snippet".into(),
                    ..Default::default()
                }
            }
            Row::AliasPrompt(text) => {
                let target = self.alias_for.map(|i| self.apps[i].name.clone()).unwrap_or_default();
                let taken = self.cfg.shortcuts.aliases.get(text).filter(|k| self.resolve_app(k) != self.alias_for);
                ResultItem {
                    title: format!("Set alias \u{201C}{text}\u{201D} for {target}").into(),
                    subtitle: match taken {
                        Some(other) => format!("replaces the alias for {other}"),
                        None => String::new(),
                    }
                    .into(),
                    warning: taken.is_some(),
                    glyph: "\u{E8EC}".into(),
                    icon_font: true,
                    action: "Save Alias".into(),
                    ..Default::default()
                }
            }
            Row::Clip(i) => {
                let e = self.clip.entries[*i].clone();
                let thumb = e.image.as_ref().and_then(|img| self.clip_thumb(img));
                ResultItem {
                    title: clip::title(&e).into(),
                    subtitle: clip::describe(&e, clip::now()).into(),
                    badge: if e.image.is_some() { "Image" } else { "Text" }.into(),
                    has_icon: thumb.is_some(),
                    icon: thumb.unwrap_or_default(),
                    glyph: if e.image.is_some() { "\u{EB9F}" } else { "\u{E77F}" }.into(),
                    icon_font: true,
                    action: "Paste".into(),
                    ..Default::default()
                }
            }
        }
    }

    // ---------------------------------------------------------------- actions

    fn open_actions(&mut self, index: usize) {
        let Some(row) = self.rows.get(index) else { return };
        let list = match row {
            Row::App(i) => {
                let i = *i;
                let app = &self.apps[i];
                let mut v = if commands::is_command(&app.launch) || commands::is_settings(&app.launch) {
                    vec![action("Run", "Enter", "open")]
                } else {
                    let mut v =
                        vec![action("Open", "Enter", "open"), action("Run as administrator", "Ctrl+Shift+Enter", "admin")];
                    if app.path.is_some() {
                        v.push(action("Open file location", "Ctrl+Enter", "folder"));
                        v.push(action("Copy path", "Ctrl+Shift+C", "copy-path"));
                        v.push(action("Properties", "Alt+Enter", "properties"));
                    }
                    v
                };
                if self.is_pinned(i) {
                    v.push(action("Unpin", "", "unpin"));
                } else {
                    v.push(action("Pin to top", "", "pin"));
                }
                v.push(action("Add alias…", "", "alias"));
                for alias in self.aliases_of(i) {
                    v.push(ActionItem {
                        title: format!("Remove alias \u{201C}{alias}\u{201D}").into(),
                        shortcut: SharedString::new(),
                        id: format!("unalias:{alias}").into(),
                    });
                }
                v
            }
            Row::File(hit) if hit.folder => vec![
                action("Open", "Enter", "open"),
                action("Show in parent folder", "Ctrl+Enter", "folder"),
                action("Open in Terminal", "", "terminal"),
                action("Copy path", "Ctrl+Shift+C", "copy-path"),
                action("Copy folder", "", "copy-file"),
                action("Properties", "Alt+Enter", "properties"),
            ],
            Row::File(hit) => {
                let mut v = vec![
                    action("Open", "Enter", "open"),
                    action("Show in folder", "Ctrl+Enter", "folder"),
                    action("Open with…", "", "openwith"),
                ];
                if files::is_executable(&hit.path) {
                    v.push(action("Run as administrator", "Ctrl+Shift+Enter", "admin"));
                }
                v.extend([
                    action("Copy path", "Ctrl+Shift+C", "copy-path"),
                    action("Copy file", "", "copy-file"),
                    action("Open in Terminal", "", "terminal"),
                    action("Properties", "Alt+Enter", "properties"),
                ]);
                v
            }
            Row::Hint(Hint::StartEverything) => vec![action("Start Everything", "Enter", "open")],
            Row::Hint(Hint::InstallEverything) => vec![
                action(if files::winget_exe().is_some() { "Install with winget" } else { "Open voidtools.com" }, "Enter", "open"),
                action("Open voidtools.com", "", "website"),
            ],
            Row::Calc(_) => vec![
                action("Copy result", "Enter", "open"),
                action("Copy number only", "", "copy-number"),
                action("Copy expression and result", "", "copy-full"),
            ],
            Row::Web(_) => vec![action("Open in browser", "Enter", "open"), action("Copy URL", "Ctrl+Shift+C", "copy-path")],
            Row::Window(_) => vec![
                action("Switch to window", "Enter", "open"),
                action("Close window", "", "close"),
                action("Show program in folder", "Ctrl+Enter", "folder"),
            ],
            Row::Emoji(_) => vec![action("Paste", "Enter", "open"), action("Copy to clipboard", "Ctrl+Enter", "folder")],
            Row::Snippet(_) => vec![
                action("Paste", "Enter", "open"),
                action("Copy to clipboard", "Ctrl+Enter", "folder"),
                action("Edit snippets…", "", "edit"),
            ],
            Row::AliasPrompt(_) => vec![action("Save alias", "Enter", "open")],
            Row::Clip(i) => {
                let mut v = vec![action("Paste", "Enter", "open"), action("Copy to clipboard", "Ctrl+Enter", "folder")];
                if self.clip.entries.get(*i).is_some_and(|e| e.image.is_some()) {
                    v.push(action("Show image file", "", "show-file"));
                }
                v.extend([action("Delete from history", "", "delete"), action("Clear history", "", "clear")]);
                v
            }
        };
        self.ui.set_actions(ModelRc::from(Rc::new(VecModel::from(list))));
        self.ui.set_action_selected(0);
        self.ui.set_actions_open(true);
    }

    fn run_action(&mut self, index: usize, id: &str) {
        let Some(row) = self.rows.get(index).cloned() else { return };
        let query = self.ui.get_query().to_string();
        let done = match row {
            Row::App(i) => self.run_app_action(i, id, &query),
            Row::File(hit) => Self::run_file_action(&hit, id),
            Row::Hint(Hint::StartEverything) => {
                if let Some(exe) = files::everything_exe() {
                    shell::launch(exe, Some("-startup".into()), shell::Verb::Open);
                    // Give Everything a moment to load its database, then search again.
                    let generation = self.generation;
                    Timer::single_shot(Duration::from_millis(1500), move || {
                        with_app(|a| {
                            if a.generation == generation {
                                a.send_file_query(generation);
                            }
                        });
                    });
                }
                false
            }
            Row::Hint(Hint::InstallEverything) => {
                if id == "website" {
                    shell::launch("https://www.voidtools.com/downloads/".into(), None, shell::Verb::Open);
                } else {
                    self.install_everything();
                }
                true
            }
            Row::Calc(c) => {
                let text = match id {
                    "copy-number" => calc::number_only(&c.result),
                    "copy-full" => format!("{} = {}", c.expression, c.result),
                    _ => c.result.clone(),
                };
                clipboard::set_text(&text);
                true
            }
            Row::Web(w) => {
                match id {
                    "copy-path" => {
                        clipboard::set_text(&w.url);
                    }
                    _ => shell::launch(w.url.clone(), None, shell::Verb::Open),
                }
                true
            }
            Row::Window(w) => match id {
                "close" => {
                    windows_list::close(w.hwnd);
                    self.windows.retain(|x| x.hwnd != w.hwnd);
                    let q = self.ui.get_query();
                    self.update_results(&q);
                    false
                }
                "folder" if !w.exe.is_empty() => {
                    shell::show_in_folder(&w.exe);
                    true
                }
                _ => {
                    // Hide first so the launcher doesn't fight the target for the foreground.
                    self.hide(false);
                    windows_list::activate(w.hwnd);
                    false
                }
            },
            Row::Clip(i) => self.run_clip_action(i, id),
            Row::Emoji(i) => {
                let e = &emoji::all()[i];
                self.usage.record(&emoji::usage_id(e), "");
                self.usage.save();
                if id == "folder" {
                    clipboard::set_text(e.glyph);
                    true
                } else {
                    self.paste_text(e.glyph);
                    false
                }
            }
            Row::Snippet(i) => {
                let Some(s) = self.cfg.snippets.items.get(i).cloned() else { return };
                match id {
                    "edit" => {
                        self.hide(true);
                        self.open_settings_page(settings_ui::PAGE_SNIPPETS);
                        false
                    }
                    "folder" => {
                        clipboard::set_text(&snippets::expand(&s.text));
                        true
                    }
                    _ => {
                        self.usage.record(&snippets::usage_id(&s), &query);
                        self.usage.save();
                        self.paste_text(&snippets::expand(&s.text));
                        false
                    }
                }
            }
            Row::AliasPrompt(alias) => {
                if let Some(target) = self.alias_for {
                    let key = self.app_key(target);
                    log::info!("alias {alias:?} -> {key:?}");
                    self.save_shortcuts(|s| {
                        s.aliases.insert(alias, key);
                    });
                }
                self.end_alias_prompt();
                false
            }
        };
        if done {
            self.hide(false);
        }
    }

    fn run_clip_action(&mut self, i: usize, id: &str) -> bool {
        let Some(entry) = self.clip.entries.get(i).cloned() else { return false };
        match id {
            "delete" | "clear" => {
                if id == "delete" {
                    self.clip.remove(i);
                } else {
                    self.clip.clear();
                }
                self.clip.save();
                self.clip_thumbs.clear();
                let q = self.ui.get_query();
                self.update_results(&q);
                false
            }
            "folder" => {
                match &entry.image {
                    Some(img) => {
                        if let Ok(png) = std::fs::read(clip::image_path(img)) {
                            clipboard::set_image(&png);
                        }
                    }
                    None => {
                        clipboard::set_text(&entry.text);
                    }
                }
                true
            }
            "show-file" => {
                if let Some(img) = &entry.image {
                    shell::show_in_folder(&clip::image_path(img).to_string_lossy());
                }
                true
            }
            _ => {
                match &entry.image {
                    Some(img) => {
                        if std::fs::read(clip::image_path(img)).is_ok_and(|png| clipboard::set_image(&png)) {
                            self.paste_clipboard();
                        }
                    }
                    None => self.paste_text(&entry.text),
                }
                false
            }
        }
    }

    /// Returns true if the launcher should close.
    fn run_app_action(&mut self, i: usize, id: &str, query: &str) -> bool {
        let app = self.apps[i].clone();
        match id {
            "pin" | "unpin" => {
                let key = self.app_key(i);
                let pinned: Vec<String> = self.cfg.shortcuts.pinned.iter().filter(|k| self.resolve_app(k) != Some(i)).cloned().collect();
                self.save_shortcuts(|s| {
                    s.pinned = pinned;
                    if id == "pin" {
                        s.pinned.push(key);
                    }
                });
                let q = self.ui.get_query();
                self.update_results(&q);
                return false;
            }
            "alias" => {
                self.alias_for = Some(i);
                self.ui.set_placeholder(format!("Alias for {}…", app.name).into());
                self.ui.set_query(SharedString::new());
                self.update_results("");
                return false;
            }
            _ => {}
        }
        if let Some(alias) = id.strip_prefix("unalias:") {
            let alias = alias.to_owned();
            self.save_shortcuts(|s| {
                s.aliases.remove(&alias);
            });
            return false;
        }
        if app.launch == "cmd:emoji" || app.launch == "cmd:clipboard" {
            let q = if app.launch == "cmd:emoji" { ":" } else { "clip " };
            self.usage.record(&app.id, query);
            self.ui.set_query(q.into());
            self.update_results(q);
            return false;
        }
        if app.launch == "cmd:settings" {
            self.hide(true);
            self.open_settings();
            return false;
        }
        if commands::is_command(&app.launch) {
            if commands::needs_confirm(&app.launch)
                && !self.armed.as_ref().is_some_and(|(l, t)| *l == app.launch && t.elapsed() < CONFIRM_WINDOW)
            {
                self.armed = Some((app.launch.clone(), Instant::now()));
                self.rebuild_rows(true);
                return false;
            }
            self.armed = None;
            self.usage.record(&app.id, query);
            self.usage.save();
            self.hide(true);
            commands::run(&app.launch);
            return false;
        }
        match (id, &app.path) {
            ("open", _) => shell::launch(app.launch.clone(), None, shell::Verb::Open),
            ("admin", _) => shell::launch(app.launch.clone(), None, shell::Verb::RunAs),
            ("folder", Some(p)) => shell::show_in_folder(p),
            ("copy-path", Some(p)) => {
                clipboard::set_text(p);
            }
            ("properties", Some(p)) => shell::invoke_verb(p, "properties"),
            _ => return false,
        }
        if matches!(id, "open" | "admin") {
            self.usage.record(&app.id, query);
            self.usage.save();
        }
        true
    }

    fn run_file_action(hit: &FileHit, id: &str) -> bool {
        let path = hit.path.as_str();
        match id {
            "open" => {
                shell::launch(path.to_owned(), None, shell::Verb::Open);
                everything::inc_run_count(path);
            }
            "admin" if !hit.folder => shell::launch(path.to_owned(), None, shell::Verb::RunAs),
            "folder" => shell::show_in_folder(path),
            "openwith" if !hit.folder => shell::invoke_verb(path, "openas"),
            "properties" => shell::invoke_verb(path, "properties"),
            "terminal" => shell::open_terminal(if hit.folder { path } else { parent_dir(path) }),
            "copy-path" => {
                clipboard::set_text(path);
            }
            "copy-file" => {
                clipboard::set_files(&[path]);
            }
            _ => return false,
        }
        true
    }

    // ---------------------------------------------------------------- config

    fn apply_config(&mut self, cfg: Config) {
        input::WIN_KEY_ENABLED.store(cfg.general.win_key, Ordering::Relaxed);
        input::WIN_DOUBLE_TAP.store(cfg.general.win_double_tap, Ordering::Relaxed);
        input::FULLSCREEN_PASSTHROUGH.store(cfg.general.fullscreen_passthrough, Ordering::Relaxed);
        input::CLIPBOARD_HISTORY.store(cfg.clipboard.enabled, Ordering::Relaxed);
        input::CLIPBOARD_IMAGES.store(cfg.clipboard.images, Ordering::Relaxed);
        if cfg.snippets != self.cfg.snippets {
            sync_snippets(&cfg);
        }
        let old_clip_hotkey = self.clip_hotkey();
        if cfg.acrylic() != self.cfg.acrylic() {
            self.ui.global::<Theme>().set_acrylic(cfg.acrylic());
            if let Some(hwnd) = self.hwnd {
                window::set_backdrop(hwnd, cfg.acrylic());
            }
        }
        if cfg.appearance.renderer != self.cfg.appearance.renderer {
            log::info!("config: renderer change takes effect after restart");
        }
        if cfg.calc != self.cfg.calc {
            self.calculator = Calculator::new(&cfg.calc.default_currency);
        }
        if cfg.clipboard.enabled && !self.cfg.clipboard.enabled {
            self.clip = History::load();
        }
        let theme_changed = cfg.appearance.theme != self.cfg.appearance.theme;
        let reindex = cfg.apps != self.cfg.apps;
        let hotkey_changed = cfg.general.hotkey != self.cfg.general.hotkey;
        self.cfg = cfg;
        if hotkey_changed || self.clip_hotkey() != old_clip_hotkey {
            input::set_hotkeys(&self.cfg.general.hotkey, &self.clip_hotkey());
        }
        if theme_changed {
            self.apply_theme();
        }
        if reindex {
            self.start_index();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{View, parent_dir, view_of};

    #[test]
    fn parents() {
        assert_eq!(parent_dir(r"C:\a\b.txt"), r"C:\a");
        assert_eq!(parent_dir(r"C:\b.txt"), r"C:\");
        assert_eq!(parent_dir(r"C:\a\"), r"C:\");
    }

    #[test]
    fn views() {
        assert!(view_of("clip") == (View::Clipboard, ""));
        assert!(view_of("Clip  foo ") == (View::Clipboard, "foo"));
        assert!(view_of("clipchamp") == (View::Normal, "clipchamp"));
        assert!(view_of("<code") == (View::Windows, "code"));
        assert!(view_of("chrome") == (View::Normal, "chrome"));
        assert!(view_of(":fire") == (View::Emoji, "fire"));
        assert!(view_of("Emoji  heart") == (View::Emoji, "heart"));
        assert!(view_of("emojis") == (View::Normal, "emojis"));
        assert!(view_of("snip") == (View::Snippets, ""));
        assert!(view_of("snip sig") == (View::Snippets, "sig"));
        assert!(view_of("snipping tool") == (View::Normal, "snipping tool"));
    }
}
