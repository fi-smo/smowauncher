//! Fuzzy matching + ranking of applications.

use crate::apps::AppEntry;
use crate::usage::Usage;
use nucleo_matcher::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

pub struct Searcher {
    matcher: Matcher,
    buf: Vec<char>,
}

/// Precomputed, lowercase data per app so each keystroke only does matching.
pub struct Prepared {
    name_lower: String,
    acronym: String,
}

pub fn prepare(app: &AppEntry) -> Prepared {
    let name_lower = app.name.to_lowercase();
    let acronym = acronym(&app.name);
    Prepared { name_lower, acronym }
}

/// First letter of each word, plus camel-case humps: "Visual Studio Code" → "vsc", "OneNote" → "on".
fn acronym(name: &str) -> String {
    let mut out = String::new();
    let mut prev: Option<char> = None;
    for c in name.chars() {
        let boundary = match prev {
            None => true,
            Some(p) => !p.is_alphanumeric() || (p.is_lowercase() && c.is_uppercase()),
        };
        if boundary && c.is_alphanumeric() {
            out.extend(c.to_lowercase());
        }
        prev = Some(c);
    }
    out
}

impl Searcher {
    pub fn new() -> Self {
        let mut cfg = Config::DEFAULT;
        cfg.prefer_prefix = true;
        Self { matcher: Matcher::new(cfg), buf: Vec::new() }
    }

    /// Fuzzy score of `query` against arbitrary text (window titles etc.).
    pub fn score_text(&mut self, query: &str, text: &str) -> Option<u32> {
        let pattern = Pattern::new(query.trim(), CaseMatching::Ignore, Normalization::Smart, AtomKind::Fuzzy);
        pattern.score(Utf32Str::new(text, &mut self.buf), &mut self.matcher)
    }

    /// Returns indices into `apps`, best match first.
    pub fn search(
        &mut self,
        query: &str,
        apps: &[AppEntry],
        prepared: &[Prepared],
        usage: &Usage,
        limit: usize,
    ) -> Vec<usize> {
        let q = query.trim();
        if q.is_empty() {
            return Vec::new();
        }
        let q_lower = q.to_lowercase();
        let pattern = Pattern::new(q, CaseMatching::Ignore, Normalization::Smart, AtomKind::Fuzzy);
        let mut scored: Vec<(usize, f64)> = Vec::with_capacity(64);

        for (i, (app, prep)) in apps.iter().zip(prepared).enumerate() {
            let name_score = pattern.score(Utf32Str::new(&app.name, &mut self.buf), &mut self.matcher);
            let kw_score = if app.keywords.is_empty() {
                None
            } else {
                pattern
                    .score(Utf32Str::new(&app.keywords, &mut self.buf), &mut self.matcher)
                    .map(|s| s * 4 / 5)
            };
            let acronym_hit = q_lower.len() >= 2 && prep.acronym.starts_with(&q_lower);
            let Some(base) = name_score.max(kw_score).or(acronym_hit.then_some(0)) else { continue };

            let mut score = base as f64;
            if prep.name_lower == q_lower || app.keywords == q_lower {
                score += 70.0;
            } else if prep.name_lower.split_whitespace().any(|w| w == q_lower) {
                // A whole word of the name ("code" in "Visual Studio Code").
                score += 45.0;
            } else if prep.name_lower.starts_with(&q_lower) {
                score += 35.0;
            } else if prep.name_lower.split_whitespace().any(|w| w.starts_with(&q_lower)) {
                score += 18.0;
            }
            if acronym_hit {
                score += 30.0 + 10.0 * q_lower.len() as f64;
            }
            score += usage.frecency(&app.id) + usage.query_bonus(&app.id, &q_lower);
            scored.push((i, score));
        }

        scored.sort_by(|a, b| {
            b.1.total_cmp(&a.1).then_with(|| apps[a.0].name.len().cmp(&apps[b.0].name.len()))
        });
        scored.truncate(limit);
        scored.into_iter().map(|(i, _)| i).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(name: &str, kw: &str) -> AppEntry {
        AppEntry {
            id: name.into(),
            name: name.into(),
            launch: name.into(),
            path: None,
            keywords: kw.into(),
            packaged: false,
            icon: None,
        }
    }

    fn run(q: &str, apps: &[AppEntry], usage: &Usage) -> Vec<String> {
        let prepared: Vec<_> = apps.iter().map(prepare).collect();
        Searcher::new()
            .search(q, apps, &prepared, usage, 10)
            .into_iter()
            .map(|i| apps[i].name.clone())
            .collect()
    }

    #[test]
    fn acronyms() {
        assert_eq!(acronym("Visual Studio Code"), "vsc");
        assert_eq!(acronym("OneNote"), "on");
        assert_eq!(acronym("7-Zip File Manager"), "7zfm");
    }

    #[test]
    fn ranking() {
        let apps = vec![
            app("Visual Studio Installer", "setup"),
            app("Visual Studio Code", "code"),
            app("Calculator", ""),
            app("Character Map", "charmap"),
            app("Google Chrome", "chrome"),
            app("Codec Tweak Tool", "codectweaktool"),
        ];
        let u = Usage::default();
        assert_eq!(run("vsc", &apps, &u)[0], "Visual Studio Code");
        assert_eq!(run("code", &apps, &u)[0], "Visual Studio Code");
        assert_eq!(run("calc", &apps, &u)[0], "Calculator");
        assert_eq!(run("chrome", &apps, &u)[0], "Google Chrome");
        assert!(run("", &apps, &u).is_empty());

        // Learned binding beats pure text score.
        let mut u = Usage::default();
        u.record("Google Chrome", "c");
        u.record("Google Chrome", "c");
        assert_eq!(run("c", &apps, &u)[0], "Google Chrome");
    }
}
