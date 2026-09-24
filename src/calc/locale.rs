//! Number format and currency from the Windows region settings.

use windows::Win32::Globalization::{GetLocaleInfoEx, LOCALE_SDECIMAL, LOCALE_SINTLSYMBOL};
use windows::core::PCWSTR;

fn locale_string(lctype: u32) -> Option<String> {
    let mut buf = [0u16; 16];
    // PCWSTR::null() = LOCALE_NAME_USER_DEFAULT.
    let n = unsafe { GetLocaleInfoEx(PCWSTR::null(), lctype, Some(&mut buf)) };
    (n > 1).then(|| String::from_utf16_lossy(&buf[..n as usize - 1]))
}

pub fn decimal_separator() -> char {
    locale_string(LOCALE_SDECIMAL).and_then(|s| s.chars().next()).unwrap_or('.')
}

/// ISO 4217 code of the user's region, e.g. "PLN".
pub fn currency_code() -> String {
    locale_string(LOCALE_SINTLSYMBOL).filter(|s| s.len() == 3).unwrap_or_else(|| "USD".into())
}
