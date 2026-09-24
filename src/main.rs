#![windows_subsystem = "windows"]

mod app;
mod apps;
mod config;
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
