//! Well-known folders. Config lives in roaming AppData, caches in local AppData.

use std::path::PathBuf;

fn env_dir(var: &str) -> PathBuf {
    std::env::var_os(var).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

/// `%APPDATA%\Smowauncher` — config.toml, usage.json
pub fn config_dir() -> PathBuf {
    let dir = env_dir("APPDATA").join("Smowauncher");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// `%LOCALAPPDATA%\Smowauncher` — logs, icon cache
pub fn data_dir() -> PathBuf {
    let dir = env_dir("LOCALAPPDATA").join("Smowauncher");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

pub fn config_file() -> PathBuf {
    config_dir().join("config.toml")
}

pub fn icon_dir() -> PathBuf {
    let dir = data_dir().join("icons");
    let _ = std::fs::create_dir_all(&dir);
    dir
}
