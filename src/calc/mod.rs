//! Calculator, unit and currency conversion on top of `fend-core`.
//!
//! fend understands arithmetic, functions and a large unit database ("10 m to ft",
//! "2 l in oz", "30 C to F"). Currencies come from `rates` (Frankfurter/ECB, cached).
//! A cheap pre-filter decides whether a query is worth evaluating at all, so typing an
//! app name never pays for it (and never shows a surprising "3d = 3 days" result).

pub mod locale;
pub mod rates;

use fend_core::{Context, DecimalSeparatorStyle};

#[derive(Debug, Clone, PartialEq)]
pub struct CalcResult {
    /// What was evaluated (after rewrites like "100 usd" → "100 usd to PLN").
    pub expression: String,
    /// Formatted result, e.g. "39.37 ft" or "429.87 PLN".
    pub result: String,
    pub kind: Kind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Math,
    Units,
    Currency,
}

pub struct Calculator {
    ctx: Context,
    /// Windows uses "," as the decimal separator.
    comma_locale: bool,
    default_currency: String,
}

/// Evaluation is cut off after this long (fend can be asked for huge factorials etc.).
struct Deadline(std::time::Instant);

impl fend_core::Interrupt for Deadline {
    fn should_interrupt(&self) -> bool {
        self.0.elapsed() > std::time::Duration::from_millis(50)
    }
}

const CURRENCY_SYMBOLS: [(&str, &str); 6] = [("$", "USD"), ("€", "EUR"), ("£", "GBP"), ("¥", "JPY"), ("zł", "PLN"), ("₹", "INR")];

impl Calculator {
    pub fn new(default_currency: &str) -> Self {
        let mut ctx = Context::new();
        ctx.set_exchange_rate_handler_v2(rates::Handler);
        let comma_locale = locale::decimal_separator() == ',';
        let default_currency = if default_currency.trim().is_empty() {
            locale::currency_code()
        } else {
            default_currency.trim().to_uppercase()
        };
        Self { ctx, comma_locale, default_currency }
    }

    pub fn evaluate(&mut self, query: &str) -> Option<CalcResult> {
        let (expression, kind) = self.prepare(query)?;
        // A comma locale still lets people type "3.5": only switch fend to comma parsing
        // when the input actually uses a comma as decimal separator.
        let comma_input = self.comma_locale && expression.contains(',') && !expression.contains('.');
        self.ctx.set_decimal_separator_style(if comma_input { DecimalSeparatorStyle::Comma } else { DecimalSeparatorStyle::Dot });
        let deadline = Deadline(std::time::Instant::now());
        let out = fend_core::evaluate_with_interrupt(&expression, &mut self.ctx, &deadline).ok()?;
        let mut result = out.get_main_result().trim().to_owned();
        if result.is_empty() || result.contains("approx.") && result.len() > 80 {
            return None;
        }
        // Nothing was computed ("42" → "42").
        if normalize(&result) == normalize(&expression) {
            return None;
        }
        if self.comma_locale && !comma_input {
            result = to_comma_decimals(&result);
        }
        let kind = if mentions_currency(&expression) {
            Kind::Currency
        } else if kind == Kind::Math && is_conversion(&expression) {
            Kind::Units
        } else {
            kind
        };
        Some(CalcResult { expression, result: tidy(&result), kind })
    }

    /// Decides whether `query` looks like a calculation and rewrites shorthands.
    fn prepare(&self, query: &str) -> Option<(String, Kind)> {
        let mut q = query.trim().to_owned();
        if let Some(rest) = q.strip_prefix('=') {
            q = rest.trim().to_owned();
            return (!q.is_empty()).then_some((q, Kind::Math));
        }
        if q.len() < 2 || !q.chars().any(|c| c.is_ascii_digit()) {
            return None;
        }
        // Symbols fend doesn't know as prefixes: "$100" → "100 USD".
        for (sym, code) in CURRENCY_SYMBOLS {
            if let Some(rest) = q.strip_prefix(sym)
                && rest.trim_start().starts_with(|c: char| c.is_ascii_digit())
            {
                let rest = rest.trim_start();
                let end = rest.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == ',')).unwrap_or(rest.len());
                q = format!("{} {code}{}", &rest[..end], &rest[end..]);
            }
        }
        q = rewrite_units(&q.replace('×', "*").replace('÷', "/"));

        // "100 usd" / "100 zł" alone → convert to the default currency.
        if let Some(code) = bare_currency_amount(&q) {
            // Already in the default currency: show it in EUR (or USD for EUR users).
            let target = if !code.eq_ignore_ascii_case(&self.default_currency) {
                self.default_currency.as_str()
            } else if code.eq_ignore_ascii_case("EUR") {
                "USD"
            } else {
                "EUR"
            };
            return Some((format!("{q} to {target}"), Kind::Currency));
        }

        let lower = format!(" {} ", q.to_lowercase());
        let conversion = is_conversion(&q);
        let operator = q.chars().any(|c| "+-*/^%()!".contains(c)) || lower.contains(" mod ");
        let function = ["sqrt", "sin", "cos", "tan", "log", "ln", "abs", "floor", "ceil", "round"]
            .iter()
            .any(|f| lower.contains(&format!("{f}(")) || lower.contains(&format!(" {f} ")));
        // "-5" alone isn't a calculation; "7-zip" (no digit after the operator) isn't either.
        let operand_after_operator = q.char_indices().any(|(i, c)| {
            "+-*/^%".contains(c) && q[i + c.len_utf8()..].trim_start().starts_with(|d: char| d.is_ascii_digit() || d == '(')
        });
        if conversion || function || (operator && (operand_after_operator || q.contains('!') || q.contains('%'))) {
            Some((q, Kind::Math))
        } else {
            None
        }
    }
}

/// "100 usd", "12.5 EUR", "3 zł" → the currency code; None for anything else.
fn bare_currency_amount(q: &str) -> Option<String> {
    let t = q.trim();
    let split = t.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == ',' || c == ' '))?;
    let (num, unit) = t.split_at(split);
    if num.trim().is_empty() || !num.chars().any(|c| c.is_ascii_digit()) {
        return None;
    }
    let unit = unit.trim();
    if let Some((_, code)) = CURRENCY_SYMBOLS.iter().find(|(s, _)| *s == unit) {
        return Some((*code).to_owned());
    }
    (unit.len() == 3 && rates::is_known(&unit.to_uppercase())).then(|| unit.to_uppercase())
}

const VOLUME_UNITS: [&str; 22] = [
    "l", "ml", "cl", "dl", "liter", "liters", "litre", "litres", "gal", "gallon", "gallons", "cup", "cups", "pint",
    "pints", "qt", "quart", "quarts", "tbsp", "tsp", "m3", "cm3",
];

fn temperature(unit: &str) -> Option<&'static str> {
    match unit.to_lowercase().as_str() {
        "f" | "°f" | "degf" | "fahrenheit" => Some("°F"),
        "c" | "°c" | "degc" | "celsius" => Some("°C"),
        "k" | "kelvin" => Some("K"),
        _ => None,
    }
}

/// Rewrites everyday spellings fend doesn't take literally:
/// "72 f to c" → "72 °F to °C", "2 l to oz" → "2 l to floz", "fl oz" → "floz".
fn rewrite_units(q: &str) -> String {
    let mut s = q.to_owned();
    for phrase in ["fluid ounces", "fluid ounce", "fl. oz", "fl oz"] {
        while let Some(i) = s.to_lowercase().find(phrase) {
            s.replace_range(i..i + phrase.len(), "floz");
        }
    }
    let tokens: Vec<&str> = s.split_whitespace().collect();
    let Some(k) = tokens.iter().position(|t| matches!(t.to_lowercase().as_str(), "to" | "in" | "as")) else {
        return s;
    };
    if k == 0 || k + 2 != tokens.len() {
        return s;
    }
    // Left side: "72 f" or "72f".
    let left: String = tokens[..k].join(" ");
    let split = left.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == ',' || c == ' ' || c == '-')).unwrap_or(left.len());
    let (amount, from) = (left[..split].trim(), left[split..].trim());
    let to = tokens[k + 1];
    if amount.is_empty() {
        return s;
    }
    if let (Some(f), Some(t)) = (temperature(from), temperature(to)) {
        return format!("{amount} {f} to {t}");
    }
    let is_oz = |u: &str| matches!(u.to_lowercase().as_str(), "oz" | "ounce" | "ounces");
    let is_volume = |u: &str| VOLUME_UNITS.contains(&u.to_lowercase().as_str()) || u.eq_ignore_ascii_case("floz");
    match (is_oz(from), is_oz(to)) {
        (true, false) if is_volume(to) => format!("{amount} floz to {to}"),
        (false, true) if is_volume(from) => format!("{amount} {from} to floz"),
        _ => s,
    }
}

fn is_conversion(expr: &str) -> bool {
    let lower = format!(" {} ", expr.to_lowercase());
    [" to ", " in ", " as ", "->"].iter().any(|k| lower.contains(k))
}

fn mentions_currency(expr: &str) -> bool {
    expr.split(|c: char| !c.is_ascii_alphabetic())
        .any(|w| w.len() == 3 && w.chars().all(|c| c.is_ascii_alphabetic()) && rates::is_known(&w.to_uppercase()))
}

fn normalize(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_lowercase()
}

/// fend prints "approx. 39.3700787401" for inexact results; round long fractions for display.
fn tidy(result: &str) -> String {
    let r = result.strip_prefix("approx. ").unwrap_or(result);
    let r = r.replace(" floz", " fl oz");
    // Round every number with more than 6 decimals to 6, dropping trailing zeros.
    r.split(' ')
        .map(|word| {
            let sep = if word.contains(',') && !word.contains('.') { ',' } else { '.' };
            match word.split_once(sep) {
                Some((int, frac))
                    if frac.len() > 6
                        && !int.is_empty()
                        && int.trim_start_matches('-').chars().all(|c| c.is_ascii_digit())
                        && frac.chars().all(|c| c.is_ascii_digit()) =>
                {
                    let v: f64 = format!("{int}.{frac}").parse().unwrap_or(0.0);
                    let s = format!("{v:.6}");
                    let s = s.trim_end_matches('0').trim_end_matches('.');
                    if sep == ',' { s.replace('.', ",") } else { s.to_owned() }
                }
                _ => word.to_owned(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// "1234.5 kg" → "1234,5 kg" (only decimal points between digits).
fn to_comma_decimals(s: &str) -> String {
    let b: Vec<char> = s.chars().collect();
    b.iter()
        .enumerate()
        .map(|(i, &c)| {
            let between_digits = i > 0 && i + 1 < b.len() && b[i - 1].is_ascii_digit() && b[i + 1].is_ascii_digit();
            if c == '.' && between_digits { ',' } else { c }
        })
        .collect()
}

/// For currency results: (amount with source code, source code, target code),
/// e.g. "100 eur to pln" → ("100 EUR", "EUR", "PLN").
pub fn currency_sides(r: &CalcResult) -> Option<(String, String, String)> {
    if r.kind != Kind::Currency {
        return None;
    }
    let to = r.result.split_whitespace().last()?.to_uppercase();
    rates::info(&to)?;
    let lower = r.expression.to_lowercase();
    let left = [" to ", " in ", " as "].iter().find_map(|k| lower.find(k)).map(|i| &r.expression[..i])?;
    let from = left.split(|c: char| !c.is_ascii_alphabetic()).find(|w| w.len() == 3 && rates::info(&w.to_uppercase()).is_some())?;
    let from = from.to_uppercase();
    let amount = left.trim().to_uppercase();
    Some((amount, from, to))
}

/// The number part of a result, for "copy number": "39.37 ft" → "39.37".
pub fn number_only(result: &str) -> String {
    result.split_whitespace().next().unwrap_or(result).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn calc() -> Calculator {
        rates::set_for_tests(&[("USD", 1.10), ("PLN", 4.40), ("GBP", 0.85)]);
        let mut ctx = Context::new();
        ctx.set_exchange_rate_handler_v2(rates::Handler);
        Calculator { ctx, comma_locale: false, default_currency: "PLN".into() }
    }

    fn eval(q: &str) -> Option<String> {
        calc().evaluate(q).map(|r| r.result)
    }

    #[test]
    fn math() {
        assert_eq!(eval("2+2").as_deref(), Some("4"));
        assert_eq!(eval("(3+4)*2").as_deref(), Some("14"));
        assert_eq!(eval("2^10").as_deref(), Some("1024"));
        assert_eq!(eval("sqrt(16)").as_deref(), Some("4"));
        assert_eq!(eval("= 7").as_deref(), None); // nothing to compute
        assert_eq!(eval("10/3").as_deref(), Some("3.333333"));
        assert_eq!(eval("15% of 200").as_deref(), Some("30"));
    }

    #[test]
    fn not_math() {
        for q in ["7zip", "7-zip", "3d builder", "notepad++", "2048", "-5", "win10", "1password", "vlc"] {
            assert_eq!(eval(q), None, "{q}");
        }
    }

    #[test]
    fn units() {
        let r = calc().evaluate("10 m to ft").unwrap();
        assert_eq!(r.result, "32.808399 ft");
        assert_eq!(r.kind, Kind::Units);
        assert_eq!(eval("2 l to oz").as_deref(), Some("67.628045 fl oz")); // US fluid ounces
        assert_eq!(eval("2 liters to fluid ounces").as_deref(), Some("67.628045 fl oz"));
        assert_eq!(eval("10 oz to ml").as_deref(), Some("295.735296 ml"));
        assert_eq!(eval("5 oz to g").as_deref(), Some("141.747616 g")); // mass stays mass
        assert_eq!(eval("30 C to F").as_deref(), Some("86 °F"));
        assert_eq!(eval("72f to c").as_deref(), Some("22.222222 °C"));
        assert_eq!(eval("5 km in miles").as_deref(), Some("3.106856 miles"));
        assert_eq!(eval("5 kg to lb").as_deref(), Some("11.023113 lbs"));
    }

    #[test]
    fn currency() {
        let r = calc().evaluate("100 usd").unwrap();
        assert_eq!(r.expression, "100 usd to PLN");
        assert_eq!(r.kind, Kind::Currency);
        assert_eq!(r.result, "400 PLN");
        assert_eq!(eval("$50 to gbp").as_deref(), Some("38.636364 GBP"));
        let r = calc().evaluate("100 usd").unwrap();
        assert_eq!(currency_sides(&r), Some(("100 USD".into(), "USD".into(), "PLN".into())));
        // Already the default currency → EUR instead.
        assert_eq!(calc().evaluate("44 pln").unwrap().expression, "44 pln to EUR");
    }

    #[test]
    fn formatting() {
        assert_eq!(tidy("approx. 39.3700787401 ft"), "39.370079 ft");
        assert_eq!(tidy("3.5"), "3.5");
        assert_eq!(tidy("2.0000000001"), "2");
        assert_eq!(tidy("approx. 295.7352956 ml"), "295.735296 ml");
        assert_eq!(tidy("-1.23456789"), "-1.234568");
        assert_eq!(tidy("1,23456789 kg"), "1,234568 kg");
        assert_eq!(to_comma_decimals("1234.5 kg"), "1234,5 kg");
        assert_eq!(number_only("39.37 ft"), "39.37");
    }

    #[test]
    fn comma_locale() {
        let mut c = calc();
        c.comma_locale = true;
        assert_eq!(c.evaluate("3,5*2").unwrap().result, "7");
        assert_eq!(c.evaluate("1,5+1").unwrap().result, "2,5");
        assert_eq!(c.evaluate("1.5+1").unwrap().result, "2,5");
    }
}
