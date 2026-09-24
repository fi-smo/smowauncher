//! Tiny file logger (no extra dependencies). Log file: `%LOCALAPPDATA%\Smowauncher\smowauncher.log`.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

struct FileLogger {
    file: Mutex<File>,
    start: Instant,
}

static LOGGER: OnceLock<FileLogger> = OnceLock::new();

impl log::Log for FileLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Info || metadata.target().starts_with("smowauncher")
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let t = self.start.elapsed().as_secs_f64();
        let line = format!("[{t:>10.3}] {:<5} {}\n", record.level(), record.args());
        if let Ok(mut f) = self.file.lock() {
            let _ = f.write_all(line.as_bytes());
        }
        if cfg!(debug_assertions) {
            eprint!("{line}");
        }
    }

    fn flush(&self) {
        if let Ok(mut f) = self.file.lock() {
            let _ = f.flush();
        }
    }
}

/// `name` distinguishes the main process from helper processes (e.g. the indexer).
pub fn init(name: &str) {
    let path = crate::paths::data_dir().join(format!("{name}.log"));
    // Start fresh when the log gets large.
    let truncate = std::fs::metadata(&path).map(|m| m.len() > 1024 * 1024).unwrap_or(false);
    let file = OpenOptions::new()
        .create(true)
        .append(!truncate)
        .write(true)
        .truncate(truncate)
        .open(&path);
    let Ok(file) = file else { return };
    let logger = LOGGER.get_or_init(|| FileLogger { file: Mutex::new(file), start: Instant::now() });
    let _ = log::set_logger(logger);
    log::set_max_level(if cfg!(debug_assertions) { log::LevelFilter::Debug } else { log::LevelFilter::Info });
}
