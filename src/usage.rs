//! Launch history: frecency per item and learned query → item bindings.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_QUERIES: usize = 2000;
const DAY: u64 = 24 * 60 * 60;

#[derive(Default, Serialize, Deserialize)]
pub struct Usage {
    items: HashMap<String, Stat>,
    /// lowercase query → (item id → times chosen)
    queries: HashMap<String, HashMap<String, u32>>,
}

#[derive(Default, Clone, Copy, Serialize, Deserialize)]
struct Stat {
    count: u32,
    last: u64,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl Usage {
    pub fn load() -> Self {
        std::fs::read(Self::path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    fn path() -> std::path::PathBuf {
        crate::paths::config_dir().join("usage.json")
    }

    pub fn save(&self) {
        if let Ok(json) = serde_json::to_vec(self) {
            let tmp = Self::path().with_extension("json.tmp");
            if std::fs::write(&tmp, json).is_ok() {
                let _ = std::fs::rename(tmp, Self::path());
            }
        }
    }

    pub fn record(&mut self, id: &str, query: &str) {
        let stat = self.items.entry(id.to_owned()).or_default();
        stat.count = stat.count.saturating_add(1);
        stat.last = now();

        let q = query.trim().to_lowercase();
        if !q.is_empty() {
            if self.queries.len() >= MAX_QUERIES && !self.queries.contains_key(&q) {
                // Drop an arbitrary old binding; precision isn't important here.
                if let Some(k) = self.queries.keys().next().cloned() {
                    self.queries.remove(&k);
                }
            }
            *self.queries.entry(q).or_default().entry(id.to_owned()).or_default() += 1;
        }
    }

    /// 0..~40 based on how often and how recently the item was used.
    pub fn frecency(&self, id: &str) -> f64 {
        let Some(s) = self.items.get(id) else { return 0.0 };
        let age = now().saturating_sub(s.last);
        let recency = match age {
            a if a < 4 * DAY => 1.0,
            a if a < 14 * DAY => 0.7,
            a if a < 31 * DAY => 0.5,
            a if a < 90 * DAY => 0.3,
            _ => 0.15,
        };
        (s.count as f64).ln_1p() * 12.0 * recency
    }

    /// Bonus for items previously chosen for this exact query (or a longer query starting with it).
    pub fn query_bonus(&self, id: &str, query_lower: &str) -> f64 {
        if query_lower.is_empty() {
            return 0.0;
        }
        let mut bonus: f64 = 0.0;
        if let Some(n) = self.queries.get(query_lower).and_then(|m| m.get(id)) {
            bonus = 50.0 + (*n as f64).min(5.0) * 6.0;
        }
        if bonus == 0.0 {
            for (q, ids) in &self.queries {
                if q.len() > query_lower.len() && q.starts_with(query_lower) && ids.contains_key(id) {
                    bonus = bonus.max(25.0);
                }
            }
        }
        bonus
    }

    /// Most frecent item ids, best first.
    pub fn recent(&self, limit: usize) -> Vec<&str> {
        let mut v: Vec<(&str, f64)> =
            self.items.keys().map(|k| (k.as_str(), self.frecency(k))).collect();
        v.sort_by(|a, b| b.1.total_cmp(&a.1));
        v.into_iter().take(limit).map(|(k, _)| k).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bindings_and_frecency() {
        let mut u = Usage::default();
        u.record("chrome", "ch");
        u.record("chrome", "ch");
        assert!(u.frecency("chrome") > u.frecency("firefox"));
        assert!(u.query_bonus("chrome", "ch") > 50.0);
        assert_eq!(u.query_bonus("chrome", "c"), 25.0);
        assert_eq!(u.query_bonus("firefox", "ch"), 0.0);
        assert_eq!(u.recent(5), vec!["chrome"]);
    }
}
