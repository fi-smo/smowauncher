//! UI thread: owns the Slint window and all launcher state.

use crate::apps::{self, AppEntry, IndexEvent, icons};
use crate::config::{self, Config};
use crate::platform::{UiEvent, autostart, input, memory, shell, window};
use crate::search::{self, Searcher};
use crate::usage::Usage;
use crate::{LauncherWindow, ResultItem, Theme};
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
const REINDEX_AFTER: Duration = Duration::from_secs(5 * 60);
const TRIM_AFTER: Duration = Duration::from_millis(1500);
const ICON_LOGICAL: f64 = 26.0;

struct App {
    ui: LauncherWindow,
    hwnd: Option<HWND>,
    cfg: Config,
    apps: Vec<AppEntry>,
    prepared: Vec<search::Prepared>,
    by_id: HashMap<String, usize>,
    usage: Usage,
    searcher: Searcher,
    results: Vec<usize>,
    model: Rc<VecModel<ResultItem>>,
    icons: HashMap<usize, Option<slint::Image>>,
    icon_size: u32,
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
    ui.on_activate(|index, modifier| {
        // Launching hides the window, which touches UI state; run it after this callback.
        let _ = slint::invoke_from_event_loop(move || {
            with_app(|a| a.activate(index as usize, modifier));
        });
    });
    ui.on_escape(|| {
        let _ = slint::invoke_from_event_loop(|| {
            with_app(|a| {
                if a.ui.get_query().is_empty() {
                    a.hide(true);
                } else {
                    a.ui.set_query(SharedString::new());
                    a.update_results("");
                }
            });
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
        results: Vec::new(),
        model,
        icons: HashMap::new(),
        icon_size,
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

impl App {
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
        let keep: Vec<usize> = self.results.clone();
        self.icons.retain(|k, _| keep.contains(k));
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
                let q = self.ui.get_query();
                self.update_results(&q);
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

    fn icon(&mut self, i: usize) -> Option<slint::Image> {
        let size = self.icon_size;
        let id = &self.apps[i].id;
        self.icons.entry(i).or_insert_with(|| icons::load(id, size)).clone()
    }

    fn update_results(&mut self, query: &str) {
        let q = query.trim();
        let (indices, section) = if q.is_empty() {
            let recent: Vec<usize> = self
                .usage
                .recent(RECENT_COUNT)
                .into_iter()
                .filter_map(|id| self.by_id.get(id).copied())
                .collect();
            (recent, "RECENT")
        } else {
            let limit = self.cfg.general.max_results.max(1);
            (self.searcher.search(q, &self.apps, &self.prepared, &self.usage, limit), "APPLICATIONS")
        };

        let mut y = 4.0f32;
        let mut items = Vec::with_capacity(indices.len());
        for (n, &i) in indices.iter().enumerate() {
            let header = n == 0;
            let h = ROW_H + if header { HEADER_H } else { 0.0 };
            let icon = self.icon(i);
            let app = &self.apps[i];
            items.push(ResultItem {
                title: app.name.as_str().into(),
                subtitle: SharedString::new(),
                badge: badge(app).into(),
                section: if header { section.into() } else { SharedString::new() },
                has_icon: icon.is_some(),
                icon: icon.unwrap_or_default(),
                glyph: app.name.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default().into(),
                y,
                h,
            });
            y += h;
        }
        self.results = indices;

        let empty = if q.is_empty() {
            if self.apps.is_empty() { "Indexing apps…".to_string() } else { "Type to search apps".to_string() }
        } else {
            format!("No results for \u{201C}{q}\u{201D}")
        };
        self.ui.set_empty_text(empty.into());
        if self.model.row_count() > 0 || !items.is_empty() {
            self.model.set_vec(items);
        }
        self.ui.set_selected(0);
        self.ui.invoke_reset_scroll();
    }

    fn activate(&mut self, index: usize, modifier: i32) {
        let Some(&i) = self.results.get(index) else { return };
        let app = self.apps[i].clone();
        match modifier {
            1 => match &app.path {
                Some(p) => shell::show_in_folder(p),
                None => return,
            },
            2 => shell::launch(app.launch.clone(), None, shell::Verb::RunAs),
            _ => shell::launch(app.launch.clone(), None, shell::Verb::Open),
        }
        let q = self.ui.get_query();
        self.usage.record(&app.id, &q);
        self.usage.save();
        self.hide(false);
    }

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
