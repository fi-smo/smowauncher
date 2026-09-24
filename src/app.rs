//! UI thread: owns the Slint window and all launcher state.

use crate::apps::{self, AppEntry, IndexEvent, icons};
use crate::config::{self, Config};
use crate::files::{self, FileHit, Mode, everything};
use crate::platform::{UiEvent, autostart, clipboard, input, memory, shell, window};
use crate::search::{self, Searcher};
use crate::usage::Usage;
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
const HEADER_H: f32 = 30.0;
const RECENT_COUNT: usize = 8;
/// Apps shown above file results (mixed mode).
const APPS_ABOVE_FILES: usize = 5;
/// Everything results fetched per search, before local ranking.
const FILE_FETCH: u32 = 60;
/// Wait for a pause in typing before asking Everything.
const FILE_DEBOUNCE: Duration = Duration::from_millis(25);
const REINDEX_AFTER: Duration = Duration::from_secs(5 * 60);
const TRIM_AFTER: Duration = Duration::from_millis(1500);
const ICON_LOGICAL: f64 = 26.0;

#[derive(Clone)]
enum Row {
    App(usize),
    File(FileHit),
    Hint(Hint),
}

#[derive(Clone, Copy)]
enum Hint {
    StartEverything,
    InstallEverything,
}

struct App {
    ui: LauncherWindow,
    hwnd: Option<HWND>,
    cfg: Config,
    apps: Vec<AppEntry>,
    prepared: Vec<search::Prepared>,
    by_id: HashMap<String, usize>,
    usage: Usage,
    searcher: Searcher,
    model: Rc<VecModel<ResultItem>>,
    icons: HashMap<usize, Option<slint::Image>>,
    icon_size: u32,

    /// Bumped on every query change; async results for older generations are dropped.
    generation: u32,
    rows: Vec<Row>,
    app_hits: Vec<usize>,
    /// Empty query shows recently used apps instead of search results.
    showing_recent: bool,
    file_hits: Vec<FileHit>,
    /// The search text `file_hits` belong to.
    files_for: String,
    file_hint: Option<Hint>,
    file_timer: Timer,
    /// Keyed by `files::icons::key_for`; `None` = requested, not arrived (or failed).
    file_icons: HashMap<String, Option<slint::Image>>,
    refresh_timer: Timer,

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

pub fn run(cfg: Config) -> Result<(), slint::PlatformError> {
    let software = cfg.software_renderer();
    slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name(if software { "software" } else { "femtovg" }.into())
        .with_winit_window_attributes_hook(move |attrs| {
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
    ui.on_open_actions(|index| {
        with_app(|a| a.open_actions(index as usize));
    });
    ui.on_escape(|| {
        later(|a| {
            if a.ui.get_query().is_empty() {
                a.hide(true);
            } else {
                a.ui.set_query(SharedString::new());
                a.update_results("");
            }
        });
    });

    input::WIN_KEY_ENABLED.store(cfg.general.win_key, Ordering::Relaxed);
    input::FULLSCREEN_PASSTHROUGH.store(cfg.general.fullscreen_passthrough, Ordering::Relaxed);

    let icon_size = {
        let dpi = unsafe { windows::Win32::UI::HiDpi::GetDpiForSystem() };
        (ICON_LOGICAL * dpi as f64 / 96.0).round() as u32
    };

    let app = App {
        ui: ui.clone_strong(),
        hwnd: None,
        cfg,
        apps: Vec::new(),
        prepared: Vec::new(),
        by_id: HashMap::new(),
        usage: Usage::load(),
        searcher: Searcher::new(),
        model,
        icons: HashMap::new(),
        icon_size,
        generation: 0,
        rows: Vec::new(),
        app_hits: Vec::new(),
        showing_recent: true,
        file_hits: Vec::new(),
        files_for: String::new(),
        file_hint: None,
        file_timer: Timer::default(),
        file_icons: HashMap::new(),
        refresh_timer: Timer::default(),
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

    let hotkey = with_app(|a| a.cfg.general.hotkey.clone()).unwrap_or_default();
    input::spawn(
        |ev| {
            let _ = slint::invoke_from_event_loop(move || handle(ev));
        },
        &hotkey,
    );
    everything::spawn(|ev| on_ui(move |a| a.on_files_event(ev)));
    files::icons::spawn(icon_size, |icon| on_ui(move |a| a.on_file_icon(icon)));
    config::watch(|cfg| on_ui(move |a| a.apply_config(cfg)));
    apps::watch_start_menu(|| on_ui(|a| a.start_index()));
    with_app(|a| a.start_index());

    slint::run_event_loop_until_quit()?;
    input::shutdown();
    with_app(|a| a.usage.save());
    Ok(())
}

/// The native window only exists once the event loop is running.
fn wait_for_window(attempt: u32) {
    Timer::single_shot(Duration::from_millis(if attempt == 0 { 0 } else { 10 }), move || {
        let ready = with_app(|a| {
            let Some(hwnd) = window::hwnd_of(a.ui.window()) else { return false };
            window::init(hwnd, a.cfg.acrylic());
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
            with_app(|a| if a.visible { a.hide(true) } else { a.show() });
        }
        UiEvent::Show => {
            with_app(|a| a.show());
        }
        UiEvent::ForegroundChanged(h) => {
            with_app(|a| a.on_foreground_changed(HWND(h as *mut _)));
        }
        UiEvent::OpenSettings => {
            shell::launch(crate::paths::config_file().to_string_lossy().into_owned(), None, shell::Verb::Open);
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

fn badge(app: &AppEntry) -> &'static str {
    if app.launch.contains("steam://") {
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

impl App {
    // ---------------------------------------------------------------- visibility

    fn show(&mut self) {
        if self.visible {
            return;
        }
        let Some(hwnd) = self.hwnd else { return };
        let t = Instant::now();
        self.trim_timer.stop();
        self.prev_foreground = window::foreground();
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
    }

    fn reveal(&mut self, hwnd: HWND) {
        window::cloak(hwnd, false);
        window::activate(hwnd);
        self.ui.invoke_focus_input();
        self.ui.set_shown(true);
        self.visible = true;
        self.shown_at = Instant::now();
    }

    fn hide(&mut self, restore_focus: bool) {
        if !self.visible {
            return;
        }
        let Some(hwnd) = self.hwnd else { return };
        window::cloak(hwnd, true);
        self.visible = false;
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
        memory::trim();
        log::info!("trimmed: {}", memory::usage_string());
    }

    fn on_foreground_changed(&mut self, hwnd: HWND) {
        if !self.visible || !self.cfg.general.hide_on_blur || window::is_own(hwnd) {
            return;
        }
        // Late notification about the window that was active before we appeared.
        if hwnd == self.prev_foreground && self.shown_at.elapsed() < Duration::from_millis(300) {
            return;
        }
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
            IndexEvent::Apps(apps) => {
                log::info!("index: {} apps", apps.len());
                self.prepared = apps.iter().map(search::prepare).collect();
                self.by_id = apps.iter().enumerate().map(|(i, a)| (a.id.clone(), i)).collect();
                self.apps = apps;
                self.icons.clear();
                self.ui.set_status(format!("{} apps", self.apps.len()).into());
                let q = self.ui.get_query();
                self.update_results(&q);
            }
            IndexEvent::IconsReady => {
                self.indexing = false;
                self.icons.retain(|_, v| v.is_some());
                self.rebuild_rows(true);
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
        let size = self.icon_size;
        let id = &self.apps[i].id;
        self.icons.entry(i).or_insert_with(|| icons::load(id, size)).clone()
    }

    // ---------------------------------------------------------------- searching

    fn update_results(&mut self, query: &str) {
        self.generation = self.generation.wrapping_add(1);
        let (app_text, file_text, files_only) = match files::parse_query(query) {
            Mode::Mixed(t) => (t, t, false),
            Mode::FilesOnly(t) => ("", t, true),
        };

        self.showing_recent = !files_only && app_text.is_empty();
        self.app_hits = if files_only {
            Vec::new()
        } else if app_text.is_empty() {
            self.usage.recent(RECENT_COUNT).into_iter().filter_map(|id| self.by_id.get(id).copied()).collect()
        } else {
            let limit = self.cfg.general.max_results.max(1);
            self.searcher.search(app_text, &self.apps, &self.prepared, &self.usage, limit)
        };

        let min_chars = if files_only { 1 } else { self.cfg.files.min_chars.max(1) };
        if self.cfg.files.enabled && file_text.chars().count() >= min_chars {
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
            self.file_timer.stop();
            self.file_hits.clear();
            self.files_for.clear();
            self.file_hint = None;
        }

        let empty = if query.trim().is_empty() {
            if self.apps.is_empty() { "Indexing apps…".to_string() } else { "Type to search apps and files".to_string() }
        } else {
            format!("No results for \u{201C}{}\u{201D}", query.trim())
        };
        self.ui.set_empty_text(empty.into());
        self.rebuild_rows(false);
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
                self.file_hits.clear();
                self.files_for.clear();
                self.file_hint =
                    Some(if files::everything_exe().is_some() { Hint::StartEverything } else { Hint::InstallEverything });
                self.rebuild_rows(true);
            }
            _ => {} // superseded by a newer query
        }
    }

    fn file_icon(&mut self, hit: &FileHit) -> Option<slint::Image> {
        let key = files::icons::key_for(&hit.path, hit.folder);
        match self.file_icons.get(&key) {
            Some(icon) => icon.clone(),
            None => {
                self.file_icons.insert(key.clone(), None);
                files::icons::request(key, hit.path.clone(), hit.folder);
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

    /// Rebuilds the visible list from apps, files and hints. `keep_selection` is used when
    /// async results arrive, so the highlighted row doesn't jump while the user looks at it.
    fn rebuild_rows(&mut self, keep_selection: bool) {
        let mut rows: Vec<Row> = Vec::new();
        let has_files = !self.file_hits.is_empty() || self.file_hint.is_some();
        let app_limit = if has_files && !self.showing_recent { APPS_ABOVE_FILES } else { usize::MAX };
        rows.extend(self.app_hits.iter().take(app_limit).map(|&i| Row::App(i)));
        rows.extend(self.file_hits.iter().cloned().map(Row::File));
        if let Some(hint) = self.file_hint {
            rows.push(Row::Hint(hint));
        }

        let mut y = 4.0f32;
        let mut items = Vec::with_capacity(rows.len());
        let mut prev_section = "";
        for row in &rows {
            let section = match row {
                Row::App(_) if self.showing_recent => "RECENT",
                Row::App(_) => "APPLICATIONS",
                Row::File(_) | Row::Hint(_) => "FILES",
            };
            let header = section != prev_section;
            prev_section = section;
            let h = ROW_H + if header { HEADER_H } else { 0.0 };
            let mut item = self.row_item(row);
            item.section = if header { section.into() } else { SharedString::new() };
            item.y = y;
            item.h = h;
            items.push(item);
            y += h;
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
    }

    fn row_item(&mut self, row: &Row) -> ResultItem {
        match row {
            Row::App(i) => {
                let icon = self.app_icon(*i);
                let app = &self.apps[*i];
                ResultItem {
                    title: app.name.as_str().into(),
                    badge: badge(app).into(),
                    has_icon: icon.is_some(),
                    icon: icon.unwrap_or_default(),
                    glyph: app.name.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default().into(),
                    action: "Open Application".into(),
                    ..Default::default()
                }
            }
            Row::File(hit) => {
                let icon = self.file_icon(hit);
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
                title: "Everything isn't running".into(),
                subtitle: "File search uses Everything — press Enter to start it".into(),
                glyph: "!".into(),
                action: "Start Everything".into(),
                ..Default::default()
            },
            Row::Hint(Hint::InstallEverything) => ResultItem {
                title: "Install Everything to search files".into(),
                subtitle: "Free, instant file search from voidtools.com".into(),
                glyph: "?".into(),
                action: "Open Website".into(),
                ..Default::default()
            },
        }
    }

    // ---------------------------------------------------------------- actions

    fn open_actions(&mut self, index: usize) {
        let Some(row) = self.rows.get(index) else { return };
        let list = match row {
            Row::App(i) => {
                let has_path = self.apps[*i].path.is_some();
                let mut v = vec![action("Open", "Enter", "open"), action("Run as administrator", "Ctrl+Shift+Enter", "admin")];
                if has_path {
                    v.push(action("Open file location", "Ctrl+Enter", "folder"));
                    v.push(action("Copy path", "Ctrl+Shift+C", "copy-path"));
                    v.push(action("Properties", "Alt+Enter", "properties"));
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
            Row::Hint(Hint::InstallEverything) => vec![action("Open voidtools.com", "Enter", "open")],
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
                shell::launch("https://www.voidtools.com/downloads/".into(), None, shell::Verb::Open);
                true
            }
        };
        if done {
            self.hide(false);
        }
    }

    /// Returns true if the launcher should close.
    fn run_app_action(&mut self, i: usize, id: &str, query: &str) -> bool {
        let app = self.apps[i].clone();
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
        input::FULLSCREEN_PASSTHROUGH.store(cfg.general.fullscreen_passthrough, Ordering::Relaxed);
        if cfg.general.hotkey != self.cfg.general.hotkey {
            input::set_hotkey(&cfg.general.hotkey);
        }
        if cfg.acrylic() != self.cfg.acrylic() {
            self.ui.global::<Theme>().set_acrylic(cfg.acrylic());
            if let Some(hwnd) = self.hwnd {
                window::set_backdrop(hwnd, cfg.acrylic());
            }
        }
        if cfg.appearance.renderer != self.cfg.appearance.renderer {
            log::info!("config: renderer change takes effect after restart");
        }
        let reindex = cfg.apps != self.cfg.apps;
        self.cfg = cfg;
        if reindex {
            self.start_index();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parent_dir;

    #[test]
    fn parents() {
        assert_eq!(parent_dir(r"C:\a\b.txt"), r"C:\a");
        assert_eq!(parent_dir(r"C:\b.txt"), r"C:\");
        assert_eq!(parent_dir(r"C:\a\"), r"C:\");
    }
}
