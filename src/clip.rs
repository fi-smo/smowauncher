//! Clipboard history (text only). Entries are kept newest-first, de-duplicated, capped,
//! and persisted to `%LOCALAPPDATA%\Smowauncher\clipboard.json` (local, not roaming).

use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

/// Larger copies (logs, whole files) are skipped: they're rarely re-pasted from a launcher.
pub const MAX_TEXT_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Entry {
    pub text: String,
    /// Unix seconds.
    pub time: u64,
    /// Process that owned the clipboard ("Code", "firefox"), may be empty.
    pub source: String,
}

#[derive(Default, Serialize, Deserialize)]
pub struct History {
    pub entries: Vec<Entry>,
}

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn path() -> std::path::PathBuf {
    crate::paths::data_dir().join("clipboard.json")
}

impl History {
    pub fn load() -> Self {
        std::fs::read(path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save(&self) {
        if let Ok(json) = serde_json::to_vec(self) {
            let tmp = path().with_extension("json.tmp");
            if std::fs::write(&tmp, json).is_ok() {
                let _ = std::fs::rename(tmp, path());
            }
        }
    }

    /// Adds (or moves to the top) `text`. Returns false if nothing changed.
    pub fn add(&mut self, text: String, source: String, max: usize) -> bool {
        if text.trim().is_empty() || text.len() > MAX_TEXT_BYTES {
            return false;
        }
        if self.entries.first().is_some_and(|e| e.text == text) {
            return false;
        }
        self.entries.retain(|e| e.text != text);
        self.entries.insert(0, Entry { text, time: now(), source });
        self.entries.truncate(max.max(1));
        true
    }

    pub fn remove(&mut self, text: &str) {
        self.entries.retain(|e| e.text != text);
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Entries containing every word of `filter` (case-insensitive), newest first.
    pub fn matching(&self, filter: &str) -> Vec<usize> {
        let words: Vec<String> = filter.split_whitespace().map(str::to_lowercase).collect();
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                let t = e.text.to_lowercase();
                words.iter().all(|w| t.contains(w))
            })
            .map(|(i, _)| i)
            .collect()
    }
}

/// First line, whitespace collapsed, shortened for a result row.
pub fn preview(text: &str) -> String {
    let first = text.trim().lines().next().unwrap_or_default();
    let collapsed: String = first.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() > 120 { format!("{}…", collapsed.chars().take(120).collect::<String>()) } else { collapsed }
}

pub fn relative_time(then: u64, now: u64) -> String {
    let s = now.saturating_sub(then);
    match s {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", s / 60),
        3600..=86_399 => format!("{} h ago", s / 3600),
        86_400..=172_799 => "yesterday".into(),
        _ => format!("{} days ago", s / 86_400),
    }
}

pub fn describe(e: &Entry, now: u64) -> String {
    let lines = e.text.trim().lines().count();
    let size = if lines > 1 { format!("{lines} lines") } else { format!("{} chars", e.text.chars().count()) };
    let mut s = format!("{} · {size}", relative_time(e.time, now));
    if !e.source.is_empty() {
        s.push_str(&format!(" · {}", e.source));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history() {
        let mut h = History::default();
        assert!(h.add("a".into(), "".into(), 3));
        assert!(h.add("b".into(), "".into(), 3));
        assert!(!h.add("b".into(), "".into(), 3)); // same as the newest
        assert!(h.add("a".into(), "".into(), 3)); // moves to the top
        assert_eq!(h.entries.iter().map(|e| e.text.as_str()).collect::<Vec<_>>(), ["a", "b"]);
        h.add("c".into(), "".into(), 3);
        h.add("d".into(), "".into(), 3);
        assert_eq!(h.entries.len(), 3);
        assert!(!h.add("   ".into(), "".into(), 3));
        assert_eq!(h.matching("C"), vec![1]);
        h.remove("c");
        assert!(h.matching("c").is_empty());
    }

    #[test]
    fn display() {
        assert_eq!(preview("  hello   world \nsecond"), "hello world");
        assert_eq!(relative_time(100, 130), "just now");
        assert_eq!(relative_time(0, 7200), "2 h ago");
        let e = Entry { text: "a\nb\nc".into(), time: 0, source: "Code".into() };
        assert_eq!(describe(&e, 300), "5 min ago · 3 lines · Code");
    }
}
