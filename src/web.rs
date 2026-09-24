//! Web search: keyword prefixes ("yt cats"), a fallback engine for unmatched queries,
//! and opening things that look like URLs ("github.com").

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Engine {
    pub keyword: String,
    pub name: String,
    /// `{q}` is replaced by the URL-encoded query.
    pub url: String,
}

pub fn default_engines() -> Vec<Engine> {
    [
        ("g", "Google", "https://www.google.com/search?q={q}"),
        ("ddg", "DuckDuckGo", "https://duckduckgo.com/?q={q}"),
        ("yt", "YouTube", "https://www.youtube.com/results?search_query={q}"),
        ("gh", "GitHub", "https://github.com/search?q={q}"),
        ("w", "Wikipedia", "https://en.wikipedia.org/w/index.php?search={q}"),
        ("maps", "Google Maps", "https://www.google.com/maps/search/{q}"),
        ("tr", "Google Translate", "https://translate.google.com/?sl=auto&tl=en&text={q}"),
    ]
    .into_iter()
    .map(|(k, n, u)| Engine { keyword: k.into(), name: n.into(), url: u.into() })
    .collect()
}

#[derive(Debug, Clone, PartialEq)]
pub struct WebItem {
    pub title: String,
    pub subtitle: String,
    pub url: String,
    /// Typed with a keyword prefix or a URL: show it first.
    pub explicit: bool,
}

fn encode(q: &str) -> String {
    let mut out = String::with_capacity(q.len() * 3);
    for b in q.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(*b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn search(engine: &Engine, text: &str, explicit: bool) -> WebItem {
    let url = engine.url.replace("{q}", &encode(text));
    let host = url.split("://").nth(1).and_then(|r| r.split('/').next()).unwrap_or_default().to_owned();
    WebItem { title: format!("Search {} for \u{201C}{text}\u{201D}", engine.name), subtitle: host, url, explicit }
}

/// "yt lofi beats" → a YouTube search.
pub fn prefixed(query: &str, engines: &[Engine]) -> Option<WebItem> {
    let (kw, rest) = query.trim_start().split_once(' ')?;
    let rest = rest.trim();
    if rest.is_empty() {
        return None;
    }
    let engine = engines.iter().find(|e| e.keyword.eq_ignore_ascii_case(kw))?;
    Some(search(engine, rest, true))
}

pub fn fallback(query: &str, engines: &[Engine], keyword: &str) -> Option<WebItem> {
    let text = query.trim();
    if text.is_empty() {
        return None;
    }
    let engine = engines.iter().find(|e| e.keyword.eq_ignore_ascii_case(keyword)).or(engines.first())?;
    Some(search(engine, text, false))
}

/// "github.com/foo", "https://x.y", "localhost:3000" → a URL to open.
pub fn url_like(query: &str) -> Option<WebItem> {
    let q = query.trim();
    if q.is_empty() || q.contains(' ') {
        return None;
    }
    let lower = q.to_lowercase();
    let url = if lower.starts_with("http://") || lower.starts_with("https://") {
        q.to_owned()
    } else if lower.starts_with("localhost:") || lower.starts_with("127.0.0.1") {
        format!("http://{q}")
    } else {
        let host = lower.split(['/', '?', '#']).next()?;
        let host = host.split(':').next()?;
        let (name, tld) = host.rsplit_once('.')?;
        let tld_ok = (2..=10).contains(&tld.len()) && tld.chars().all(|c| c.is_ascii_alphabetic());
        let name_ok = !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.');
        // "report.pdf", "notes.txt" are files, not sites.
        const FILE_EXTS: [&str; 16] =
            ["pdf", "txt", "md", "exe", "png", "jpg", "jpeg", "doc", "docx", "xlsx", "zip", "rs", "py", "js", "json", "toml"];
        // ".rs", ".md" are also real TLDs: accept them only with a path ("docs.rs/slint").
        let file_like = FILE_EXTS.contains(&tld) && !lower.contains('/');
        if !tld_ok || !name_ok || file_like {
            return None;
        }
        format!("https://{q}")
    };
    Some(WebItem { title: format!("Open {q}"), subtitle: "Open in browser".into(), url, explicit: true })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixes() {
        let e = default_engines();
        let w = prefixed("yt lofi beats", &e).unwrap();
        assert_eq!(w.url, "https://www.youtube.com/results?search_query=lofi+beats");
        assert!(w.explicit);
        assert!(prefixed("yt", &e).is_none());
        assert!(prefixed("chrome canary", &e).is_none());
        assert_eq!(prefixed("g a&b", &e).unwrap().url, "https://www.google.com/search?q=a%26b");
    }

    #[test]
    fn fallback_uses_configured_engine() {
        let e = default_engines();
        assert!(fallback("rust traits", &e, "ddg").unwrap().url.starts_with("https://duckduckgo.com/?q=rust+traits"));
        assert!(fallback("x", &e, "missing").unwrap().url.contains("google"));
    }

    #[test]
    fn urls() {
        assert_eq!(url_like("github.com").unwrap().url, "https://github.com");
        assert_eq!(url_like("docs.rs/slint").unwrap().url, "https://docs.rs/slint");
        assert_eq!(url_like("localhost:3000").unwrap().url, "http://localhost:3000");
        assert_eq!(url_like("http://x.y/z").unwrap().url, "http://x.y/z");
        for q in ["report.pdf", "notes.txt", "main.rs", "hello", "1.5", "hello world.com", "v1.2"] {
            assert!(url_like(q).is_none(), "{q}");
        }
    }
}
