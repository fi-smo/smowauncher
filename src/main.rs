#![windows_subsystem = "windows"]

mod app;
mod apps;
mod config;
mod files;
mod logging;
mod paths;
mod platform;
mod search;
mod usage;

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
