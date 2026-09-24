//! Exchange rates from Frankfurter (European Central Bank reference rates, free, no key).
//! Cached in `%APPDATA%\Smowauncher\rates.json` so conversion works offline and instantly;
//! refreshed in the background when older than 12 hours.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{SystemTime, UNIX_EPOCH};

const HOST: &str = "api.frankfurter.dev";
const PATH: &str = "/v1/latest";
const MAX_AGE_SECS: u64 = 12 * 60 * 60;

/// Codes Frankfurter publishes, used before the first download completes.
const KNOWN: [&str; 31] = [
    "AUD", "BRL", "CAD", "CHF", "CNY", "CZK", "DKK", "EUR", "GBP", "HKD", "HUF", "IDR", "ILS", "INR", "ISK", "JPY",
    "KRW", "MXN", "MYR", "NOK", "NZD", "PHP", "PLN", "RON", "SEK", "SGD", "THB", "TRY", "USD", "ZAR", "BGN",
];

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Rates {
    /// ECB publication date, e.g. "2026-09-24".
    pub date: String,
    /// Unix seconds when we downloaded them.
    pub fetched: u64,
    /// Units of each currency per 1 EUR.
    pub rates: HashMap<String, f64>,
}

static RATES: RwLock<Option<Rates>> = RwLock::new(None);

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn cache_path() -> std::path::PathBuf {
    crate::paths::config_dir().join("rates.json")
}

/// fend asks for each currency's rate against a common base (here EUR): units per 1 EUR.
pub struct Handler;

impl fend_core::ExchangeRateFnV2 for Handler {
    fn relative_to_base_currency(
        &self,
        currency: &str,
        _options: &fend_core::ExchangeRateFnV2Options,
    ) -> Result<f64, Box<dyn std::error::Error + Send + Sync + 'static>> {
        let guard = RATES.read().map_err(|_| "rates lock poisoned")?;
        let rates = guard.as_ref().ok_or("exchange rates not loaded yet")?;
        match rates.rates.get(&currency.to_uppercase()) {
            Some(per_eur) if *per_eur > 0.0 => Ok(*per_eur),
            _ => Err(format!("no exchange rate for {currency}").into()),
        }
    }
}

/// (symbol, name) for the currencies Frankfurter publishes.
pub fn info(code: &str) -> Option<(&'static str, &'static str)> {
    Some(match code {
        "AUD" => ("A$", "Australian dollar"),
        "BGN" => ("лв", "Bulgarian lev"),
        "BRL" => ("R$", "Brazilian real"),
        "CAD" => ("C$", "Canadian dollar"),
        "CHF" => ("Fr", "Swiss franc"),
        "CNY" => ("¥", "Chinese yuan"),
        "CZK" => ("Kč", "Czech koruna"),
        "DKK" => ("kr", "Danish krone"),
        "EUR" => ("€", "Euro"),
        "GBP" => ("£", "British pound"),
        "HKD" => ("HK$", "Hong Kong dollar"),
        "HUF" => ("Ft", "Hungarian forint"),
        "IDR" => ("Rp", "Indonesian rupiah"),
        "ILS" => ("₪", "Israeli shekel"),
        "INR" => ("₹", "Indian rupee"),
        "ISK" => ("kr", "Icelandic króna"),
        "JPY" => ("¥", "Japanese yen"),
        "KRW" => ("₩", "South Korean won"),
        "MXN" => ("MX$", "Mexican peso"),
        "MYR" => ("RM", "Malaysian ringgit"),
        "NOK" => ("kr", "Norwegian krone"),
        "NZD" => ("NZ$", "New Zealand dollar"),
        "PHP" => ("₱", "Philippine peso"),
        "PLN" => ("zł", "Polish złoty"),
        "RON" => ("lei", "Romanian leu"),
        "SEK" => ("kr", "Swedish krona"),
        "SGD" => ("S$", "Singapore dollar"),
        "THB" => ("฿", "Thai baht"),
        "TRY" => ("₺", "Turkish lira"),
        "USD" => ("$", "US dollar"),
        "ZAR" => ("R", "South African rand"),
        _ => return None,
    })
}

pub fn is_known(code: &str) -> bool {
    match RATES.read().ok().as_ref().and_then(|g| g.as_ref()) {
        Some(r) => r.rates.contains_key(code),
        None => KNOWN.contains(&code),
    }
}

/// Publication date of the loaded rates.
pub fn date() -> Option<String> {
    RATES.read().ok()?.as_ref().map(|r| r.date.clone())
}

fn install(mut rates: Rates) {
    rates.rates.insert("EUR".into(), 1.0);
    if let Ok(mut g) = RATES.write() {
        *g = Some(rates);
    }
}

/// Loads the cache from disk (fast; call at startup).
pub fn load_cache() {
    if let Some(r) = std::fs::read(cache_path()).ok().and_then(|b| serde_json::from_slice::<Rates>(&b).ok()) {
        install(r);
    }
}

/// Downloads fresh rates on a background thread if the cache is missing or stale.
pub fn refresh_if_stale() {
    let fetched = RATES.read().ok().and_then(|g| g.as_ref().map(|r| r.fetched)).unwrap_or(0);
    if now().saturating_sub(fetched) < MAX_AGE_SECS {
        return;
    }
    let _ = std::thread::Builder::new().name("rates".into()).stack_size(256 * 1024).spawn(refresh_blocking);
}

/// Downloads rates now (blocking) and installs + caches them.
pub fn refresh_blocking() {
    #[derive(Deserialize)]
    struct Response {
        date: String,
        rates: HashMap<String, f64>,
    }
    match crate::platform::http::get(HOST, PATH) {
        Ok(body) => match serde_json::from_slice::<Response>(&body) {
            Ok(resp) => {
                let rates = Rates { date: resp.date, fetched: now(), rates: resp.rates };
                log::info!("rates: {} currencies from {}", rates.rates.len(), rates.date);
                if let Ok(json) = serde_json::to_vec(&rates) {
                    let _ = std::fs::write(cache_path(), json);
                }
                install(rates);
            }
            Err(e) => log::warn!("rates: bad response: {e}"),
        },
        Err(e) => log::warn!("rates: download failed: {e}"),
    }
}

#[cfg(test)]
pub fn set_for_tests(per_eur: &[(&str, f64)]) {
    install(Rates {
        date: "2026-01-01".into(),
        fetched: now(),
        rates: per_eur.iter().map(|(c, r)| ((*c).to_owned(), *r)).collect(),
    });
}
