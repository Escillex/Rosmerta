//! Currency is a display setting, not part of any record. Changing it only
//! changes how hundredths are shown.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Currency {
    pub code: &'static str,
    pub symbol: &'static str,
    /// Decimals shown; amounts are still stored in hundredths.
    pub decimals: u32,
}

const fn c(code: &'static str, symbol: &'static str, decimals: u32) -> Currency {
    Currency { code, symbol, decimals }
}

pub const CURRENCIES: [Currency; 16] = [
    c("USD", "$", 2),
    c("EUR", "€", 2),
    c("GBP", "£", 2),
    c("PHP", "₱", 2),
    c("JPY", "¥", 0),
    c("KRW", "₩", 0),
    c("CNY", "CN¥", 2),
    c("INR", "₹", 2),
    c("SGD", "S$", 2),
    c("AUD", "A$", 2),
    c("CAD", "C$", 2),
    c("IDR", "Rp", 2),
    c("MYR", "RM", 2),
    c("THB", "฿", 2),
    c("VND", "₫", 0),
    c("AED", "AED ", 2),
];

pub const USD: Currency = CURRENCIES[0];
/// Data created before currencies existed is treated as PHP.
pub const LEGACY: Currency = CURRENCIES[3];

impl Currency {
    pub fn from_code(code: &str) -> Option<Currency> {
        CURRENCIES.iter().copied().find(|c| c.code.eq_ignore_ascii_case(code))
    }

    /// First guess from the device region (ISO 3166 code), else USD.
    pub fn for_region(region: &str) -> Currency {
        let code = match region.to_ascii_uppercase().as_str() {
            "US" => "USD",
            "GB" => "GBP",
            "PH" => "PHP",
            "JP" => "JPY",
            "KR" => "KRW",
            "CN" => "CNY",
            "IN" => "INR",
            "SG" => "SGD",
            "AU" => "AUD",
            "CA" => "CAD",
            "ID" => "IDR",
            "MY" => "MYR",
            "TH" => "THB",
            "VN" => "VND",
            "AE" => "AED",
            "AT" | "BE" | "HR" | "CY" | "EE" | "FI" | "FR" | "DE" | "GR" | "IE" | "IT" | "LV" | "LT" | "LU" | "MT" | "NL" | "PT"
            | "SK" | "SI" | "ES" => "EUR",
            _ => "USD",
        };
        Currency::from_code(code).unwrap_or(USD)
    }
}

/// 1,234.56 vs 1.234,56.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NumberStyle {
    pub group: char,
    pub decimal: char,
}

pub const DOT_DECIMAL: NumberStyle = NumberStyle { group: ',', decimal: '.' };
pub const COMMA_DECIMAL: NumberStyle = NumberStyle { group: '.', decimal: ',' };

impl NumberStyle {
    /// From the device language (BCP 47, e.g. "de-DE", "en").
    pub fn for_language(lang: &str) -> NumberStyle {
        let primary = lang.split(['-', '_']).next().unwrap_or("").to_ascii_lowercase();
        let comma = [
            "de", "fr", "es", "it", "nl", "pt", "id", "vi", "tr", "ru", "pl", "cs", "sk", "sl", "hr", "ro", "hu", "da", "nb", "sv", "fi",
            "el", "uk",
        ];
        if comma.contains(&primary.as_str()) { COMMA_DECIMAL } else { DOT_DECIMAL }
    }
}

/// Plain number, grouped, with the currency's decimals: 123456 → "1,234.56".
pub fn format_number(hundredths: i64, cur: Currency, style: NumberStyle) -> String {
    let neg = hundredths < 0;
    let abs = hundredths.unsigned_abs();
    // Round half up to the shown decimals.
    let (whole, frac) = match cur.decimals {
        0 => ((abs + 50) / 100, None),
        _ => (abs / 100, Some(abs % 100)),
    };
    let digits = whole.to_string();
    let mut out = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(style.group);
        }
        out.push(ch);
    }
    if let Some(f) = frac {
        out.push(style.decimal);
        out.push_str(&format!("{f:02}"));
    }
    if neg { format!("−{out}") } else { out }
}

/// With the symbol: −₱1,234.56.
pub fn format(hundredths: i64, cur: Currency, style: NumberStyle) -> String {
    let n = format_number(hundredths.abs(), cur, style);
    if hundredths < 0 { format!("−{}{n}", cur.symbol) } else { format!("{}{n}", cur.symbol) }
}

/// Short form for calendar cells: 950, 1.2k, 34k, 1.5M (whole units, no symbol).
pub fn compact(hundredths: i64) -> String {
    let units = hundredths.abs() as f64 / 100.0;
    let sign = if hundredths < 0 { "−" } else { "" };
    let (v, suffix) = match units {
        u if u >= 1_000_000.0 => (u / 1_000_000.0, "M"),
        u if u >= 1_000.0 => (u / 1_000.0, "k"),
        u => return format!("{sign}{}", u.round()),
    };
    let s = if v < 10.0 { format!("{:.1}", (v * 10.0).floor() / 10.0) } else { format!("{}", v.floor()) };
    format!("{sign}{}{suffix}", s.trim_end_matches(".0"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_with_style_and_decimals() {
        let php = Currency::from_code("php").unwrap();
        assert_eq!(format(123_456, php, DOT_DECIMAL), "₱1,234.56");
        assert_eq!(format(-123_456, php, COMMA_DECIMAL), "−₱1.234,56");
        assert_eq!(format(5, php, DOT_DECIMAL), "₱0.05");
        assert_eq!(format(100_000_000, php, DOT_DECIMAL), "₱1,000,000.00");
        let jpy = Currency::from_code("JPY").unwrap();
        assert_eq!(format(123_450, jpy, DOT_DECIMAL), "¥1,235");
    }

    #[test]
    fn guesses() {
        assert_eq!(Currency::for_region("ph").code, "PHP");
        assert_eq!(Currency::for_region("DE").code, "EUR");
        assert_eq!(Currency::for_region("ZZ").code, "USD");
        assert_eq!(NumberStyle::for_language("de-DE"), COMMA_DECIMAL);
        assert_eq!(NumberStyle::for_language("en_PH"), DOT_DECIMAL);
    }

    #[test]
    fn compact_cells() {
        assert_eq!(compact(95_000), "950");
        assert_eq!(compact(120_000), "1.2k");
        assert_eq!(compact(100_000), "1k");
        assert_eq!(compact(3_456_700), "34k");
        assert_eq!(compact(150_000_000), "1.5M");
    }
}
