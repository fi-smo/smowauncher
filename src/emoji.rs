//! Emoji picker data and search (":fire" or "emoji fire" in the launcher).

const DATA: &str = include_str!("../res/emoji.txt");

/// Commonly wanted emoji win ties ("heart" → ❤️, not ♥️ "heart suit").
const POPULAR: &[&str] = &[
    "red heart",
    "thumbs up",
    "face with tears of joy",
    "smiling face with smiling eyes",
    "smiling face with heart-eyes",
    "grinning face",
    "winking face",
    "thinking face",
    "loudly crying face",
    "folded hands",
    "clapping hands",
    "party popper",
    "sparkles",
    "check mark button",
    "cross mark",
    "rocket",
    "eyes",
    "hundred points",
    "waving hand",
    "ok hand",
];

pub struct Emoji {
    pub glyph: &'static str,
    pub name: &'static str,
    /// Unicode subgroup ("face-smiling", "animal-mammal"…), searched as extra keywords.
    pub group: &'static str,
}

pub fn all() -> &'static [Emoji] {
    static ALL: std::sync::OnceLock<Vec<Emoji>> = std::sync::OnceLock::new();
    ALL.get_or_init(|| {
        DATA.lines()
            .filter(|l| !l.starts_with('#'))
            .filter_map(|l| {
                let mut parts = l.split('\t');
                Some(Emoji { glyph: parts.next()?, name: parts.next()?, group: parts.next().unwrap_or("") })
            })
            .collect()
    })
}

/// Id used for usage history (recently picked emoji come first).
pub fn usage_id(e: &Emoji) -> String {
    format!("emoji:{}", e.glyph)
}

/// Indices into `all()`, best match first. Every query word must match the start of a word
/// in the name or subgroup ("smil fa" finds "smiling face…").
pub fn search(query: &str, limit: usize) -> Vec<usize> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    if words.is_empty() {
        return Vec::new();
    }
    let q = query.trim().to_lowercase();
    let mut scored: Vec<(usize, u32)> = Vec::new();
    for (i, e) in all().iter().enumerate() {
        let name = e.name.to_lowercase();
        let in_name = |w: &str| name.split(|c: char| !c.is_alphanumeric()).any(|n| n.starts_with(w));
        let in_group = |w: &str| e.group.split('-').any(|g| g.starts_with(w));
        if !words.iter().all(|w| in_name(w) || in_group(w)) {
            continue;
        }
        // Words found in the name itself count more than subgroup ("face-smiling") hits.
        let name_hits = words.iter().filter(|w| in_name(w)).count();
        let mut score = 100 + 40 * name_hits as u32;
        if name == q {
            score += 300;
        } else if name.starts_with(&q) {
            score += 200;
        } else if name.split(|c: char| !c.is_alphanumeric()).any(|n| n == q) {
            score += 150;
        }
        if name_hits == words.len() && POPULAR.contains(&e.name) {
            score += 120;
        }
        // Shorter names are usually the "main" emoji ("fire" before "fire engine").
        score -= (name.len() as u32).min(60);
        scored.push((i, score));
    }
    // Stable sort keeps Unicode's order among equals (faces before objects).
    scored.sort_by(|a, b| b.1.cmp(&a.1));
    scored.into_iter().take(limit).map(|(i, _)| i).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(q: &str) -> Vec<&'static str> {
        search(q, 5).into_iter().map(|i| all()[i].name).collect()
    }

    #[test]
    fn data_loads() {
        assert!(all().len() > 1500);
        assert_eq!(all()[0].glyph, "😀");
        assert_eq!(all()[0].name, "grinning face");
    }

    #[test]
    fn finds_by_word_prefix() {
        assert_eq!(names("fire")[0], "fire");
        assert_eq!(names("thumbs")[0], "thumbs up");
        assert_eq!(names("heart")[0], "red heart");
        // Name matches rank above subgroup matches ("smil" is also in "face-smiling").
        assert!(names("smil fa").iter().all(|n| n.contains("smil")));
        assert!(names("mammal").len() == 5);
        assert!(names("zzzqqq").is_empty());
    }
}
