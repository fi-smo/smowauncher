//! File & folder search through Everything.

pub mod everything;
pub mod icons;
pub mod wsearch;

use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FileHit {
    pub name: String,
    /// Full path including the name.
    pub path: String,
    pub folder: bool,
    pub size: u64,
    /// Unix seconds (0 = unknown).
    pub modified: u64,
    pub run_count: u32,
}

/// What the user asked for.
#[derive(Debug, PartialEq, Eq)]
pub enum Mode<'a> {
    /// Apps first, files below once the query is long enough.
    Mixed(&'a str),
    /// `f query` or `/query`: files only.
    FilesOnly(&'a str),
}

pub fn parse_query(q: &str) -> Mode<'_> {
    let t = q.trim_start();
    if let Some(rest) = t.strip_prefix("f ").or_else(|| t.strip_prefix('/')) {
        return Mode::FilesOnly(rest.trim());
    }
    Mode::Mixed(q.trim())
}

/// The Everything search string: the user's text plus exclusions of noisy locations.
pub fn search_string(text: &str, exclude: &[String]) -> String {
    let mut s = text.to_owned();
    for ex in exclude.iter().filter(|e| !e.trim().is_empty()) {
        s.push_str(&format!(" !\"{}\"", ex.replace('"', "")));
    }
    s
}

/// Drops hits inside excluded locations (Everything applies these itself; Windows Search
/// results are filtered here with the same list).
pub fn without_excluded(mut hits: Vec<FileHit>, exclude: &[String]) -> Vec<FileHit> {
    let ex: Vec<String> = exclude.iter().map(|e| e.trim().to_lowercase()).filter(|e| !e.is_empty()).collect();
    hits.retain(|h| {
        let p = h.path.to_lowercase();
        !ex.iter().any(|e| p.contains(e.as_str()))
    });
    hits
}

/// Windows Package Manager, if installed (used to offer a one-key Everything install).
pub fn winget_exe() -> Option<String> {
    std::env::var_os("LOCALAPPDATA")
        .map(|l| std::path::PathBuf::from(l).join(r"Microsoft\WindowsApps\winget.exe"))
        .filter(|p| p.exists())
        .map(|p| p.to_string_lossy().into_owned())
}

/// Build output folders: rarely what someone opens from a launcher.
const BUILD_DIRS: [&str; 6] = ["\\target\\debug\\", "\\target\\release\\", "\\obj\\", "\\bin\\debug\\", "\\bin\\release\\", "\\deps\\"];
/// Compiler/debugger artifacts.
const ARTIFACT_EXTS: [&str; 11] = ["d", "pdb", "o", "obj", "rlib", "rmeta", "ilk", "exp", "pch", "tlog", "idb"];

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Orders Everything's hits by relevance for a launcher: how well the *name* matches,
/// then how often it was opened, how recently it changed, and how deep/noisy the path is.
pub fn rank(text: &str, mut hits: Vec<FileHit>, limit: usize) -> Vec<FileHit> {
    let now = now();
    let q = text.trim().to_lowercase();
    let words: Vec<&str> = q.split_whitespace().collect();
    let score = |h: &FileHit| -> f64 {
        let name = h.name.to_lowercase();
        let stem = match name.rfind('.') {
            Some(i) if i > 0 && !h.folder => &name[..i],
            _ => name.as_str(),
        };
        let mut s = if stem == q || name == q {
            100.0
        } else if name.starts_with(&q) {
            60.0
        } else if name_words(&name).any(|w| w.starts_with(&q)) {
            40.0
        } else if words.iter().all(|w| name.contains(w)) {
            25.0
        } else {
            0.0 // matched through the path only
        };
        s += (h.run_count.min(10) as f64) * 6.0;
        if h.modified > 0 {
            let age_days = now.saturating_sub(h.modified) / 86_400;
            s += match age_days {
                0 => 25.0,
                1..=6 => 15.0,
                7..=29 => 8.0,
                30..=364 => 2.0,
                _ => 0.0,
            };
        }
        if h.folder {
            s += 5.0;
        }
        let lower_path = h.path.to_lowercase();
        s -= h.path.matches('\\').count() as f64 * 1.5;
        if lower_path.contains("\\appdata\\") || lower_path.contains("\\programdata\\") {
            s -= 20.0;
        }
        if lower_path.contains("\\.") {
            s -= 10.0; // inside hidden dot-folders (.cache, .cargo, ...)
        }
        if BUILD_DIRS.iter().any(|d| lower_path.contains(d)) {
            s -= 25.0;
        }
        if !h.folder && name.rsplit_once('.').is_some_and(|(_, ext)| ARTIFACT_EXTS.contains(&ext)) {
            s -= 20.0;
        }
        s
    };
    let mut scored: Vec<(f64, FileHit)> = hits.drain(..).map(|h| (score(&h), h)).collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.path.len().cmp(&b.1.path.len())));
    scored.into_iter().take(limit).map(|(_, h)| h).collect()
}

fn name_words(name: &str) -> impl Iterator<Item = &str> {
    name.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty())
}

/// Parent folder for display, with the user profile shortened to `~`.
pub fn display_parent(path: &str) -> String {
    let parent = match path.trim_end_matches('\\').rfind('\\') {
        Some(i) if i <= 2 => &path[..i + 1], // keep the root's backslash: "D:\"
        Some(i) => &path[..i],
        None => return String::new(),
    };
    if let Some(home) = std::env::var_os("USERPROFILE") {
        let home = home.to_string_lossy();
        if parent.len() >= home.len() && parent[..home.len()].eq_ignore_ascii_case(&home) {
            return format!("~{}", &parent[home.len()..]);
        }
    }
    parent.to_owned()
}

pub fn badge(hit: &FileHit) -> String {
    if hit.folder {
        return "Folder".into();
    }
    match hit.name.rfind('.') {
        Some(i) if i > 0 && hit.name.len() - i <= 6 => hit.name[i + 1..].to_uppercase(),
        _ => "File".into(),
    }
}

pub fn is_executable(path: &str) -> bool {
    let lower = path.to_lowercase();
    [".exe", ".bat", ".cmd", ".msi", ".ps1", ".lnk"].iter().any(|e| lower.ends_with(e))
}

/// Default install location of Everything, if present (for the "start Everything" hint).
pub fn everything_exe() -> Option<String> {
    ["ProgramFiles", "ProgramFiles(x86)"]
        .iter()
        .filter_map(std::env::var_os)
        .map(|p| std::path::PathBuf::from(p).join(r"Everything\Everything.exe"))
        .find(|p| p.exists())
        .map(|p| p.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(path: &str, folder: bool, days_old: u64, runs: u32) -> FileHit {
        FileHit {
            name: path.rsplit('\\').next().unwrap().into(),
            path: path.into(),
            folder,
            size: 1,
            modified: now() - days_old * 86_400,
            run_count: runs,
        }
    }

    #[test]
    fn exclusions() {
        let hits = vec![
            hit(r"C:\$Recycle.Bin\S-1-5\Cargo.toml", false, 1, 0),
            hit(r"E:\AI\Smowauncher\Cargo.toml", false, 1, 0),
        ];
        let kept = without_excluded(hits, &[r"\$Recycle.Bin\".into()]);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].path, r"E:\AI\Smowauncher\Cargo.toml");
    }

    #[test]
    fn modes() {
        assert_eq!(parse_query("f report"), Mode::FilesOnly("report"));
        assert_eq!(parse_query("/report "), Mode::FilesOnly("report"));
        assert_eq!(parse_query("  report "), Mode::Mixed("report"));
        assert_eq!(parse_query("firefox"), Mode::Mixed("firefox"));
    }

    #[test]
    fn search_string_excludes() {
        let s = search_string("report", &[r"C:\Windows\".into(), "".into()]);
        assert_eq!(s, r#"report !"C:\Windows\""#);
    }

    #[test]
    fn ranking_prefers_name_matches_and_usage() {
        let hits = vec![
            hit(r"C:\Users\me\AppData\Local\cache\budget-report.tmp", false, 400, 0),
            hit(r"C:\Users\me\Documents\old\reports\notes.txt", false, 400, 0), // path-only match
            hit(r"C:\Users\me\Documents\report.pdf", false, 3, 0),
            hit(r"D:\work\Report 2026.xlsx", false, 1, 0),
        ];
        let ranked = rank("report", hits.clone(), 10);
        assert_eq!(ranked[0].name, "report.pdf");
        assert_eq!(ranked[1].name, "Report 2026.xlsx");
        assert_eq!(ranked.last().unwrap().name, "notes.txt");

        // Frequently opened files climb.
        let mut hits = hits;
        hits[3].run_count = 10;
        assert_eq!(rank("report", hits, 10)[0].name, "Report 2026.xlsx");
    }

    #[test]
    fn display_helpers() {
        let h = hit(r"D:\work\Report 2026.xlsx", false, 1, 0);
        assert_eq!(badge(&h), "XLSX");
        assert_eq!(display_parent(&h.path), r"D:\work");
        assert_eq!(display_parent(r"D:\work"), r"D:\");
        assert_eq!(badge(&hit(r"D:\work", true, 1, 0)), "Folder");
        assert!(is_executable(r"C:\x\setup.EXE"));
    }
}
