//! Clipboard history: text and images. Entries are kept newest-first, de-duplicated, capped,
//! and persisted to `%LOCALAPPDATA%\Smowauncher\clipboard.json` (local, not roaming); images
//! are PNG files in `clipboard-images\` next to it, named by a hash of their content.

use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

/// Larger copies (logs, whole files) are skipped: they're rarely re-pasted from a launcher.
pub const MAX_TEXT_BYTES: usize = 256 * 1024;
/// Larger images are skipped (PNG size).
pub const MAX_IMAGE_BYTES: usize = 32 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Entry {
    /// Empty for images.
    pub text: String,
    /// Unix seconds.
    pub time: u64,
    /// Process that owned the clipboard ("Code", "firefox"), may be empty.
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<ClipImage>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClipImage {
    /// File name inside `images_dir()`.
    pub file: String,
    pub width: u32,
    pub height: u32,
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

pub fn images_dir() -> std::path::PathBuf {
    crate::paths::data_dir().join("clipboard-images")
}

pub fn image_path(img: &ClipImage) -> std::path::PathBuf {
    images_dir().join(&img.file)
}

/// Content-based file name, so copying the same image twice keeps one file.
pub fn image_file_name(png: &[u8]) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in png {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}.png")
}

fn forget(e: &Entry) {
    if let Some(img) = &e.image {
        let _ = std::fs::remove_file(image_path(img));
    }
}

impl History {
    pub fn load() -> Self {
        let h: Self = std::fs::read(path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        h.remove_orphan_images();
        h
    }

    pub fn save(&self) {
        if let Ok(json) = serde_json::to_vec(self) {
            let tmp = path().with_extension("json.tmp");
            if std::fs::write(&tmp, json).is_ok() {
                let _ = std::fs::rename(tmp, path());
            }
        }
    }

    /// Image files no entry refers to (left by a crash or an older history file).
    fn remove_orphan_images(&self) {
        let Ok(dir) = std::fs::read_dir(images_dir()) else { return };
        for f in dir.flatten() {
            let name = f.file_name().to_string_lossy().into_owned();
            if !self.entries.iter().any(|e| e.image.as_ref().is_some_and(|i| i.file == name)) {
                let _ = std::fs::remove_file(f.path());
            }
        }
    }

    fn insert(&mut self, entry: Entry, max: usize) {
        self.entries.insert(0, entry);
        for dropped in self.entries.drain(max.max(1).min(self.entries.len())..) {
            forget(&dropped);
        }
    }

    /// Adds (or moves to the top) `text`. Returns false if nothing changed.
    pub fn add(&mut self, text: String, source: String, max: usize) -> bool {
        if text.trim().is_empty() || text.len() > MAX_TEXT_BYTES {
            return false;
        }
        if self.entries.first().is_some_and(|e| e.image.is_none() && e.text == text) {
            return false;
        }
        self.entries.retain(|e| e.image.is_some() || e.text != text);
        self.insert(Entry { text, time: now(), source, image: None }, max);
        true
    }

    /// Adds an image already written to `image_path(&img)`. Returns false if nothing changed.
    pub fn add_image(&mut self, img: ClipImage, source: String, max: usize) -> bool {
        let same = |e: &Entry| e.image.as_ref().is_some_and(|i| i.file == img.file);
        if self.entries.first().is_some_and(same) {
            return false;
        }
        // The same file may be listed further down: move it up without deleting the file.
        self.entries.retain(|e| !same(e));
        self.insert(Entry { text: String::new(), time: now(), source, image: Some(img) }, max);
        true
    }

    pub fn remove(&mut self, index: usize) {
        if index < self.entries.len() {
            forget(&self.entries.remove(index));
        }
    }

    pub fn clear(&mut self) {
        for e in self.entries.drain(..) {
            forget(&e);
        }
    }

    /// Entries containing every word of `filter` (case-insensitive), newest first. Images
    /// match "image", "picture", "screenshot" and their source app.
    pub fn matching(&self, filter: &str) -> Vec<usize> {
        let words: Vec<String> = filter.split_whitespace().map(str::to_lowercase).collect();
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                let t = match &e.image {
                    Some(_) => format!("image picture screenshot {}", e.source.to_lowercase()),
                    None => e.text.to_lowercase(),
                };
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

pub fn title(e: &Entry) -> String {
    match &e.image {
        Some(img) => format!("Image {}×{}", img.width, img.height),
        None => preview(&e.text),
    }
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
    let mut s = relative_time(e.time, now);
    if e.image.is_none() {
        let lines = e.text.trim().lines().count();
        let size = if lines > 1 { format!("{lines} lines") } else { format!("{} chars", e.text.chars().count()) };
        s.push_str(&format!(" · {size}"));
    }
    if !e.source.is_empty() {
        s.push_str(&format!(" · {}", e.source));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(h: &History) -> Vec<&str> {
        h.entries.iter().map(|e| e.text.as_str()).collect()
    }

    #[test]
    fn history() {
        let mut h = History::default();
        assert!(h.add("a".into(), "".into(), 3));
        assert!(h.add("b".into(), "".into(), 3));
        assert!(!h.add("b".into(), "".into(), 3)); // same as the newest
        assert!(h.add("a".into(), "".into(), 3)); // moves to the top
        assert_eq!(text(&h), ["a", "b"]);
        h.add("c".into(), "".into(), 3);
        h.add("d".into(), "".into(), 3);
        assert_eq!(h.entries.len(), 3);
        assert!(!h.add("   ".into(), "".into(), 3));
        assert_eq!(h.matching("C"), vec![1]);
        h.remove(1);
        assert!(h.matching("c").is_empty());
    }

    #[test]
    fn images() {
        let mut h = History::default();
        let img = |f: &str| ClipImage { file: f.into(), width: 10, height: 5 };
        h.add("text".into(), "".into(), 5);
        assert!(h.add_image(img("x.png"), "Snip".into(), 5));
        assert!(!h.add_image(img("x.png"), "Snip".into(), 5));
        h.add("more".into(), "".into(), 5);
        assert!(h.add_image(img("x.png"), "Snip".into(), 5)); // moves up, still one entry
        assert_eq!(h.entries.len(), 3);
        assert_eq!(h.matching("screenshot snip"), vec![0]);
        assert_eq!(title(&h.entries[0]), "Image 10×5");
        assert_eq!(describe(&h.entries[0], h.entries[0].time), "just now · Snip");
    }

    #[test]
    fn display() {
        assert_eq!(preview("  hello   world \nsecond"), "hello world");
        assert_eq!(relative_time(100, 130), "just now");
        assert_eq!(relative_time(0, 7200), "2 h ago");
        let e = Entry { text: "a\nb\nc".into(), time: 0, source: "Code".into(), image: None };
        assert_eq!(describe(&e, 300), "5 min ago · 3 lines · Code");
    }
}
