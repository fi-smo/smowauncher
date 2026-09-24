#![windows_subsystem = "windows"]

mod app;
mod calc;
mod clip;
mod commands;
mod apps;
mod config;
mod files;
mod logging;
mod paths;
mod platform;
mod search;
mod usage;
mod web;

slint::include_modules!();

use platform::{autostart, input, instance};

pub fn message_box(text: &str) {
    use windows::Win32::UI::WindowsAndMessaging::{MB_ICONINFORMATION, MB_OK, MessageBoxW};
    let text = platform::wide(text);
    let title = platform::wide("Smowauncher");
    unsafe {
        MessageBoxW(None, platform::pcwstr(&text), platform::pcwstr(&title), MB_OK | MB_ICONINFORMATION);
    }
}

fn exit_code(r: Result<(), String>) -> i32 {
    match r {
        Ok(()) => 0,
        Err(e) => {
            log::error!("{e}");
            1
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(String::as_str).unwrap_or("");

    match cmd {
        "--index" => {
            logging::init("indexer");
            std::process::exit(apps::indexer::run(&args[2..]));
        }
        "--install" | "--uninstall" => {
            logging::init("setup");
            let r = if cmd == "--install" { autostart::install() } else { autostart::uninstall() };
            message_box(&r.unwrap_or_else(|e| format!("Failed:\n{e}")));
            return;
        }
        "--install-elevated" => {
            logging::init("setup");
            std::process::exit(exit_code(autostart::install_elevated()));
        }
        "--uninstall-elevated" => {
            logging::init("setup");
            std::process::exit(exit_code(autostart::uninstall_elevated()));
        }
        "--icon-debug" => {
            unsafe {
                let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
            }
            apps::icons::debug(args.get(2).map(String::as_str).unwrap_or(""));
            return;
        }
        "--files-debug" => {
            // Developer aid: run a file search through Everything and print the ranked hits.
            logging::init("setup");
            log::set_max_level(log::LevelFilter::Debug);
            let no_exclude = args.get(2).is_some_and(|a| a == "--no-exclude");
            let q = args[if no_exclude { 3 } else { 2 }..].join(" ");
            let mut cfg = config::load();
            if no_exclude {
                cfg.files.exclude.clear();
            }
            let (tx, rx) = std::sync::mpsc::channel();
            files::everything::spawn(move |ev| {
                let _ = tx.send(ev);
            });
            std::thread::sleep(std::time::Duration::from_millis(50));
            let t = std::time::Instant::now();
            files::everything::query(1, files::search_string(&q, &cfg.files.exclude), 60);
            match rx.recv_timeout(std::time::Duration::from_secs(3)) {
                Ok(files::everything::Event::Results { hits, .. }) => {
                    println!("{} hits in {:?}", hits.len(), t.elapsed());
                    for h in files::rank(&q, hits, cfg.files.max_mixed) {
                        println!("  {:<40} {:<8} runs={} {}", h.name, files::badge(&h), h.run_count, files::display_parent(&h.path));
                    }
                }
                Ok(files::everything::Event::Unavailable { .. }) => println!("Everything unavailable"),
                Err(_) => println!("timed out"),
            }
            return;
        }
        "--calc-debug" => {
            // Developer aid: evaluate expressions exactly like the launcher (and raw through fend).
            calc::rates::refresh_blocking();
            let mut c = calc::Calculator::new(&config::load().calc.default_currency);
            for q in &args[2..] {
                let raw = fend_core::evaluate(q, &mut fend_core::Context::new()).map(|r| r.get_main_result().to_owned());
                println!("{q:<24} => {:?}   (raw fend: {raw:?})", c.evaluate(q).map(|r| (r.expression, r.result, r.kind)));
            }
            return;
        }
        "--preview" => {
            // Developer aid: render the launcher for a query (no hooks, no focus) and save a BMP.
            logging::init("preview");
            let preview = app::Preview {
                query: args.get(2).cloned().unwrap_or_default(),
                out: args.get(3).map(Into::into).unwrap_or_else(|| "preview.bmp".into()),
            };
            if let Err(e) = app::run_preview(config::load(), preview) {
                println!("preview failed: {e}");
            }
            return;
        }
        "--wsearch" => {
            logging::init("wsearch");
            std::process::exit(files::wsearch::serve());
        }
        "--wsearch-debug" => {
            // Developer aid: query the Windows Search index directly and print ranked hits.
            unsafe {
                let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
            }
            let q = args[2..].join(" ");
            let t = std::time::Instant::now();
            match files::wsearch::Connection::open() {
                Ok(conn) => {
                    println!("connected in {:?}", t.elapsed());
                    for _ in 0..2 {
                        let t = std::time::Instant::now();
                        match conn.search(&q, 60) {
                            Ok(hits) => {
                                println!("{} hits in {:?}", hits.len(), t.elapsed());
                                for h in files::rank(&q, hits, 8) {
                                    println!("  {:<40} {:<8} {}", h.name, files::badge(&h), files::display_parent(&h.path));
                                }
                            }
                            Err(e) => println!("query failed: {e}"),
                        }
                    }
                }
                Err(e) => println!("connect failed: {e}"),
            }
            return;
        }
        "--via-explorer" => {
            // Developer aid: run a command outside any package context / elevation.
            let file = args.get(2).cloned().unwrap_or_default();
            let rest = args[3.min(args.len())..].join(" ");
            if let Err(e) = platform::shell::run_via_explorer(&file, &rest) {
                println!("failed: {e}");
            }
            return;
        }
        "--update" => {
            logging::init("setup");
            if let Err(e) = autostart::update() {
                message_box(&format!("Update failed:\n{e}"));
                std::process::exit(1);
            }
            return;
        }
        "--quit" => {
            instance::signal(input::quit_message());
            return;
        }
        _ => {}
    }

    logging::init("smowauncher");
    // Release builds abort on panic; leave a trace first. (If the process dies, its keyboard
    // hook goes with it and the Win key simply opens Start again; the task restarts us.)
    std::panic::set_hook(Box::new(|info| {
        log::error!("panic: {info}\n{}", std::backtrace::Backtrace::force_capture());
        log::logger().flush();
    }));
    if !instance::acquire() {
        // Already running: just open it.
        instance::signal(input::show_message());
        return;
    }
    log::info!(
        "starting Smowauncher {} (elevated: {})",
        env!("CARGO_PKG_VERSION"),
        platform::shell::is_elevated()
    );
    let cfg = config::load();
    if let Err(e) = app::run(cfg) {
        log::error!("fatal: {e}");
        message_box(&format!("Smowauncher could not start:\n{e}"));
    }
}
