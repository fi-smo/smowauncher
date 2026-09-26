//! Snippets: saved texts, pasted from the launcher or expanded when their keyword is typed
//! anywhere (see `platform::input`). Stored in the [snippets] section of config.toml.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct Snippet {
    /// Typed to expand (e.g. ";sig"); also searchable in the launcher.
    pub keyword: String,
    pub name: String,
    pub text: String,
}

pub fn usage_id(s: &Snippet) -> String {
    format!("snip:{}", s.keyword)
}

/// Fills in {date}, {time} and {clipboard}.
pub fn expand(text: &str) -> String {
    if !text.contains('{') {
        return text.to_owned();
    }
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    let mut out = text
        .replace("{date}", &format!("{:04}-{:02}-{:02}", t.wYear, t.wMonth, t.wDay))
        .replace("{time}", &format!("{:02}:{:02}", t.wHour, t.wMinute));
    if out.contains("{clipboard}") {
        out = out.replace("{clipboard}", &crate::platform::clipboard::get_text().unwrap_or_default());
    }
    out
}

/// A keyword that can be expanded while typing: no whitespace, at least 2 characters.
pub fn valid_keyword(k: &str) -> bool {
    k.chars().count() >= 2 && !k.chars().any(char::is_whitespace)
}

/// First line of the text, for result rows and lists.
pub fn preview(text: &str) -> String {
    crate::clip::preview(text)
}

/// Whether `typed` (the most recent characters typed) ends with `keyword` as a whole word:
/// either the keyword starts with a symbol (";sig"), or what comes before it isn't a letter
/// or digit ("xsig" doesn't expand "sig").
pub fn typed_matches(typed: &str, keyword: &str) -> bool {
    if keyword.is_empty() || !typed.ends_with(keyword) {
        return false;
    }
    if !keyword.chars().next().is_some_and(char::is_alphanumeric) {
        return true;
    }
    let before = &typed[..typed.len() - keyword.len()];
    !before.chars().next_back().is_some_and(char::is_alphanumeric)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyword_matching() {
        assert!(typed_matches("hello ;sig", ";sig"));
        assert!(typed_matches("x;sig", ";sig"));
        assert!(typed_matches("sig", "sig"));
        assert!(typed_matches("hi sig", "sig"));
        assert!(!typed_matches("xsig", "sig"));
        assert!(!typed_matches(";si", ";sig"));
        assert!(valid_keyword(";s"));
        assert!(!valid_keyword("a"));
        assert!(!valid_keyword("a b"));
    }

    #[test]
    fn placeholders() {
        assert_eq!(expand("plain"), "plain");
        let d = expand("{date}");
        assert_eq!(d.len(), 10);
        assert_eq!(&d[4..5], "-");
        assert_eq!(expand("{time}").len(), 5);
    }
}
